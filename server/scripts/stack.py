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


def stop() -> None:
    names = sorted(path.stem for path in STATE.glob("*.pid")) if STATE.exists() else []
    groups = {name: pid for name in names if (pid := running(name))}
    if not groups:
        print("nothing running")
    for name, pid in groups.items():
        os.killpg(pid, signal.SIGTERM)
    deadline = time.monotonic() + STOP_TIMEOUT_S
    for name, pid in groups.items():
        while time.monotonic() < deadline and group_alive(pid):
            time.sleep(0.2)
        if group_alive(pid):
            os.killpg(pid, signal.SIGKILL)
            print(f"{name}: killed after {STOP_TIMEOUT_S}s")
        else:
            print(f"{name}: stopped")
        (STATE / f"{name}.pid").unlink(missing_ok=True)


def group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    return True


def status(config: Config) -> None:
    for component in components(config):
        pid = running(component.name)
        endpoints = ", ".join(
            f"{label} {host}:{port} {'up' if listening(host, port) else 'down'}" for label, host, port in component.ready)
        print(f"{component.name:13} {'running (pid %d)' % pid if pid else 'stopped':20} {endpoints}")
    print(f"SRT listener {config.srt}; WebRTC ICE-TCP from {config.webrtc_tcp}; logs in output/stack/")


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
