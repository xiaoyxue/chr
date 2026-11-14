#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Starts, stops and reports the local development stack with one command.

The stack is the tracked-camera simulation (chr-tracking-sim, chr-server and
chr-monitor through run-tracked.sh, which restarts whichever exits), the
pitch-studio client, the program monitor page and an optional SRT-to-TCP relay for
viewers behind an SSH port forward. Machine-specific paths live in a local TOML
file (default output/stack/stack.toml; see stack.example.toml).

Each component runs detached in its own process group, logs to
output/stack/<name>.log and is recorded in output/stack/<name>.pid, so `stop`
only signals processes this script started.

    uv run server/scripts/stack.py start|stop|restart|status [--config FILE]
"""

from __future__ import annotations

import argparse
import getpass
import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / "output" / "stack"
NIXGL = ["env", "NIXPKGS_ALLOW_UNFREE=1", "nix", "run", "--impure", "github:nix-community/nixGL#nixGLNvidia", "--"]
STOP_TIMEOUT_S = 15


@dataclass
class Config:
    scene: str
    video: str
    description: str | None = None
    asset_root: str | None = None
    size: str = "1280x720"
    dir: str = field(default_factory=lambda: f"{os.environ.get('XDG_RUNTIME_DIR', '/tmp')}/chr-stack")
    nixgl: bool = True
    srt: str = "127.0.0.1:9001"
    server_ws: str = "127.0.0.1:8787"
    monitor_ws: str = "127.0.0.1:8789"
    webrtc_tcp: str = "127.0.0.1:8790"
    srt_relay_port: int = 0
    pitch_studio_port: int = 4173
    monitor_page_port: int = 5173
    # "worker": chr-server's tracking worker supplies cameras; "external": the simulator does.
    tracking: str = "worker"


def load_config(path: Path) -> Config:
    if not path.exists():
        raise SystemExit(f"{path} not found: copy server/scripts/stack.example.toml there and edit it")
    values = tomllib.loads(path.read_text())
    known = Config.__dataclass_fields__
    unknown = sorted(set(values) - set(known))
    if unknown:
        raise SystemExit(f"{path}: unknown keys {', '.join(unknown)}")
    missing = [key for key in ("scene", "video") if key not in values]
    if missing:
        raise SystemExit(f"{path}: missing {', '.join(missing)}")
    if values.get("tracking", "worker") not in ("worker", "external"):
        raise SystemExit(f"{path}: tracking must be \"worker\" or \"external\"")
    return Config(**values)


@dataclass
class Component:
    name: str
    command: list[str]
    env: dict[str, str]
    # Endpoints that answer once the component is ready, as (label, host, port).
    ready: list[tuple[str, str, int]]


def endpoint(address: str) -> tuple[str, int]:
    host, _, port = address.rpartition(":")
    return host, int(port)


def components(config: Config) -> list[Component]:
    render = [
        "server/scripts/run-tracked.sh", "--scene", config.scene, "--video", config.video,
        "--size", config.size, "--srt", config.srt, "--dir", config.dir, "--ws", config.server_ws,
        "--monitor-arg", "--ws", "--monitor-arg", config.monitor_ws,
        "--monitor-arg", "--encoder", "--monitor-arg", "gstreamer",
        "--monitor-arg", "--webrtc-tcp", "--monitor-arg", config.webrtc_tcp,
    ]
    if config.description:
        render += ["--description", config.description]
    if config.asset_root:
        render += ["--asset-root", config.asset_root]
    if config.tracking == "worker":
        render += ["--tracking-worker"]
    if config.nixgl:
        render = NIXGL + render
    stack = [
        Component("render", ["nix", "develop", ".#media", "--command", *render], {},
                  [("chr-server", *endpoint(config.server_ws)), ("chr-monitor", *endpoint(config.monitor_ws))]),
    ]
    if config.srt_relay_port:
        host, port = endpoint(config.srt)
        caller = f"srt://{'127.0.0.1' if host in ('0.0.0.0', '') else host}:{port}?mode=caller&latency=120000"
        stack.append(Component(
            "srt-relay",
            ["nix", "develop", ".#media", "--command", "python3", "server/scripts/stack.py", "relay", caller,
             str(config.srt_relay_port)],
            {}, [("SRT relay", "127.0.0.1", config.srt_relay_port)]))
    for name, directory, port, env in (
        ("pitch-studio", "client/pitch-studio", config.pitch_studio_port, {"CHR_RENDER_URL": f"ws://{config.server_ws}"}),
        ("monitor-page", "client/example/render-harness", config.monitor_page_port, {}),
    ):
        if port:
            script = (f"cd {directory} && {{ [ -d node_modules ] || npm ci --no-audit --no-fund; }} && "
                      f"exec npx vite --host 127.0.0.1 --port {port} --strictPort")
            stack.append(Component(name, ["nix", "develop", ".#default", "--command", "bash", "-c", script], env,
                                   [(name, "127.0.0.1", port)]))
    return stack


def forwarded_ports(config: Config) -> list[int]:
    """Loopback TCP ports a remote browser or player needs through an SSH tunnel."""
    webrtc = endpoint(config.webrtc_tcp)[1]
    ports = [config.pitch_studio_port, config.monitor_page_port, endpoint(config.server_ws)[1],
             endpoint(config.monitor_ws)[1], *range(webrtc, webrtc + 4), config.srt_relay_port]
    return [port for port in ports if port]


def roles(config: Config, external_tracking: bool) -> dict[str, str]:
    """What each configured socket is for, keyed like `listening_sockets` values."""
    host, port = endpoint(config.webrtc_tcp)
    known = {
        f"tcp {config.server_ws}": "chr-server control JSON-RPC WebSocket (description.load, status)",
        f"unix {config.dir}/render.sock": "chr-server NativeSocket: local control"
                                         + (", tracking.frame from chr-tracking-sim" if external_tracking else ""),
        f"tcp {config.monitor_ws}": "chr-monitor WebSocket: status and WebRTC signalling",
        f"udp {config.srt}": "chr-monitor SRT listener: the program for VLC / ffplay",
        **{f"tcp {host}:{port + i}": "chr-monitor WebRTC ICE-TCP, one per open stream" for i in range(4)},
        f"tcp 127.0.0.1:{config.srt_relay_port}": "SRT copied to TCP for players behind SSH",
        f"tcp 127.0.0.1:{config.pitch_studio_port}": f"pitch-studio page; /chr-render -> ws://{config.server_ws}",
        f"tcp 127.0.0.1:{config.monitor_page_port}": "read-only program monitor page",
    }
    return known


# Processes that hold no socket, and what they do instead.
PORTLESS = {
    "chr-tracking-sim": "camera: frames into the input ring (shared memory)",
    "ffmpeg": "decodes the clip in a loop for chr-tracking-sim",
    "chr-renderer-cli worker": "GPU renderer; stdin/stdout and shared memory with chr-server",
    "chr-tracking-sim-cli worker": "tracking worker (replays recorded cameras); frames from chr-server's"
                                   " shared-memory ring, cameras back over stdin/stdout",
}
# Launchers and restart loops that only start and watch the processes doing the work.
WRAPPERS = ("bash", "npm", "esbuild")


def socket_address(field: str) -> str:
    """`/proc/net/{tcp,udp}{,6}` local address, e.g. `0100007F:2253` -> `127.0.0.1:8787`."""
    host, port = field.split(":")
    raw = bytes.fromhex(host)
    if len(raw) == 4:
        ip = socket.inet_ntop(socket.AF_INET, raw[::-1])
    else:
        ip = socket.inet_ntop(socket.AF_INET6, b"".join(raw[i:i + 4][::-1] for i in range(0, 16, 4)))
    return f"{ip}:{int(port, 16)}"


def listening_sockets() -> dict[int, str]:
    """Listening TCP, bound UDP and listening Unix sockets by inode, e.g. `tcp 127.0.0.1:8787`."""
    found = {}
    # TCP LISTEN is state 0A; an unconnected UDP socket reports 07.
    for table, state in (("tcp", "0A"), ("tcp6", "0A"), ("udp", "07"), ("udp6", "07")):
        try:
            lines = Path(f"/proc/net/{table}").read_text().splitlines()[1:]
        except OSError:
            continue
        for line in lines:
            fields = line.split()
            if fields[3] == state:
                found[int(fields[9])] = f"{table[:3]} {socket_address(fields[1])}"
    for line in Path("/proc/net/unix").read_text().splitlines()[1:]:
        fields = line.split()
        if len(fields) == 8 and int(fields[3], 16) & 0x10000:  # __SO_ACCEPTCON: listening
            found[int(fields[6])] = f"unix {fields[7]}"
    return found


def process_sockets(pid: int, sockets: dict[int, str]) -> list[str]:
    found = set()
    try:
        fds = os.listdir(f"/proc/{pid}/fd")
    except OSError:
        return []
    for fd in fds:
        try:
            target = os.readlink(f"/proc/{pid}/fd/{fd}")
        except OSError:
            continue
        if target.startswith("socket:[") and (inode := int(target[8:-1])) in sockets:
            found.add(sockets[inode])
    return sorted(found)


def describe(argv: list[str]) -> str:
    """A short name from a command line: the program and its leading non-option words."""
    words = [word for arg in argv for word in (arg.split() if arg is argv[0] else [arg])]
    name = [Path(words[0]).name]
    for word in words[1:3]:
        if word.startswith("-") or ":" in word:
            break
        name.append(Path(word).name)
    return " ".join(name)


def group_processes(pgid: int) -> list[tuple[int, str]]:
    """(pid, short name) of every process in the process group, by pid."""
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
            argv = [arg.decode(errors="replace") for arg in (entry / "cmdline").read_bytes().split(b"\0") if arg]
        except OSError:
            continue
        state, _, group = stat.rsplit(")", 1)[1].split()[:3]
        # A zombie has exited; only its parent's wait is missing.
        if argv and state != "Z" and int(group) == pgid:
            found.append((int(entry.name), describe(argv)))
    return sorted(found)


def uptime(pid: int) -> str:
    """How long `pid` has run, e.g. `3m`; empty if unknown."""
    started = start_time(pid)
    try:
        seconds = float(Path("/proc/uptime").read_text().split()[0]) - int(started) / os.sysconf("SC_CLK_TCK")
    except (OSError, TypeError, ValueError):
        return ""
    for unit, size in (("d", 86400), ("h", 3600), ("m", 60)):
        if seconds >= size:
            return f"{int(seconds // size)}{unit}"
    return f"{int(seconds)}s"


def old_build(pid: int) -> bool:
    """Whether `pid` runs an executable that was replaced on disk since it started."""
    try:
        return os.readlink(f"/proc/{pid}/exe").endswith(" (deleted)")
    except OSError:
        return False


def command_line(pid: int) -> list[str]:
    try:
        return [arg.decode(errors="replace") for arg in Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0") if arg]
    except OSError:
        return []


def receive(client: socket.socket, size: int) -> bytes:
    data = b""
    while len(data) < size:
        chunk = client.recv(size - len(data))
        if not chunk:
            raise ConnectionError("chr-server closed the connection")
        data += chunk
    return data


def rpc(path: str, methods: list[str], timeout: float = 2) -> dict[str, object]:
    """Calls parameterless chr-server methods on its NativeSocket (4-byte big-endian
    length, then JSON). A method's error comes back as `{"error": message}`."""
    results: dict[str, object] = {}
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(timeout)
        client.connect(path)
        for number, method in enumerate(methods, 1):
            request = json.dumps({"jsonrpc": "2.0", "id": number, "method": method}).encode()
            client.sendall(len(request).to_bytes(4, "big") + request)
            while True:
                reply = json.loads(receive(client, int.from_bytes(receive(client, 4), "big")))
                if reply.get("id") == number:
                    break
            results[method] = reply["result"] if "result" in reply else {"error": reply["error"].get("message", "error")}
    return results


