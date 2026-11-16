//! The transport between `chr-server` and its workers: GStreamer `unixfdsink` /
//! `unixfdsrc`, one Unix socket per direction.
//!
//! ```text
//! chr-server  appsrc ! unixfdsink socket-path=input.sock   ──►  unixfdsrc ! appsink   worker
//! chr-server  appsink ! unixfdsrc socket-path=output.sock  ◄──  unixfdsink ! appsrc   worker
//! ```
//!
//! The sending side listens (`unixfdsink`) and the receiving side connects
//! (`unixfdsrc`). The server listens on `input.sock` before it starts the worker
//! and passes both paths as `worker --input-socket <path> --output-socket <path>`;
//! the worker listens on `output.sock` and the server connects once it exists.
//! Both sinks hold their buffers until a client connects
//! (`wait-for-connection`), so no message is lost to the startup order.
//!
//! Every buffer carries exactly one [`META`] `GstCustomMeta` whose structure has
//! a string field `json`: one protocol message, a serde JSON object tagged by
//! `type`. A message with pixels carries them as the buffer's single memory,
//! memfd-backed ([`SharedPixels`]), which crosses the socket as a file
//! descriptor without a copy; a control message is a buffer without memory.
//! All buffers on a socket have the caps [`CAPS`]; the protocol messages give
//! pixel formats and extents. Both sides must register [`META`] (see [`init`])
//! before they receive, or `unixfdsrc` rejects the buffer.
//!
//! A receiver releases a buffer (and its memory, in both processes) by dropping
//! it; the sink also releases everything of a client that disconnects.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError, mpsc},
    time::Duration,
};

use gstreamer::{self as gst, prelude::*};
use gstreamer_allocators::ShmAllocator;
use gstreamer_app::{AppSink, AppSrc};
use serde::{Serialize, de::DeserializeOwned};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Name of the custom meta that carries each message.
pub const META: &str = "chr-worker-message";
/// Caps of every buffer on a worker socket.
pub const CAPS: &str = "application/x-chr-worker";
/// The worker's command-line flags for the two socket paths.
pub const INPUT_FLAG: &str = "--input-socket";
pub const OUTPUT_FLAG: &str = "--output-socket";
/// Buffers an `appsrc` may hold before [`Sender::send`] fails instead of growing.
/// The server's credits keep the queue far below this.
const QUEUE: u64 = 16;
/// Released [`SharedPixels`] memories kept for reuse: a fresh memfd costs page
/// faults on every frame (about 3 ms more per 1080p RGBA8 frame than a reused one).
const RECYCLED: usize = 8;

/// Each recycled memory sits alone in a holder buffer: a memory counts the
/// buffers that hold it, so it is writable only if no other buffer (a message
/// being sent, a frame a worker has not released) still holds it.
static RECYCLE: Mutex<Vec<gst::Buffer>> = Mutex::new(Vec::new());

/// Initializes GStreamer and registers [`META`]; idempotent.
pub fn init() -> Result<(), Error> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        gst::init().map_err(|error| error.to_string())?;
        gst::meta::CustomMeta::register(META, &[]);
        Ok(())
    })
    .clone()
    .map_err(Into::into)
}

/// Bytes in memfd-backed GStreamer memory, mapped read-only. Every worker it is
/// sent to receives the same memory as a file descriptor: no copy. Once dropped
/// and no longer in any buffer (every worker released it), the memory is reused.
pub struct SharedPixels {
    memory: gst::Memory,
    mapped: gst::MappedMemory<gst::memory::Readable>,
}

impl SharedPixels {
    /// Allocates `len` bytes and lets `fill` write them: the only copy into the memory.
    pub fn filled<T>(len: usize, fill: impl FnOnce(&mut [u8]) -> T) -> Result<(Self, T), Error> {
        init()?;
        let mut memory = match reusable(len) {
            Some(memory) => memory,
            None => ShmAllocator::get()
                .ok_or("no GStreamer shared-memory allocator")?
                .alloc(len, None)?,
        };
        let filled = fill(
            &mut memory
                .get_mut()
                .expect("new memory is not shared")
                .map_writable()?,
        );
        // `MemoryRef::to_owned` would copy; a cloned `Memory` is another reference.
        let shared = memory.clone();
        let mapped = memory
            .into_mapped_memory_readable()
            .map_err(|_| "cannot map shared pixels")?;
        Ok((
            Self {
                memory: shared,
                mapped,
            },
            filled,
        ))
    }

