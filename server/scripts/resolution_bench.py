#!/usr/bin/env python3
"""Measures the tracked live stack at one native resolution, 720p to 8K UHD.

`prepare WxH` writes a synthetic camera into output/resolution-bench/<WxH>p<fps>/: a
testsrc2 clip, its calibrated-video-v1 manifest (a fixed top-down camera), a render
description with one generated image quad, and one clip frame as a PNG. No private
asset is needed.

`bench WxH` times the render worker's path for that description with
`chr-renderer-cli bench` (GPU and wall time per frame).

`run WxH` starts run-tracked.sh (chr-server with both workers, chr-tracking-sim and
chr-monitor with --encoder gstreamer) in its own process group on separate ports and
runtime directory, waits until frames are tracked and rendered, records the SRT
preview while it samples for --seconds, then stops everything it started and
removes the runtime directory. It prints the program counters, CPU and memory per
process, runtime-directory (tmpfs) and GPU memory, and the preview's codec and size.
--scene/--video/--description/--asset-root run a real camera instead of the
synthetic one.

Run from the repository root inside `nix develop .#media`. Another Vulkan process on
the same GPU, such as a running live stack, distorts the numbers (#144: upload
staging buffers share the 256 MiB BAR1 window when Resizable BAR is off).

    python3 server/scripts/resolution_bench.py prepare 3840x2160
    python3 server/scripts/resolution_bench.py bench 3840x2160
    python3 server/scripts/resolution_bench.py run 3840x2160 --seconds 60
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import stack  # noqa: E402  (shares the launcher's NativeSocket client and nixGL wrapper)

ROOT = stack.ROOT
OUTPUT = ROOT / "output" / "resolution-bench"
TICKS = os.sysconf("SC_CLK_TCK")
HEALTH = ["sdi.get_status", "render.get_worker_status", "tracking.get_status", "tracking.get_worker_status"]
# A fixed top-down camera 8 m above the pitch centre: ground XY -> SNC.
TI = [[0.12, 0, 0], [0, 0.12, 0], [0, 0, 1]]
CENTER = [0, 0, -8]


def size(text: str) -> tuple[int, int]:
    width, height = (int(value) for value in text.split("x"))
    return width, height


def run_checked(command: list[str]) -> None:
    subprocess.run(command, check=True, stdin=subprocess.DEVNULL)


def camera_dir(width: int, height: int, fps: int) -> Path:
    return OUTPUT / f"{width}x{height}p{fps}"


def prepare(width: int, height: int, fps: int, frames: int) -> Path:
    out = camera_dir(width, height, fps)
    out.mkdir(parents=True, exist_ok=True)
    clip = out / "clip.mp4"
    if not clip.exists():
        run_checked([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi",
            "-i", f"testsrc2=size={width}x{height}:rate={fps}", "-frames:v", str(frames),
            "-vf", "format=yuv420p,setsar=1,setparams=range=tv:color_primaries=bt709:color_trc=bt709:"
            "colorspace=bt709", "-c:v", "libx264", "-preset", "superfast", "-crf", "28", "-bf", "0",
            "-g", str(fps), "-movflags", "+faststart", str(clip)])
    background = out / "background.png"
    if not background.exists():
        run_checked(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(clip),
                     "-frames:v", "1", "-pix_fmt", "rgba", str(background)])
    quad = out / "quad.png"
    if not quad.exists():
        run_checked(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi",
                     "-i", "testsrc=size=512x256:rate=1", "-frames:v", "1", "-pix_fmt", "rgba", str(quad)])
    s = math.sqrt(width * height)
    # Native pixel -> SNC: SNC = (2 px - extent) / sqrt(width * height).
    warp = [[2 / s, 0, -width / s], [0, 2 / s, -height / s], [0, 0, 1]]
    radial = {"centre_px": [width / 2, height / 2], "scale_px": width / 2,
              "coefficients": [-0.05, 0, 0], "support": None}
    manifest = {"width": width, "height": height, "fps": fps, "ti": TI, "center": CENTER,
                "radial": radial, "frame_warps": [warp] * frames, "model": "none.glb",
                "height_m": 2, "anchor_pixel": [width / 2, height / 2], "pitch_m": [105, 68]}
    (out / "scene.json").write_text(json.dumps(manifest))
    quads = [{"id": f"quad-{i}", "asset": "quad",
              "model": [[4, 0, 0, x], [0, 2, 0, y], [0, 0, 1, -0.05], [0, 0, 0, 1]]}
             for i, (x, y) in enumerate([(-6, -3), (2, -3), (-6, 2), (2, 2)])]
    description = {
        "profile": "chr-render-description-v1",
        "camera": {"width": width, "height": height, "fps": fps, "radial": radial,
                   "reference": {"ti": TI, "center": CENTER, "frame_warps": [warp] * frames}},
        "scene": {"ground": {"pitch_m": [105, 68]},
                  "assets": [{"id": "quad", "kind": "image_quad", "source": {"path": "quad.png"},
                              "sha256": hashlib.sha256(quad.read_bytes()).hexdigest()}],
                  "instances": quads},
    }
    (out / "description.json").write_text(json.dumps(description))
    print(f"prepared {out}: clip.mp4 ({frames} frames at {fps} fps), scene.json, description.json, "
          f"quad.png, background.png")
    return out


def bench(width: int, height: int, fps: int, frames: int, nixgl: bool) -> None:
    out = camera_dir(width, height, fps)
    run_checked(["cargo", "build", "--locked", "--release", "-p", "chr-runtime", "--bin", "chr-renderer-cli"])
    command = [str(target() / "release" / "chr-renderer-cli"), "bench", "--description",
               str(out / "description.json"), "--size", f"{width}x{height}", "--fps", str(fps),
               "--background", str(out / "background.png"), "--frames", str(frames)]
    run_checked((stack.NIXGL if nixgl else []) + command)


def target() -> Path:
    metadata = subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
                              check=True, capture_output=True, text=True).stdout
    return Path(json.loads(metadata)["target_directory"])


def proc_sample(pgid: int) -> dict[int, tuple[str, int, int, int]]:
    """pid -> (name, CPU ticks, RSS kB, shared-memory RSS kB) for the group's real work."""
    sample = {}
    for pid, name in stack.group_processes(pgid):
        base = name.split()[0]
        if base in stack.WRAPPERS or base in ("sleep", "nix", "nixGLNvidia"):
            continue
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            status = dict(line.split(":", 1) for line in Path(f"/proc/{pid}/status").read_text().splitlines()
                          if ":" in line)
        except OSError:
            continue
        kb = lambda key: int(status.get(key, "0 kB").split()[0])  # noqa: E731
        sample[pid] = (name, int(fields[11]) + int(fields[12]), kb("VmRSS"), kb("RssShmem"))
    return sample


