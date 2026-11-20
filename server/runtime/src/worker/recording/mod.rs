//! The recording worker (#202): `chr-recorder-cli`, which chr-server starts
//! with `--record <dir>` and supervises like its other workers, records the
//! captured input as `chr-sdi-in` delivered it and the program as it goes to the
//! output.
//!
//! ```text
//! chr-server core                                     chr-recorder-cli
//!   capture tee ─ queue leaky ! appsink ─► Tap "input"   ─unixfd─► input-NNNN.mp4 + .jsonl
//!   program tee ─ queue leaky ! appsink ─► Tap "program" ─unixfd─► program-NNNN.mp4 + .jsonl
//! ```
//!
//! Each [`Tap`] is a standalone `appsrc ! unixfdsink` pipeline that listens on
//! a socket of its own for the whole run, so the worker can come and go and
//! capture sessions can be replaced without either side noticing. A tap with no
//! worker connected does nothing; a full one drops: recording never holds up the
//! capture, the selector or the output. The [`writer`] is the worker's side.

pub mod tap;
pub mod writer;

pub use tap::Tap;

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

use super::supervisor::create_private_dir;
use super::transport::Error;

/// One recorded stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Every captured frame, after `chr-sdi-in`, before the core's branches.
    Input,
    /// Every program slot, before the output.
    Program,
}

impl Stream {
    pub const ALL: [Self; 2] = [Self::Input, Self::Program];

    /// Its name in files, sockets and flags.
    pub fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Program => "program",
        }
    }

    /// The worker's flag naming this stream's socket.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Input => "--input-socket",
            Self::Program => "--program-socket",
        }
    }
}

/// The streams `--record-streams` names: `input`, `program` or both, comma-separated.
pub fn parse_streams(text: &str) -> Result<Vec<Stream>, String> {
    let mut streams = Vec::new();
    for name in text.split(',') {
        let stream = Stream::ALL
            .into_iter()
            .find(|stream| stream.name() == name.trim())
            .ok_or_else(|| {
                format!("the recorded streams are input, program or input,program, not {text:?}")
            })?;
        if !streams.contains(&stream) {
            streams.push(stream);
        }
    }
    Ok(streams)
}

pub struct RecordingConfig {
    /// `chr-recorder-cli`.
    pub executable: PathBuf,
    /// Where this run's segments go: a new session folder ([`session_dir`]).
    pub dir: PathBuf,
    /// The taps' sockets, private to this server.
    pub socket_dir: PathBuf,
    pub streams: Vec<Stream>,
    /// Record at most this many frames per second; `None` records every frame.
    pub fps: Option<chr_common::rate::Rate>,
}

/// The recording of one chr-server run: the taps the core feeds and the
/// supervised worker that records them.
pub struct Recording {
    taps: Vec<(Stream, Arc<Tap>)>,
    stop: Arc<AtomicBool>,
    /// The running worker's process ID; 0 between two.
    pid: Arc<AtomicU32>,
    supervisor: Option<JoinHandle<()>>,
    socket_dir: PathBuf,
}

/// How long a worker that exited waits before it is started again.
const RESTART: Duration = Duration::from_secs(1);
/// How long the worker may take to finish its segments when chr-server stops.
const FINISH: Duration = Duration::from_secs(5);