    pub fn copy_from(bytes: &[u8]) -> Result<Self, Error> {
        Self::filled(bytes.len(), |memory| memory.copy_from_slice(bytes)).map(|(pixels, ())| pixels)
    }

    /// Another reference to the same memory, for a buffer.
    pub fn memory(&self) -> gst::Memory {
        self.memory.clone()
    }
}

/// A recycled memory of `len` bytes that no buffer holds any more.
fn reusable(len: usize) -> Option<gst::Memory> {
    let mut recycled = RECYCLE.lock().unwrap_or_else(PoisonError::into_inner);
    let index = recycled
        .iter()
        .position(|holder| holder.size() == len && holder.is_all_memory_writable())?;
    recycled.swap_remove(index).memory(0)
}

impl Drop for SharedPixels {
    fn drop(&mut self) {
        let mut holder = gst::Buffer::new();
        holder
            .get_mut()
            .expect("new buffer is not shared")
            .append_memory(self.memory.clone());
        let mut recycled = RECYCLE.lock().unwrap_or_else(PoisonError::into_inner);
        if recycled.len() == RECYCLED {
            recycled.remove(0);
        }
        recycled.push(holder);
    }
}

impl std::fmt::Debug for SharedPixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedPixels")
            .field("len", &self.len())
            .finish()
    }
}

impl Deref for SharedPixels {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.mapped.as_slice()
    }
}

/// The sending side of one socket: `appsrc ! unixfdsink`, listening on its path.
pub struct Sender {
    pipeline: gst::Pipeline,
    source: AppSrc,
}

impl Sender {
    /// Listens on `path`, replacing a socket file that a killed process left behind.
    pub fn listen(path: &Path) -> Result<Self, Error> {
        init()?;
        let _ = std::fs::remove_file(path);
        // Never blocks the caller: the queue is checked in `send`.
        let source = AppSrc::builder()
            .caps(&gst::Caps::new_empty_simple(CAPS))
            .format(gst::Format::Time)
            .is_live(true)
            .block(false)
            .max_buffers(QUEUE)
            .max_bytes(0)
            .max_time(gst::ClockTime::ZERO)
            .build();
        let sink = gst::ElementFactory::make("unixfdsink")
            .property("socket-path", path_str(path)?)
            .property("wait-for-connection", true)
            // Payloads must already be fd-backed: fail instead of copying them.
            .property("min-memory-size", -1_i64)
            .property("sync", false)
            .property("async", false)
            .property("enable-last-sample", false)
            .build()?;
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([source.upcast_ref(), &sink])?;
        source.link(&sink)?;
        let sender = Self { pipeline, source };
        sender.pipeline.set_state(gst::State::Playing)?;
        Ok(sender)
    }

    /// Queues one message, with `memory` as its payload, without blocking.
    pub fn send(&self, message: &impl Serialize, memory: Option<gst::Memory>) -> Result<(), Error> {
        if let Some(error) = bus_error(&self.pipeline) {
            return Err(error.into());
        }
        if self.source.current_level_buffers() >= QUEUE {
            return Err("the send queue is full".into());
        }
        let mut buffer = gst::Buffer::new();
        let writable = buffer.get_mut().expect("new buffer is not shared");
        if let Some(memory) = memory {
            writable.append_memory(memory);
        }
        attach(writable, &serde_json::to_string(message)?);
        self.source.push_buffer(buffer)?;
        Ok(())
    }

