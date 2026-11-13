//! Direct MPEG-TS/SRT fan-out of the monitor's H.264 access units.
use crate::{BoxError, Counters, VideoMode, encoder::VideoFeed};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app::AppSrc;
use std::{
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    thread::JoinHandle,
};
use tokio::sync::{broadcast::error::RecvError, watch};

pub struct SrtOutput {
    pipeline: gst::Pipeline,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl SrtOutput {
    pub fn spawn(
        mode: VideoMode,
        bind: SocketAddr,
        feed: Arc<VideoFeed>,
        counters: Arc<Counters>,
    ) -> Result<Self, BoxError> {
        gst::init()?;
        let pipeline = gst::parse::launch(
            "appsrc name=input is-live=true format=time block=true max-buffers=2 max-bytes=0 \
             ! h264parse config-interval=-1 ! mpegtsmux alignment=7 \
             ! srtsink name=output wait-for-connection=false sync=false async=false poll-timeout=100 auto-reconnect=true",
        )?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "SRT output is not a pipeline")?;
        let source = pipeline
            .by_name("input")
            .ok_or("missing SRT input")?
            .downcast::<AppSrc>()
            .map_err(|_| "SRT input is not appsrc")?;
        source.set_caps(Some(
            &gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .field("framerate", gst::Fraction::new(mode.fps as i32, 1))
                .build(),
        ));
        let sink = pipeline.by_name("output").ok_or("missing SRT sink")?;
        sink.set_property("uri", format!("srt://{bind}?mode=listener"));
        let caller_feed = feed.clone();
        let callers = Arc::clone(&counters);
        sink.connect("caller-added", false, move |_| {
            caller_feed.request_keyframe();
            callers.srt_callers.fetch_add(1, Ordering::Relaxed);
            log::info!("SRT caller connected");
            None
        });
        sink.connect("caller-removed", false, move |_| {
            counters.srt_callers.fetch_sub(1, Ordering::Relaxed);
            log::info!("SRT caller disconnected");
            None
        });
        let mut frames = feed.subscribe();
        let (stop, mut stopping) = watch::channel(false);
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        pipeline.set_state(gst::State::Playing)?;
        let worker_pipeline = pipeline.clone();
        let thread = match std::thread::Builder::new()
            .name("chr-srt".into())
            .spawn(move || {
                runtime.block_on(async {
                    let mut waiting_for_keyframe = true;
                    loop {
                        let frame = tokio::select! {
                            _ = stopping.changed() => break,
                            frame = frames.recv() => match frame {
                                Ok(frame) => frame,
                                Err(RecvError::Lagged(_)) => {
                                    waiting_for_keyframe = true;
                                    feed.request_keyframe();
                                    continue;
                                }
                                Err(RecvError::Closed) => break,
                            },
                        };
                        if waiting_for_keyframe && !frame.keyframe {
                            continue;
                        }
                        waiting_for_keyframe = false;
                        let mut buffer = gst::Buffer::from_slice(frame.data.clone());
                        let writable = buffer.get_mut().expect("new SRT buffer");
                        let pts = gst::ClockTime::from_nseconds(
                            (u128::from(frame.frame) * 1_000_000_000 / u128::from(mode.fps)) as u64,
                        );
                        writable.set_pts(pts);
                        writable.set_dts(pts);
                        writable.set_duration(gst::ClockTime::SECOND / u64::from(mode.fps));
                        if let Err(error) = source.push_buffer(buffer) {
                            if !*stopping.borrow() {
                                log::error!("SRT output stopped: {error}");
                            }
                            break;
                        }
                        if let Some(message) = worker_pipeline
                            .bus()
                            .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
                            && let gst::MessageView::Error(error) = message.view()
                        {
                            log::error!("SRT output stopped: {}", error.error());
                            break;
                        }
                    }
                });
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(error.into());
            }
        };
        log::info!("program H.264 -> MPEG-TS/SRT listening at srt://{bind}");
        Ok(Self {
            pipeline,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for SrtOutput {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // Flush appsrc before joining: a slow SRT consumer must not pin shutdown.
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encoder::EncodedFrame, gstreamer::Encoder};
    use bytes::Bytes;
    use gstreamer_app::AppSink;
    use std::{
        net::UdpSocket,
        time::{Duration, Instant},
    };

    #[test]
    fn direct_srt_streams_without_webrtc_and_accepts_reconnecting_receivers() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mode = VideoMode {
            width: 64,
            height: 48,
            fps: 30,
        };
        let feed = Arc::new(VideoFeed::new());
        let counters = Arc::new(Counters::default());
        let output = SrtOutput::spawn(mode, address, feed.clone(), Arc::clone(&counters)).unwrap();
        let mut encoder = Encoder::new(mode).unwrap();
        let mut slot = 0;
        let pixels = [128, 80, 128, 80].repeat(64 * 48 / 2);
        for _ in 0..2 {
            let receiver = gst::parse::launch(&format!(
                        "srtsrc uri=\"srt://{address}?mode=caller\" latency=20 poll-timeout=100 \
                         ! tsdemux ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au \
                         ! appsink name=received sync=false max-buffers=4 drop=true wait-on-eos=false"
                    )).unwrap().downcast::<gst::Pipeline>().unwrap();
            let sink = receiver
                .by_name("received")
                .unwrap()
                .downcast::<AppSink>()
                .unwrap();
            receiver.set_state(gst::State::Playing).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut received = None;
            while Instant::now() < deadline {
                let (data, keyframe) = encoder
                    .encode(slot, &pixels, feed.take_keyframe_request())
                    .unwrap();
                feed.publish(EncodedFrame {
                    frame: slot,
                    keyframe,
                    data: Bytes::from(data),
                });
                slot += 1;
                if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(30)) {
                    received = Some(sample);
                    break;
                }
            }
            receiver.set_state(gst::State::Null).unwrap();
            let sample =
                received.expect("direct SRT receiver must get H.264 without a WebRTC peer");
            assert!(counters.srt_callers.load(Ordering::Relaxed) >= 1);
            let caps = sample.caps().unwrap().structure(0).unwrap();
            assert_eq!(caps.get::<i32>("width").unwrap(), 64);
            assert_eq!(caps.get::<i32>("height").unwrap(), 48);
            assert!(!sample.buffer().unwrap().map_readable().unwrap().is_empty());
        }
        let stopping = Instant::now();
        drop(output);
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "SRT shutdown must not await a caller"
        );
    }
}
