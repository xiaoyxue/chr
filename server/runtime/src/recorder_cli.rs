//! `chr-recorder-cli`: chr-server's recording worker (#202). chr-server starts
//! it with `--record <dir>` and supervises it; it may also be run by hand
//! against a running chr-server's recording sockets.
//!
//! Each recorded stream (the captured input, the program) arrives on a unixfd
//! socket of chr-server's and is written as H.264 MP4 segments, each with a
//! JSONL sidecar of its frames ([`chr_runtime::worker::recording::writer`]).

use std::{
    error::Error,
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use chr_common::rate::Rate;
use chr_runtime::worker::recording::{Stream, writer};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = "\
Usage: chr-recorder-cli [--input-socket <path>] [--program-socket <path>] --dir <dir>
                        [--fps <rate>] [--server-pid <pid>]

Records chr-server's captured input and/or its program from the recording sockets
of chr-server --record (one per stream) into <dir>: <stream>-NNNN.mp4 (H.264)
and <stream>-NNNN.jsonl (a header line, then one line per frame), a new segment for
each format. It exits when chr-server ends the streams, on Ctrl-C or SIGTERM, and once
its parent is no longer --server-pid (chr-server starts it in a process group of its
own, so signals to chr-server's group do not reach it).

Options:
  --input-socket <path>   Record the captured input from this socket
  --program-socket <path> Record the program from this socket
  --dir <dir>             Where the segments go (created if missing)
  --fps <rate>            Record at most this many frames per second, on a cadence of
                          the frames' timestamps: 10, 25, 30000/1001, 29.97, ...
                          [default: every frame]
  --server-pid <pid>      Exit once the parent process is no longer this one
  -h, --help              Show this help";

struct Args {
    sockets: Vec<(Stream, PathBuf)>,
    dir: PathBuf,
    fps: Option<Rate>,
    server_pid: Option<u32>,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let (mut sockets, mut dir, mut fps, mut server_pid) = (Vec::new(), None, None, None);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--input-socket" => sockets.push((Stream::Input, value()?.into())),
            "--program-socket" => sockets.push((Stream::Program, value()?.into())),
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--fps" => fps = Some(value()?.parse()?),
            "--server-pid" => server_pid = Some(value()?.parse()?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}").into()),
        }
    }
    if sockets.is_empty() {
        return Err("give --input-socket, --program-socket or both".into());
    }
    Ok(Some(Args {
        sockets,
        dir: dir.ok_or("--dir is required")?,
        fps,
        server_pid,
    }))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Records every stream on its own thread; when one ends, all stop.
fn run(args: Args) -> Result<()> {
    std::fs::create_dir_all(&args.dir)?;
    let stop = Arc::new(AtomicBool::new(false));
    on_signal(Arc::clone(&stop))?;
    let recorders = args
        .sockets
        .into_iter()
        .map(|(stream, socket)| {
            let (stop, dir, fps, server_pid) = (
                Arc::clone(&stop),
                args.dir.clone(),
                args.fps,
                args.server_pid,
            );
            std::thread::Builder::new()
                .name(format!("chr-record-{}", stream.name()))
                .spawn(move || {
                    let alive =
                        || server_pid.is_none_or(|pid| std::os::unix::process::parent_id() == pid);
                    let result = writer::record(stream, &socket, &dir, fps, &stop, alive);
                    stop.store(true, Ordering::Release);
                    result
                })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut failed = None;
    for recorder in recorders {
        if let Err(error) = recorder.join().map_err(|_| "a recorder panicked")? {
            failed = Some(error);
        }
    }
    failed.map_or(Ok(()), Err)
}

/// Sets `stop` on Ctrl-C or SIGTERM, so that the segments are finished. chr-server
/// starts the worker in a process group of its own, so neither reaches it
/// through chr-server's group: chr-server ends the streams as it stops.
fn on_signal(stop: Arc<AtomicBool>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name("chr-record-signals".into())
        .spawn(move || {
            runtime.block_on(async {
                let Ok(mut terminate) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                else {
                    return;
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            });
            log::info!("stopping: finishing the segments");
            stop.store(true, Ordering::Release);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn the_worker_needs_a_socket_and_a_folder() {
        let args = parse(&["--program-socket", "/p.sock", "--dir", "/rec"])
            .unwrap()
            .unwrap();
        assert_eq!(args.sockets, [(Stream::Program, PathBuf::from("/p.sock"))]);
        assert!(parse(&["--dir", "/rec"]).is_err());
        assert!(parse(&["--input-socket", "/i.sock"]).is_err());
        assert!(
            parse(&[
                "--input-socket",
                "/i.sock",
                "--dir",
                "/r",
                "--server-pid",
                "x"
            ])
            .is_err()
        );
        let args = parse(&["--input-socket", "/i.sock", "--dir", "/r", "--fps", "29.97"])
            .unwrap()
            .unwrap();
        assert_eq!(args.fps, Some(Rate::new(30000, 1001).unwrap()));
        assert!(parse(&["--input-socket", "/i.sock", "--dir", "/r", "--fps", "0"]).is_err());
    }
}