HEALTH = ["sdi.get_status", "render.get_worker_status", "tracking.get_worker_status", "tracking.get_status"]


def health(first: dict[str, object], second: dict[str, object], seconds: float) -> list[str]:
    """Readable health from two samples of HEALTH taken `seconds` apart."""
    def rate(method: str, key: str) -> float:
        before, after = first.get(method), second.get(method)
        if not isinstance(before, dict) or not isinstance(after, dict) or key not in before:
            return 0.0
        return (after[key] - before[key]) / seconds

    def worker(status: dict) -> str:
        return f"{status['state']}" + (f" (pid {status['pid']})" if status.get("pid") else "") + f", {status['restarts']} restarts"
    lines = []
    problems = []
    sdi = second.get("sdi.get_status")
    if isinstance(sdi, dict) and "error" not in sdi:
        lines.append(f"  SDI output        {sdi['video_mode']}: {rate('sdi.get_status', 'frames_output'):.1f} slots/s, "
                     f"{rate('sdi.get_status', 'repeated_frames'):.1f} repeats/s; since start {sdi['frames_output']} slots, "
                     f"{sdi['repeated_frames']} repeated, {sdi['dropped_frames']} dropped")
    render = second.get("render.get_worker_status")
    if isinstance(render, dict) and "error" not in render:
        lines.append(f"  render worker     {worker(render)}, {render['deadline_misses']} deadline misses, "
                     f"{render['clean_feed_frames']} clean-feed frames")
        if render["state"] != "ready":
            problems.append(f"render worker {render['state']}: {render.get('last_error') or 'starting'}")
    tracker = second.get("tracking.get_worker_status")
    if isinstance(tracker, dict) and "error" not in tracker:
        lines.append(f"  tracking worker   {worker(tracker)}, {tracker['cameras']}/{tracker['requests']} cameras, "
                     f"{tracker['unavailable']} unavailable, {tracker['not_sent']} not sent"
                     + (f"; last error: {tracker['last_error']}" if tracker.get("last_error") else ""))
        if tracker["state"] != "ready":
            problems.append(f"tracking worker {tracker['state']}: {tracker.get('last_error') or 'starting'}")
    elif "tracking.get_status" in second and isinstance(second["tracking.get_status"], dict) \
            and "error" not in second["tracking.get_status"]:
        lines.append("  tracking worker   none: cameras come from tracking.frame")
    join = second.get("tracking.get_status")
    if isinstance(join, dict) and "error" not in join:
        joined = rate("tracking.get_status", "frames_joined")
        lines.append(f"  frames + cameras  {joined:.1f} joined/s, "
                     f"{rate('tracking.get_status', 'frames_without_tracking'):.1f} without camera/s; since start "
                     f"{join['frames_joined']} joined, {join['frames_without_tracking']} without camera, "
                     f"{join['samples_late']} late cameras, {join['frames_dropped']} dropped")
        if joined == 0:
            problems.append("no frame was paired with a camera in the last second (is the camera running?)")
    lines += [f"  PROBLEM: {problem}" for problem in problems]
    return lines