    /// Ends the stream and waits up to `timeout` until the queued messages were sent.
    pub fn finish(&self, timeout: Duration) {
        if self.source.end_of_stream().is_ok()
            && let Some(bus) = self.pipeline.bus()
        {
            let _ = bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// The receiving side of one socket: `unixfdsrc ! appsink`, connected to its path.
pub struct Receiver {
    pipeline: gst::Pipeline,
    reader: Reader,
}

/// Reads a [`Receiver`]'s messages, for example on another thread.
#[derive(Clone)]
pub struct Reader {
    sink: AppSink,
}

/// One received message and the buffer that carried it; dropping it releases
/// the buffer's memory to the sender.
pub struct Incoming<T> {
    pub message: T,
    pub buffer: gst::Buffer,
}

impl<T> Incoming<T> {
    /// The payload, or an empty slice for a control message.
    pub fn payload(&self) -> Result<gst::BufferMap<'_, gst::buffer::Readable>, Error> {
        Ok(self.buffer.map_readable()?)
    }
}

impl Receiver {
    /// Connects to the sender listening on `path`.
    pub fn connect(path: &Path) -> Result<Self, Error> {
        init()?;
        let source = gst::ElementFactory::make("unixfdsrc")
            .property("socket-path", path_str(path)?)
            .build()?;
        // Unbounded: the server's credits bound what a worker receives, and its
        // requests bound the replies.
        let sink = AppSink::builder()
            .sync(false)
            .enable_last_sample(false)
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([&source, sink.upcast_ref()])?;
        source.link(&sink)?;
        let receiver = Self {
            pipeline,
            reader: Reader { sink },
        };
        receiver.pipeline.set_state(gst::State::Playing)?;
        Ok(receiver)
    }

    pub fn reader(&self) -> Reader {
        self.reader.clone()
    }

    pub fn recv<T: DeserializeOwned>(&self) -> Result<Option<Incoming<T>>, Error> {
        self.reader.recv()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Reader {
    /// Blocks for the next message; `None` once the stream ended, the sender
    /// went away or the receiver was dropped.
    pub fn recv<T: DeserializeOwned>(&self) -> Result<Option<Incoming<T>>, Error> {
        let Ok(sample) = self.sink.pull_sample() else {
            return Ok(None);
        };
        let buffer = sample.buffer_owned().ok_or("a sample without a buffer")?;
        let message = decode(&buffer)?;
        Ok(Some(Incoming { message, buffer }))
    }
}

/// What arrives from a worker on the server's side.
pub enum Event<T> {
    Message(Incoming<T>),
    /// The worker's output ended; the reason is for the log.
    Closed(String),
}

/// `chr-server`'s side of one worker's two sockets in `dir`: it listens on
/// `input.sock` at once and connects to the worker's `output.sock` once that
/// exists ([`Self::connect`]). A thread forwards the worker's messages, so
/// [`Self::try_event`] never blocks.
pub struct Channel<T> {
    sender: Sender,
    input: PathBuf,
    output: PathBuf,
    receiver: Option<Receiver>,
    forward: mpsc::Sender<Event<T>>,
    events: mpsc::Receiver<Event<T>>,
}

impl<T: DeserializeOwned + Send + 'static> Channel<T> {
    pub fn open(dir: &Path) -> Result<Self, Error> {
        let input = dir.join("input.sock");
        let output = dir.join("output.sock");
        // Connecting must wait for this worker's socket, not a dead one's.
        let _ = std::fs::remove_file(&output);
        let (forward, events) = mpsc::channel();
        Ok(Self {
            sender: Sender::listen(&input)?,
            input,
            output,
            receiver: None,
            forward,
            events,
        })
    }

    /// Connects to the worker's output socket once the worker listens on it;
    /// until then, does nothing.
    pub fn connect(&mut self) -> Result<(), Error> {
        if self.receiver.is_some() || !self.output.exists() {
            return Ok(());
        }
        let receiver = match Receiver::connect(&self.output) {
            Ok(receiver) => receiver,
            Err(error) => {
                log::debug!("worker output not connected yet: {error}");
                return Ok(());
            }
        };
        let reader = receiver.reader();
        let forward = self.forward.clone();
        std::thread::Builder::new()
            .name("chr-worker-replies".into())
            .spawn(move || {
                loop {
                    let event = match reader.recv() {
                        Ok(Some(message)) => Event::Message(message),
                        Ok(None) => Event::Closed("the worker exited".into()),
                        Err(error) => Event::Closed(format!("invalid worker output: {error}")),
                    };
                    let closed = matches!(event, Event::Closed(_));
                    if forward.send(event).is_err() || closed {
                        return;
                    }
                }
            })?;
        self.receiver = Some(receiver);
        Ok(())
    }
}

impl<T> Channel<T> {
    /// The worker's command-line arguments naming both sockets.
    pub fn worker_args(&self) -> [&std::ffi::OsStr; 4] {
        [
            INPUT_FLAG.as_ref(),
            self.input.as_os_str(),
            OUTPUT_FLAG.as_ref(),
            self.output.as_os_str(),
        ]
    }

    pub fn send(&self, message: &impl Serialize, memory: Option<gst::Memory>) -> Result<(), Error> {
        self.sender.send(message, memory)
    }

    pub fn is_connected(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn try_event(&self) -> Option<Event<T>> {
        self.events.try_recv().ok()
    }

    /// The next event, or `None` if none arrives within `timeout`.
    pub fn event_timeout(&self, timeout: Duration) -> Option<Event<T>> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Asks the worker to exit: the end of its input stream.
    pub fn finish(&self) {
        self.sender.finish(Duration::ZERO);
    }
}

/// A worker's side: listens on its output socket and connects to its input.
pub fn worker_endpoints(input: &Path, output: &Path) -> Result<(Receiver, Sender), Error> {
    let sender = Sender::listen(output)?;
    Ok((Receiver::connect(input)?, sender))
}

/// Parses `--input-socket <path> --output-socket <path>` from a worker's
/// arguments; returns both paths and the remaining arguments.
pub fn parse_worker_args(
    args: impl IntoIterator<Item = String>,
) -> Result<(PathBuf, PathBuf, Vec<String>), Error> {
    let (mut input, mut output, mut rest) = (None, None, Vec::new());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            INPUT_FLAG => &mut input,
            OUTPUT_FLAG => &mut output,
            _ => {
                rest.push(arg);
                continue;
            }
        };
        *slot = Some(PathBuf::from(
            args.next().ok_or_else(|| format!("{arg} needs a path"))?,
        ));
    }
    match (input, output) {
        (Some(input), Some(output)) => Ok((input, output, rest)),
        _ => Err(format!("a worker needs {INPUT_FLAG} <path> and {OUTPUT_FLAG} <path>").into()),
    }
}

fn attach(buffer: &mut gst::BufferRef, json: &str) {
    gst::meta::CustomMeta::add(buffer, META)
        .expect("the meta is registered in init")
        .mut_structure()
        .set("json", json);
}

/// The message a buffer carries.
pub fn decode<T: DeserializeOwned>(buffer: &gst::BufferRef) -> Result<T, Error> {
    let meta = gst::meta::CustomMeta::from_buffer(buffer, META)
        .map_err(|_| format!("a buffer without the {META} meta"))?;
    let json = meta.structure().get::<&str>("json")?;
    Ok(serde_json::from_str(json)?)
}

fn bus_error(pipeline: &gst::Pipeline) -> Option<String> {
    let message = pipeline.bus()?.pop_filtered(&[gst::MessageType::Error])?;
    match message.view() {
        gst::MessageView::Error(error) => Some(error.error().to_string()),
        _ => None,
    }
}

fn path_str(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| format!("socket path {} is not UTF-8", path.display()).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Message {
        Control { id: u64, text: String },
        Frame { id: u64 },
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chr-transport-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn messages_and_shared_memory_cross_a_socket() {
        let dir = temp_dir("round-trip");
        let path = dir.join("a.sock");
        let sender = Sender::listen(&path).unwrap();
        // Queued before the receiver connects; waits for it instead of being dropped.
        let control = Message::Control {
            id: 1,
            text: "a \"quoted\" ; text, {with} = specials\n".into(),
        };
        sender.send(&control, None).unwrap();
        let receiver = Receiver::connect(&path).unwrap();
        let pixels = SharedPixels::copy_from(&[7; 4096]).unwrap();
        sender
            .send(&Message::Frame { id: 2 }, Some(pixels.memory()))
            .unwrap();
        sender
            .send(&Message::Frame { id: 3 }, Some(pixels.memory()))
            .unwrap();

        let first = receiver.recv::<Message>().unwrap().unwrap();
        assert_eq!(first.message, control);
        assert_eq!(
            first.buffer.n_memory(),
            0,
            "a control message has no memory"
        );
        for id in [2, 3] {
            let frame = receiver.recv::<Message>().unwrap().unwrap();
            assert_eq!(frame.message, Message::Frame { id });
            assert_eq!(*frame.payload().unwrap(), [7; 4096]);
        }
        sender.finish(Duration::from_secs(2));
        assert!(
            receiver.recv::<Message>().unwrap().is_none(),
            "end of stream"
        );
        drop((sender, receiver));
        assert!(!path.exists(), "the sink removes its socket");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The memfd behind fd-backed memory, seen from this process.
    fn inode(memory: &gst::MemoryRef) -> u64 {
        use std::os::unix::fs::MetadataExt;
        let fd = memory
            .downcast_memory_ref::<gstreamer_allocators::FdMemory>()
            .expect("fd-backed memory")
            .fd();
        std::fs::metadata(format!("/proc/self/fd/{fd}"))
            .unwrap()
            .ino()
    }

    #[test]
    fn one_frame_reaches_two_workers_as_the_same_memory() {
        let dir = temp_dir("shared");
        let pixels = SharedPixels::copy_from(&[9; 1 << 16]).unwrap();
        let mut received = Vec::new();
        for name in ["tracking.sock", "render.sock"] {
            let path = dir.join(name);
            let sender = Sender::listen(&path).unwrap();
            let receiver = Receiver::connect(&path).unwrap();
            sender
                .send(&Message::Frame { id: 1 }, Some(pixels.memory()))
                .unwrap();
            let frame = receiver.recv::<Message>().unwrap().unwrap();
            received.push(inode(frame.buffer.peek_memory(0)));
        }
        // A copy by unixfdsink or the sender would be another memfd.
        assert_eq!(received, [inode(&pixels.memory); 2]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn memory_in_a_buffer_is_not_reused() {
        // A size no other test uses: the recycled memories are process-wide.
        const LEN: usize = 777;
        let first = SharedPixels::copy_from(&[1; LEN]).unwrap();
        let mut held = gst::Buffer::new();
        held.get_mut().unwrap().append_memory(first.memory());
        let first_inode = inode(&first.memory);
        drop(first);
        let second = SharedPixels::copy_from(&[2; LEN]).unwrap();
        assert_ne!(inode(&second.memory), first_inode);
        assert_eq!(
            *held.map_readable().unwrap(),
            [1; LEN],
            "untouched while held"
        );
    }

    /// Per-frame cost of the server's one copy and of the transfer, for 1080p RGBA8:
    /// `cargo test --release -p chr-runtime --lib transfer_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn transfer_cost() {
        const FRAMES: u32 = 200;
        let dir = temp_dir("cost");
        let path = dir.join("cost.sock");
        let sender = Sender::listen(&path).unwrap();
        let receiver = Receiver::connect(&path).unwrap();
        let frame = vec![5; 1920 * 1080 * 4];
        let (mut heap, mut copy, mut transfer) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        for id in 0..u64::from(FRAMES) {
            // What a chr-shm ring read into a new Vec, the copy this replaces, costs.
            let start = std::time::Instant::now();
            std::hint::black_box(frame.clone());
            heap += start.elapsed();
            let start = std::time::Instant::now();
            let pixels = SharedPixels::copy_from(&frame).unwrap();
            let copied = std::time::Instant::now();
            sender
                .send(&Message::Frame { id }, Some(pixels.memory()))
                .unwrap();
            let received = receiver.recv::<Message>().unwrap().unwrap();
            assert_eq!(received.payload().unwrap()[0], 5);
            transfer += copied.elapsed();
            copy += copied - start;
        }
        println!(
            "1080p RGBA8: copy into a new Vec {:?}/frame, into a recycled memfd {:?}/frame, \
             send + receive + map {:?}/frame",
            heap / FRAMES,
            copy / FRAMES,
            transfer / FRAMES
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_buffer_without_the_meta_or_with_another_message_is_rejected() {
        init().unwrap();
        let buffer = gst::Buffer::new();
        assert!(decode::<Message>(&buffer).is_err());
        let mut buffer = gst::Buffer::new();
        attach(buffer.get_mut().unwrap(), r#"{"type":"bogus"}"#);
        assert!(decode::<Message>(&buffer).is_err());
    }

    #[test]
    fn worker_args_name_both_sockets() {
        let args = ["--input-socket", "a", "--x", "--output-socket", "b"].map(str::to_owned);
        let (input, output, rest) = parse_worker_args(args).unwrap();
        assert_eq!((input, output), ("a".into(), "b".into()));
        assert_eq!(rest, ["--x"]);
        assert!(parse_worker_args(["--input-socket".to_owned()]).is_err());
        assert!(parse_worker_args(["--input-socket", "a"].map(str::to_owned)).is_err());
    }
}