impl Recording {
    /// Listens on every stream's socket and starts the worker.
    pub fn start(config: RecordingConfig) -> Result<Self, Error> {
        create_private_dir(&config.socket_dir)?;
        std::fs::create_dir_all(&config.dir)
            .map_err(|error| format!("cannot create {}: {error}", config.dir.display()))?;
        let taps = config
            .streams
            .iter()
            .map(|&stream| {
                let path = config.socket_dir.join(format!("{}.sock", stream.name()));
                Ok((stream, Arc::new(Tap::listen(stream, &path)?)))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let mut command = Command::new(&config.executable);
        for (stream, tap) in &taps {
            command.arg(stream.flag()).arg(tap.path());
        }
        if let Some(fps) = config.fps {
            command.arg("--fps").arg(fps.to_string());
        }
        // Its own process group: a signal to chr-server's group (Ctrl-C,
        // `stack.py stop`, a service manager) must not end the recording before
        // chr-server's last frames; chr-server ends it by closing the taps.
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        command
            .arg("--dir")
            .arg(&config.dir)
            .arg("--server-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(std::io::stderr())
            .stderr(Stdio::inherit());
        log::info!(
            "recording {} into {} at {} with {}",
            config
                .streams
                .iter()
                .map(|stream| stream.name())
                .collect::<Vec<_>>()
                .join(" and "),
            config.dir.display(),
            config.fps.map_or_else(
                || "every frame".to_owned(),
                |fps| format!("at most {fps} fps")
            ),
            config.executable.display()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicU32::new(0));
        let supervisor = std::thread::Builder::new()
            .name("chr-recorder-supervisor".into())
            .spawn({
                let (stop, pid) = (Arc::clone(&stop), Arc::clone(&pid));
                move || supervise(command, &stop, &pid)
            })?;
        Ok(Self {
            taps,
            stop,
            pid,
            supervisor: Some(supervisor),
            socket_dir: config.socket_dir,
        })
    }

    /// The tap of `stream`, if it is recorded.
    pub fn tap(&self, stream: Stream) -> Option<Arc<Tap>> {
        self.taps
            .iter()
            .find(|(recorded, _)| *recorded == stream)
            .map(|(_, tap)| Arc::clone(tap))
    }

    /// The running worker's process ID.
    pub fn worker_pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::Acquire)).filter(|pid| *pid != 0)
    }
}

/// Runs the worker until `stop`, starting it again whenever it exits.
fn supervise(mut command: Command, stop: &AtomicBool, pid: &AtomicU32) {
    let mut sessions = 0_u64;
    while !stop.load(Ordering::Acquire) {
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                log::error!("cannot start the recording worker: {error}");
                pause(stop, RESTART);
                continue;
            }
        };
        pid.store(child.id(), Ordering::Release);
        sessions += 1;
        if sessions > 1 {
            log::info!("recording worker started again (session {sessions})");
        }
        loop {
            if stop.load(Ordering::Acquire) {
                finish(child);
                return;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    pid.store(0, Ordering::Release);
                    log::warn!(
                        "the recording worker exited ({status}); starting it again in {RESTART:?}"
                    );
                    pause(stop, RESTART);
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(error) => {
                    log::error!("cannot watch the recording worker: {error}");
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
}

/// Waits for a worker whose streams end to finish its segments; kills it after [`FINISH`].
fn finish(mut child: Child) {
    let deadline = Instant::now() + FINISH;
    while matches!(child.try_wait(), Ok(None)) {
        if Instant::now() >= deadline {
            log::warn!("the recording worker did not finish in {FINISH:?}; killing it");
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.wait();
}

fn pause(stop: &AtomicBool, period: Duration) {
    let until = Instant::now() + period;
    while !stop.load(Ordering::Acquire) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for Recording {
    /// Ends every tap's stream, which asks the worker to finish its segments and
    /// exit, and waits for it.
    fn drop(&mut self) {
        // First, so that the worker's exit is not taken for a crash.
        self.stop.store(true, Ordering::Release);
        for (_, tap) in &self.taps {
            tap.close();
        }
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
        for (stream, tap) in &self.taps {
            log::info!("recording {}: {}", stream.name(), tap.counters().report());
        }
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

/// A new session folder in `root` named after the UTC time now,
/// `20261120T095807Z`; a second run in the same second gets a suffix.
pub fn session_dir(root: &Path) -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let name = utc_name(seconds);
    let mut dir = root.join(&name);
    let mut suffix = 1;
    while dir.exists() {
        suffix += 1;
        dir = root.join(format!("{name}-{suffix}"));
    }
    dir
}

/// `YYYYMMDDTHHMMSSZ` of `seconds` since the Unix epoch.
fn utc_name(seconds: u64) -> String {
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_folders_are_named_after_the_utc_time() {
        assert_eq!(utc_name(0), "19700101T000000Z");
        assert_eq!(utc_name(951_782_400), "20000229T000000Z");
        assert_eq!(utc_name(1_795_168_687), "20261120T095807Z");
    }

    #[test]
    fn recorded_streams_are_input_program_or_both() {
        assert_eq!(parse_streams("input"), Ok(vec![Stream::Input]));
        assert_eq!(
            parse_streams("program, input"),
            Ok(vec![Stream::Program, Stream::Input])
        );
        assert_eq!(parse_streams("input,input"), Ok(vec![Stream::Input]));
        assert!(parse_streams("output").is_err());
        assert!(parse_streams("").is_err());
    }
}