def report(config: Config) -> str:
    """What each component runs, with the sockets each process actually holds and its
    health from chr-server itself, then how to reach them."""
    sockets = listening_sockets()
    lines = []
    total = working = 0
    processes = {component.name: (pgid, group_processes(pgid)) for component in components(config)
                 if (pgid := running(component.name))}
    server_pid = next((pid for _, members in processes.values() for pid, name in members if name == "chr-server"), None)
    external_tracking = server_pid is not None and "--tracking-worker" not in command_line(server_pid)
    known = roles(config, external_tracking)
    for component in components(config):
        if component.name not in processes:
            lines.append(f"{component.name}: stopped")
            continue
        pgid, members = processes[component.name]
        lines.append(f"{component.name}: pgid {pgid}, log output/stack/{component.name}.log")
        wrappers = []
        for pid, name in members:
            total += 1
            if name.split()[0] in WRAPPERS:
                wrappers.append(name)
                continue
            working += 1
            held = process_sockets(pid, sockets)
            ports = [address for address in held if address in known]
            others = len(held) - len(ports)
            notes = f"up {uptime(pid)}" + (", OLD BUILD: rebuilt since it started, restart to run it" if old_build(pid) else "")
            role = "" if ports else PORTLESS.get(name, "no listening socket")
            if name == "chr-tracking-sim" and external_tracking:
                role += "; Ti/C to chr-server's socket (tracking.frame)"
            lines.append(f"  {name:28} pid {pid:<8} {notes:8} {role}".rstrip())
            lines += [f"      {address:40} {known[address]}" for address in ports]
            if others:
                lines.append(f"      + {others} other sockets (e.g. WebRTC ICE candidates)")
        if wrappers:
            counts = {name: wrappers.count(name) for name in wrappers}
            lines.append("  wrappers: " + ", ".join(f"{count} x {name}" for name, count in counts.items()))
    lines.insert(0, f"{total} processes, {working} doing the work:" if processes else "nothing running")
    if server_pid is not None:
        path = f"{config.dir}/render.sock"
        try:
            first = rpc(path, HEALTH)
            time.sleep(1)
            second = rpc(path, HEALTH)
            lines += [f"health (asked chr-server on {path}, 1 s apart):", *health(first, second, 1.0)]
        except OSError as error:
            lines.append(f"health: chr-server does not answer on {path}: {error}")
    if not processes:
        return "\n".join(lines)
    lines.append("open:")
    if config.pitch_studio_port:
        lines.append(f"  placement editor  http://127.0.0.1:{config.pitch_studio_port}/")
    if config.monitor_page_port:
        lines.append(f"  program monitor   http://127.0.0.1:{config.monitor_page_port}/?transport=tcp"
                     f"&ws=ws://{config.server_ws}&monitor=ws://{config.monitor_ws}")
    lines.append(f"  SRT player        srt://{config.srt}" + (f"  or tcp://127.0.0.1:{config.srt_relay_port}" if config.srt_relay_port else ""))
    forwards = " ".join(f"-L {port}:127.0.0.1:{port}" for port in forwarded_ports(config))
    lines.append(f"from another machine: ssh -N {forwards} {getpass.getuser()}@{socket.gethostname()}")
    lines.append(f"  (ICE-TCP {config.webrtc_tcp} and up listen only while a WebRTC viewer is connected)")
    return "\n".join(lines)


