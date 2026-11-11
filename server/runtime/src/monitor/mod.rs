//! Program monitor: turns the SDI output back into a WebRTC stream for clients.
//!
//! The monitor consumes the slots the SDI port transmitted, converts them from UYVY 4:2:2
//! to planar 4:2:0, encodes H.264, and publishes the frames to every WebRTC peer. It runs
//! on its own thread and only receives slots the port could spare, so it can never delay
//! the program output.
//!
//! Encoding is software (openh264) on CPU-side frames as a development shortcut. The
//! intended path hands the GPU's SDI frame to a hardware encoder such as NVENC.

pub mod encoder;
pub mod webrtc;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread::JoinHandle,
};

use bytes::Bytes;
use openh264::{encoder::VuiConfig, formats::YUVSlices};

use crate::sdi::{POLL, SdiSlot, VideoMode};
use encoder::{EncodedFrame, H264Encoder, VideoFeed};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    /// Encodes slots from `tap` into `feed` while anyone watches.
    pub fn spawn(
        mode: VideoMode,
        tap: Receiver<SdiSlot>,
        feed: Arc<VideoFeed>,
    ) -> Result<Self, BoxError> {
        if !mode.width.is_multiple_of(2) || !mode.height.is_multiple_of(2) {
            return Err("4:2:0 encoding needs an even width and height".into());
        }
        // The SDI frames are BT.709 limited range; signal that to decoders.
        let encoder = H264Encoder::new(mode.fps, VuiConfig::bt709())?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("chr-monitor".into())
                .spawn(move || run(mode, &tap, &feed, encoder, &stop))?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mode: VideoMode,
    tap: &Receiver<SdiSlot>,
    feed: &VideoFeed,
    mut encoder: H264Encoder,
    stop: &AtomicBool,
) {
    let (width, height) = (mode.width as usize, mode.height as usize);
    let mut planes = I420::new(width, height);
    while !stop.load(Ordering::Relaxed) {
        let slot = match tap.recv_timeout(POLL) {
            Ok(slot) => slot,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if !feed.has_viewers() {
            continue;
        }
        planes.fill_from_uyvy(&slot.uyvy);
        let yuv = YUVSlices::new(
            (&planes.y, &planes.u, &planes.v),
            (width, height),
            (width, width / 2, width / 2),
        );
        match encoder.encode_yuv(&yuv, feed.take_keyframe_request()) {
            // RTP time follows output slots, the cadence clients actually receive.
            Ok(Some((data, keyframe))) => feed.publish(EncodedFrame {
                frame: slot.slot,
                keyframe,
                data: Bytes::from(data),
            }),
            Ok(None) => {}
            Err(error) => {
                log::error!("monitor encoder failed: {error}");
                return;
            }
        }
    }
}

/// Planar 4:2:0, as the encoder takes it.
struct I420 {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420 {
    fn new(width: usize, height: usize) -> Self {
        let chroma = (width / 2) * (height / 2);
        Self {
            width,
            height,
            y: vec![0; width * height],
            u: vec![0; chroma],
            v: vec![0; chroma],
        }
    }

    /// Unpacks UYVY 4:2:2 and halves chroma vertically by averaging row pairs.
    fn fill_from_uyvy(&mut self, uyvy: &[u8]) {
        let row_bytes = self.width * 2;
        let half_width = self.width / 2;
        for (row_index, row) in uyvy.chunks_exact(row_bytes).take(self.height).enumerate() {
            let luma = &mut self.y[row_index * self.width..][..self.width];
            for (pair, group) in row.as_chunks::<4>().0.iter().enumerate() {
                luma[pair * 2] = group[1];
                luma[pair * 2 + 1] = group[3];
            }
        }
        for chroma_row in 0..self.height / 2 {
            let top = &uyvy[2 * chroma_row * row_bytes..][..row_bytes];
            let bottom = &uyvy[(2 * chroma_row + 1) * row_bytes..][..row_bytes];
            let base = chroma_row * half_width;
            for pair in 0..half_width {
                let (t, b) = (
                    &top[pair * 4..pair * 4 + 4],
                    &bottom[pair * 4..pair * 4 + 4],
                );
                self.u[base + pair] = average(t[0], b[0]);
                self.v[base + pair] = average(t[2], b[2]);
            }
        }
    }
}

fn average(a: u8, b: u8) -> u8 {
    ((u16::from(a) + u16::from(b)).div_ceil(2)) as u8
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn unpacks_uyvy_into_i420() {
        // 4x2 image: two pixel pairs per row, rows differ in chroma.
        let uyvy = [
            10, 1, 20, 2, 30, 3, 40, 4, // row 0: (Cb, Y0, Cr, Y1) x 2
            12, 5, 22, 6, 33, 7, 44, 8, // row 1
        ];
        let mut planes = I420::new(4, 2);
        planes.fill_from_uyvy(&uyvy);
        assert_eq!(planes.y, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(planes.u, [11, 32]);
        assert_eq!(planes.v, [21, 42]);
    }

    #[test]
    fn encodes_transmitted_slots_with_their_slot_index() {
        let mode = VideoMode {
            width: 64,
            height: 32,
            fps: 50,
        };
        let feed = Arc::new(VideoFeed::new());
        let mut viewer = feed.subscribe();
        let (tap_tx, tap_rx) = mpsc::sync_channel(4);
        let monitor = Monitor::spawn(mode, tap_rx, Arc::clone(&feed)).unwrap();
        tap_tx
            .send(SdiSlot {
                slot: 7,
                uyvy: Arc::new([128, 100, 128, 100].repeat(64 * 32 / 2)),
            })
            .unwrap();
        let first = viewer.blocking_recv().expect("an encoded slot");
        assert_eq!(first.frame, 7, "RTP timing follows output slots");
        assert!(first.keyframe);
        drop(monitor);
    }
}