def nvidia() -> dict[str, object]:
    def query(*args: str) -> str:
        try:
            return subprocess.run(["nvidia-smi", *args], capture_output=True, text=True, check=True).stdout
        except (OSError, subprocess.CalledProcessError):
            return ""
    apps = {}
    for line in query("--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits").splitlines():
        pid, used = (value.strip() for value in line.split(","))
        apps[int(pid)] = int(used)
    bar1 = None
    memory = query("-q", "-d", "MEMORY", "-i", "0").splitlines()
    for index, line in enumerate(memory):
        if "BAR1 Memory Usage" in line:
            bar1 = next((m.split(":")[1].strip() for m in memory[index + 1:index + 4] if "Used" in m), None)
    gpu = query("--query-gpu=memory.used,utilization.gpu,utilization.encoder", "--format=csv,noheader", "-i", "0")
    return {"apps": apps, "bar1_used": bar1, "gpu": gpu.strip()}


def run(args: argparse.Namespace) -> None:
    width, height = args.size
    out = camera_dir(width, height, args.fps)
    scene = args.scene or str(out / "scene.json")
    video = args.video or str(out / "clip.mp4")
    description = args.description or str(out / "description.json")
    asset_root = args.asset_root or str(out)
    runtime_dir = Path(args.dir)
    port = args.port_base
    srt = f"127.0.0.1:{port + 3}"
    command = [
        "server/scripts/run-tracked.sh", "--scene", scene, "--video", video, "--description", description,
        "--asset-root", asset_root, "--size", f"{width}x{height}", "--srt", srt, "--dir", str(runtime_dir),
        "--ws", f"127.0.0.1:{port}", "--tracking-worker",
        "--monitor-arg", "--ws", "--monitor-arg", f"127.0.0.1:{port + 2}",
        "--monitor-arg", "--encoder", "--monitor-arg", args.encoder,
        "--monitor-arg", "--webrtc-tcp", "--monitor-arg", f"127.0.0.1:{port + 5}",
        *args.server_arg,
    ]
    if args.nixgl:
        command = stack.NIXGL + command
    busy = [p for p in (port, port + 2, port + 5) if stack.listening("127.0.0.1", p)]
    if busy:
        raise SystemExit(f"ports {busy} are in use; pick another --port-base")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    log_path = OUTPUT / f"run-{width}x{height}.log"
    with open(log_path, "wb") as log:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.DEVNULL, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
    pgid = process.pid
    (OUTPUT / "run.pid").write_text(f"{pgid}\n")
    print(f"started process group {pgid}, log {log_path}", flush=True)
    recorder = None
    try:
        socket_path = str(runtime_dir / "render.sock")
        deadline = time.monotonic() + args.start_timeout
        previous, streak = None, 0
        while True:
            if process.poll() is not None:
                raise SystemExit(f"run-tracked.sh exited ({process.returncode}); see {log_path}")
            if time.monotonic() > deadline:
                raise SystemExit(f"not rendering tracked frames after {args.start_timeout:.0f}s; see {log_path}")
            try:
                sample = stack.rpc(socket_path, HEALTH)
                joined = sample["tracking.get_status"]["frames_joined"]
                streak = streak + 1 if previous is not None and joined > previous else 0
                if streak >= 2:
                    break
                previous = joined
            except (OSError, KeyError, TypeError, ValueError):
                pass
            time.sleep(2)
        print(f"frames flowing; settling {args.settle:.0f}s", flush=True)
        time.sleep(args.settle)
        preview = OUTPUT / f"preview-{width}x{height}.ts"
        preview.unlink(missing_ok=True)
        recorder = subprocess.Popen(
            ["gst-launch-1.0", "-e", "-q", "srtsrc", f"uri=srt://{srt}?mode=caller", "latency=120",
             "!", "filesink", f"location={preview}"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
        time.sleep(3)
        first, procs0, gpu0, t0 = stack.rpc(socket_path, HEALTH), proc_sample(pgid), nvidia(), time.monotonic()
        time.sleep(args.seconds)
        second, procs1, gpu1, t1 = stack.rpc(socket_path, HEALTH), proc_sample(pgid), nvidia(), time.monotonic()
        tmpfs = subprocess.run(["du", "-sk", str(runtime_dir)], capture_output=True, text=True).stdout.split()[0]
        recorder.send_signal(signal.SIGINT)
        recorder.wait(timeout=10)
        recorder = None
        report(width, height, args, first, second, t1 - t0, procs0, procs1, gpu0, gpu1, int(tmpfs), preview,
               log_path)
    finally:
        if recorder is not None:
            recorder.kill()
        os.killpg(pgid, signal.SIGTERM)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and stack.group_processes(pgid):
            time.sleep(0.5)
        if stack.group_processes(pgid):
            os.killpg(pgid, signal.SIGKILL)
        process.wait()
        (OUTPUT / "run.pid").unlink(missing_ok=True)
        subprocess.run(["rm", "-rf", str(runtime_dir)])
        print(f"stopped process group {pgid}, removed {runtime_dir}")


def report(width, height, args, first, second, seconds, procs0, procs1, gpu0, gpu1, tmpfs_kb, preview, log_path):
    def delta(method: str, key: str) -> int:
        return second[method][key] - first[method][key]
    sdi, worker = second["sdi.get_status"], second["render.get_worker_status"]
    print(f"\n## {width}x{height}, {seconds:.0f} s ({sdi['video_mode']})")
    print(f"- SDI: {delta('sdi.get_status', 'frames_output') / seconds:.1f} slots/s, "
          f"{delta('sdi.get_status', 'repeated_frames')} repeated, {delta('sdi.get_status', 'dropped_frames')} dropped")
    print(f"- render worker: {worker['state']}, {delta('render.get_worker_status', 'deadline_misses')} deadline "
          f"misses, {delta('render.get_worker_status', 'clean_feed_frames')} clean-feed frames, "
          f"{worker['restarts']} restarts")
    print(f"- tracking: {delta('tracking.get_status', 'frames_joined') / seconds:.1f} joined/s, "
          f"{delta('tracking.get_status', 'frames_without_tracking')} without camera, "
          f"{delta('tracking.get_status', 'frames_dropped')} dropped, "
          f"{delta('tracking.get_status', 'frames_overwritten')} overwritten")
    print("- processes (CPU % of one core over the window; RSS / of it shared memory):")
    for pid, (name, ticks, rss, shmem) in sorted(procs1.items(), key=lambda item: item[1][0]):
        before = procs0.get(pid)
        cpu = (ticks - before[1]) / TICKS / seconds * 100 if before else float("nan")
        gpu = gpu1["apps"].get(pid)
        print(f"  - {name} (pid {pid}): {cpu:.0f} %, RSS {rss / 1024:.0f} MiB / shm {shmem / 1024:.0f} MiB"
              + (f", GPU {gpu} MiB" if gpu is not None else ""))
    print(f"- runtime dir {args.dir} (tmpfs): {tmpfs_kb / 1024:.0f} MiB")
    print(f"- GPU 0 (memory used, GPU %, encoder %): before {gpu0['gpu']}; during {gpu1['gpu']}; "
          f"BAR1 used {gpu1['bar1_used']}")
    probe = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                            "stream=codec_name,profile,width,height,nb_read_packets", "-of", "json", str(preview)],
                           capture_output=True, text=True)
    stream = (json.loads(probe.stdout or "{}").get("streams") or [{}])[0]
    encoder = next((line.split(" with ")[-1] for line in log_path.read_text(errors="replace").splitlines()
                    if "monitor preview" in line), "unknown")
    print(f"- SRT preview: {stream.get('codec_name')} {stream.get('profile')} {stream.get('width')}x"
          f"{stream.get('height')}, {stream.get('nb_read_packets')} frames recorded; encoder {encoder}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=["prepare", "bench", "run"])
    parser.add_argument("size", type=size, help="native camera and program output size, WxH")
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument("--frames", type=int, default=150, help="prepare: clip frames; bench: timed frames")
    parser.add_argument("--seconds", type=float, default=60, help="run: sampling window")
    parser.add_argument("--settle", type=float, default=10, help="run: wait after frames flow")
    parser.add_argument("--start-timeout", type=float, default=900, help="run: includes the release build")
    parser.add_argument("--port-base", type=int, default=28987,
                        help="run: chr-server ws, +2 monitor ws, +3 SRT (UDP), +5..+8 ICE-TCP")
    parser.add_argument("--dir", default=f"{os.environ.get('XDG_RUNTIME_DIR', '/tmp')}/chr-resolution-bench")
    parser.add_argument("--encoder", default="gstreamer", help="run: chr-monitor --encoder")
    parser.add_argument("--server-arg", action="append", default=[],
                        help="run: extra chr-server argument (repeatable), e.g. --server-arg=--render-deadline-ms")
    parser.add_argument("--scene")
    parser.add_argument("--video")
    parser.add_argument("--description")
    parser.add_argument("--asset-root")
    parser.add_argument("--no-nixgl", dest="nixgl", action="store_false")
    args = parser.parse_args()
    width, height = args.size
    if args.command == "prepare":
        prepare(width, height, args.fps, args.frames)
    elif args.command == "bench":
        bench(width, height, args.fps, args.frames, args.nixgl)
    else:
        run(args)


if __name__ == "__main__":
    main()