# ---------------------------------------------------------------------------------------
# Process records


def start_time(pid: int) -> str | None:
    """The kernel's start time of `pid`, which tells a recorded process from a reused PID."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    # Field 22; the command name in field 2 may contain spaces, so split after its ')'.
    return stat.rsplit(")", 1)[1].split()[19]


def running(name: str) -> int | None:
    """The recorded process group of `name` if its leader is still the process we started."""
    record = STATE / f"{name}.pid"
    try:
        pid, started = record.read_text().split()
    except (OSError, ValueError):
        return None
    if start_time(int(pid)) != started:
        record.unlink(missing_ok=True)
        return None
    return int(pid)


def listening(host: str, port: int) -> bool:
    with socket.socket() as probe:
        probe.settimeout(0.5)
        return probe.connect_ex((host, port)) == 0


def start(config: Config, wait_s: float) -> None:
    STATE.mkdir(parents=True, exist_ok=True)
    stack = components(config)
    for component in stack:
        if running(component.name):
            print(f"{component.name}: already running")
            continue
        busy = [f"{host}:{port}" for _, host, port in component.ready if listening(host, port)]
        if busy:
            print(f"{component.name}: not started, {', '.join(busy)} is in use by another process")
            continue
        log = open(STATE / f"{component.name}.log", "ab")
        process = subprocess.Popen(component.command, cwd=ROOT, env={**os.environ, **component.env},
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        (STATE / f"{component.name}.pid").write_text(f"{process.pid} {start_time(process.pid)}\n")
        print(f"{component.name}: started (pid {process.pid}, log output/stack/{component.name}.log)")
    pending = {(label, host, port) for component in stack for label, host, port in component.ready}
    deadline = time.monotonic() + wait_s
    while pending and time.monotonic() < deadline:
        for label, host, port in sorted(pending):
            if listening(host, port):
                print(f"ready: {label} on {host}:{port}")
                pending.discard((label, host, port))
        time.sleep(1)
    for label, host, port in sorted(pending):
        print(f"not ready after {wait_s:.0f}s: {label} on {host}:{port} (the first start builds; see the log)")
    # Workers become ready and frames start flowing in the first seconds; report steady rates.
    time.sleep(3)
    print(report(config))


def stop() -> list[tuple[str, int, str, str]]:
    """Stops every recorded component: TERM to its process group, then KILL to what is
    left after STOP_TIMEOUT_S. Prints and returns every process it stopped, as
    (component, pid, name, outcome)."""
    names = sorted(path.stem for path in STATE.glob("*.pid")) if STATE.exists() else []
    groups = {name: pid for name in names if (pid := running(name))}
    if not groups:
        print("nothing running")
        return []
    # Listed before signalling: a stopped process can no longer be named.
    members = {name: group_processes(pgid) for name, pgid in groups.items()}
    for pgid in groups.values():
        os.killpg(pgid, signal.SIGTERM)
    deadline = time.monotonic() + STOP_TIMEOUT_S
    stopped = []
    for name, pgid in groups.items():
        while time.monotonic() < deadline and group_alive(pgid):
            time.sleep(0.2)
        survivors = dict(group_processes(pgid))
        if survivors:
            os.killpg(pgid, signal.SIGKILL)
        (STATE / f"{name}.pid").unlink(missing_ok=True)
        processes = dict(members[name]) | survivors
        print(f"{name} (pgid {pgid}): " + (f"{len(survivors)} killed after {STOP_TIMEOUT_S}s" if survivors else "stopped"))
        for pid, process in sorted(processes.items()):
            outcome = "killed (SIGKILL)" if pid in survivors else "stopped (SIGTERM)"
            print(f"  pid {pid:<8} {process:28} {outcome}")
            stopped.append((name, pid, process, outcome))
    killed = sum(outcome.startswith("killed") for *_, outcome in stopped)
    print(f"{len(stopped)} processes in {len(groups)} components stopped"
          + (f", {killed} of them killed" if killed else ""))
    return stopped


def group_alive(pgid: int) -> bool:
    return bool(group_processes(pgid))


def status(config: Config) -> None:
    print(report(config))
    down = [f"{label} {host}:{port}" for component in components(config) if running(component.name)
            for label, host, port in component.ready if not listening(host, port)]
    if down:
        print("not answering: " + ", ".join(down))


# ---------------------------------------------------------------------------------------
# SRT -> TCP relay


def relay(url: str, port: int) -> None:
    """Gives every TCP viewer its own ffmpeg SRT caller (stream copy, no re-encode), so
    nothing reads SRT while nobody watches and an SSH port forward can carry the stream."""

    def serve(client: socket.socket, address: tuple[str, int]) -> None:
        print(f"viewer {address} connected", flush=True)
        ffmpeg = subprocess.Popen(
            ["ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-fflags", "nobuffer",
             "-i", url, "-c", "copy", "-f", "mpegts", "pipe:1"], stdout=subprocess.PIPE)
        try:
            while chunk := ffmpeg.stdout.read1(65536):
                client.sendall(chunk)
        except OSError:
            pass
        finally:
            ffmpeg.kill()
            ffmpeg.wait()
            client.close()
            print(f"viewer {address} left", flush=True)

    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", port))
    server.listen()
    print(f"relaying {url} on tcp://127.0.0.1:{port}", flush=True)
    while True:
        client, address = server.accept()
        threading.Thread(target=serve, args=(client, address), daemon=True).start()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=["start", "stop", "restart", "status", "relay"])
    parser.add_argument("args", nargs="*", help=argparse.SUPPRESS)
    parser.add_argument("--config", type=Path, default=STATE / "stack.toml")
    parser.add_argument("--wait", type=float, default=300, help="seconds to wait for readiness after start")
    options = parser.parse_args()
    if options.command == "relay":
        relay(options.args[0], int(options.args[1]))
        return
    if options.command == "stop":
        stop()
        return
    config = load_config(options.config)
    if options.command == "restart":
        stop()
    if options.command in ("start", "restart"):
        start(config, options.wait)
    else:
        status(config)


if __name__ == "__main__":
    main()
