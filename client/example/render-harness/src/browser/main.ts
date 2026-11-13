// Program monitor: controls the triangle over chr-server's WebSocket and shows the SDI
// program output, which the separate chr-monitor encodes and streams over WebRTC.
// `?ws=` is the control endpoint and `?monitor=` the monitor's signaling endpoint.
// `?auto=1` connects, opens the stream, rotates, and writes a JSON report to #result
// for the headless browser check (src/harness/browser.ts).

import type { MonitorStatus, SdiStatus, TriangleState } from "../protocol.ts";
import { RpcClient, connectWebSocket } from "../rpc.ts";
import { installScenePanel } from "./scene-panel.ts";
import { programSelfTest } from "./program-self-test.ts";

const params = new URLSearchParams(location.search);
const liveProgram = params.get("live") === "1";
const calibrated = params.get("calibrated") === "1";
const $ = <T extends HTMLElement>(selector: string) => {
  const element = document.querySelector<T>(selector);
  if (element === null) throw new Error(`missing ${selector}`);
  return element;
};

const wsInput = $<HTMLInputElement>("#ws-url");
wsInput.value = params.get("ws") ?? `ws://${location.hostname || "127.0.0.1"}:8787`;
const monitorInput = $<HTMLInputElement>("#monitor-url");
monitorInput.value = params.get("monitor") ?? `ws://${location.hostname || "127.0.0.1"}:8789`;
const monitorLabel = $("#monitor-connection");
const connectButton = $<HTMLButtonElement>("#connect");
const streamButton = $<HTMLButtonElement>("#stream");
const startButton = $<HTMLButtonElement>("#start");
const stopButton = $<HTMLButtonElement>("#stop");
const connectionLabel = $("#connection");
const video = $<HTMLVideoElement>("video");
const overlay = $("#overlay");

let rpc: RpcClient | null = null;
/** chr-monitor; independent of control, so either can be down without the other. */
let monitorRpc: RpcClient | null = null;
const scenePanel = installScenePanel(() => rpc);
let pollTimer: ReturnType<typeof setInterval> | undefined;

function log(message: string): void {
  const line = `${new Date().toLocaleTimeString()}  ${message}\n`;
  const element = $("#log");
  element.textContent = line + (element.textContent ?? "");
}

function renderList(element: HTMLElement, entries: [string, string][]): void {
  element.replaceChildren(
    ...entries.flatMap(([key, value]) => {
      const dt = document.createElement("dt");
      dt.textContent = key;
      const dd = document.createElement("dd");
      dd.textContent = value;
      return [dt, dd];
    }),
  );
}

function setConnected(connected: boolean, label: string): void {
  connectionLabel.textContent = label;
  connectionLabel.className = `status ${connected ? "ok" : "bad"}`;
  connectButton.textContent = connected ? "Disconnect" : "Connect";
  startButton.disabled = !connected;
  stopButton.disabled = !connected;
}

function setMonitor(client: RpcClient | null, label: string): void {
  monitorRpc = client;
  monitorLabel.textContent = label;
  monitorLabel.className = `status ${client ? "ok" : "bad"}`;
  streamButton.disabled = client === null;
}

function showState(state: TriangleState): void {
  renderList($("#scene"), [
    ["rotating", String(state.rotating)],
    ["angle", `${((state.angle_rad * 180) / Math.PI).toFixed(1)}°`],
    ["speed", `${((state.angular_velocity_rad_per_s * 180) / Math.PI).toFixed(1)}°/s`],
    ["frame", String(state.frame)],
  ]);
}

function showSdi(status: SdiStatus): void {
  renderList($("#sdi"), [
    ["mode", status.video_mode],
    ["format", status.pixel_format],
    ["slots out", String(status.frames_output)],
    ["repeated", String(status.repeated_frames)],
    ["dropped", String(status.dropped_frames)],
    ["source frame", String(status.last_source_frame ?? "-")],
  ]);
}

function showMonitor(status: MonitorStatus): void {
  renderList($("#monitor"), [
    ["encoded", String(status.frames_encoded)],
    ["skipped", String(status.frames_skipped)],
    ["viewers", String(status.viewers)],
    ["SRT callers", String(status.srt_callers)],
  ]);
}

// ---------------------------------------------------------------------------------------
// WebRTC stream of the program output

interface StreamStats {
  source: "program";
  state: string;
  framesDecoded: number;
  fps: number;
  width: number;
  height: number;
  decoder: string;
  jitterBufferMs: number;
}

const emptyStats = (state: string): StreamStats => ({
  source: "program",
  state,
  framesDecoded: 0,
  fps: 0,
  width: 0,
  height: 0,
  decoder: "-",
  jitterBufferMs: 0,
});

let pc: RTCPeerConnection | null = null;
let stats = emptyStats("closed");

function showStats(): void {
  renderList($("#stats"), [
    ["state", stats.state],
    ["resolution", stats.width ? `${stats.width}×${stats.height}` : "-"],
    ["fps", stats.fps.toFixed(1)],
    ["decoded", String(stats.framesDecoded)],
    ["decoder", stats.decoder],
    ["jitter buffer", `${stats.jitterBufferMs.toFixed(0)} ms`],
  ]);
}

async function openStream(): Promise<void> {
  if (monitorRpc === null || pc !== null) return;
  const peer = new RTCPeerConnection();
  pc = peer;
  streamButton.textContent = "Close stream";
  overlay.textContent = "Connecting…";
  overlay.hidden = false;
  peer.addTransceiver("video", { direction: "recvonly" });
  peer.ontrack = (event) => {
    video.srcObject = event.streams[0] ?? new MediaStream([event.track]);
  };
  video.onplaying = () => {
    overlay.hidden = true;
  };
  peer.onconnectionstatechange = () => {
    log(`stream: ${peer.connectionState}`);
    if (peer.connectionState === "failed") closeStream();
  };
  try {
    await peer.setLocalDescription(await peer.createOffer());
    await iceGatheringComplete(peer);
    const offer = peer.localDescription;
    if (offer === null) throw new Error("no local description");
    await peer.setRemoteDescription(await monitorRpc.offer({ type: "offer", sdp: offer.sdp }));
  } catch (error) {
    log(`stream failed: ${(error as Error).message}`);
    closeStream();
  }
}

function closeStream(): void {
  pc?.close();
  pc = null;
  video.srcObject = null;
  streamButton.textContent = "Open stream";
  overlay.textContent = "Stream closed";
  overlay.hidden = false;
  stats = emptyStats("closed");
  showStats();
}

async function pollStream(): Promise<void> {
  const peer = pc;
  if (peer === null) return;
  let inbound: Record<string, unknown> | undefined;
  (await peer.getStats()).forEach((report: Record<string, unknown>) => {
    if (report.type === "inbound-rtp" && report.kind === "video") inbound = report;
  });
  const number = (key: string) => Number(inbound?.[key] ?? 0);
  const emitted = number("jitterBufferEmittedCount");
  stats = {
    source: "program",
    state: peer.connectionState,
    framesDecoded: number("framesDecoded"),
    fps: number("framesPerSecond"),
    width: number("frameWidth"),
    height: number("frameHeight"),
    decoder: String(inbound?.decoderImplementation ?? "-"),
    jitterBufferMs: emitted > 0 ? (number("jitterBufferDelay") / emitted) * 1000 : 0,
  };
  showStats();
}

function iceGatheringComplete(peer: RTCPeerConnection): Promise<void> {
  if (peer.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      if (peer.iceGatheringState === "complete") {
        peer.removeEventListener("icegatheringstatechange", done);
        resolve();
      }
    };
    peer.addEventListener("icegatheringstatechange", done);
    // Host candidates gather quickly; do not wait on unreachable interfaces forever.
    setTimeout(resolve, 3000);
  });
}

// ---------------------------------------------------------------------------------------
// Control

async function connect(): Promise<void> {
  let client: RpcClient;
  try {
    client = new RpcClient(await connectWebSocket(wsInput.value), { idPrefix: "ui" });
  } catch (error) {
    setConnected(false, (error as Error).message);
    return;
  }
  rpc = client;
  setConnected(true, "connected");
  log(`connected to ${wsInput.value}`);
  let monitor: RpcClient | null = null;
  let monitorError = "";
  try {
    monitor = new RpcClient(await connectWebSocket(monitorInput.value), { idPrefix: "monitor" });
  } catch (error) {
    monitorError = (error as Error).message;
  }
  // Disconnected (or reconnected) while the monitor connection was pending.
  if (rpc !== client) {
    monitor?.close();
    return;
  }
  if (monitor === null) {
    setMonitor(null, `monitor: ${monitorError}`);
  } else {
    setMonitor(monitor, "monitor connected");
    log(`monitor at ${monitorInput.value}`);
  }
  pollTimer = setInterval(() => void poll(), 500);
  await poll();
}

function disconnect(reason: string): void {
  clearInterval(pollTimer);
  closeStream();
  rpc?.close();
  rpc = null;
  monitorRpc?.close();
  setMonitor(null, "monitor disconnected");
  setConnected(false, reason);
}

async function poll(): Promise<void> {
  if (rpc === null) return;
  try {
    if (liveProgram) {
      const rotation = await scenePanel.rotationState();
      renderList($("#scene"), [
        ["controller", rotation?.running ? "running" : "stopped"],
        ["angle since start", `${((rotation?.angle ?? 0) * 180 / Math.PI).toFixed(1)}°`],
        ["speed", `${((rotation?.speed ?? 0) * 180 / Math.PI).toFixed(1)}°/s`],
        ["output slot", String(rotation?.frame ?? "-")],
      ]);
    } else {
      showState(await rpc.getState());
    }
    showSdi(await rpc.getSdiStatus());
  } catch (error) {
    log(`poll failed: ${(error as Error).message}`);
    disconnect("connection lost");
    return;
  }
  const monitor = monitorRpc;
  if (monitor === null) return;
  try {
    showMonitor(await monitor.getMonitorStatus());
    await pollStream();
  } catch (error) {
    // Already replaced or closed by a disconnect.
    if (monitorRpc !== monitor) return;
    // Losing the monitor never affects control or the program output.
    log(`monitor lost: ${(error as Error).message}`);
    closeStream();
    monitor.close();
    setMonitor(null, "monitor lost");
  }
}

async function command(run: (client: RpcClient) => Promise<TriangleState>, label: string): Promise<void> {
  if (rpc === null) return;
  try {
    const state = await run(rpc);
    showState(state);
    log(`${label}: applied at frame ${state.frame}`);
  } catch (error) {
    log(`${label} failed: ${(error as Error).message}`);
  }
}

const speedInput = $<HTMLInputElement>("#speed");
const directionInput = $<HTMLSelectElement>("#rotation-direction");
const speed = () => (Number(speedInput.value) * Math.PI) / 180;

directionInput.onchange = () => {
  const magnitude = speedInput.valueAsNumber;
  if (Number.isFinite(magnitude)) {
    speedInput.value = String(Math.abs(magnitude) * Number(directionInput.value));
  }
};
speedInput.oninput = () => {
  if (Number.isFinite(speedInput.valueAsNumber)) {
    directionInput.value = speedInput.valueAsNumber < 0 ? "-1" : "1";
  }
};

connectButton.onclick = () => (rpc ? disconnect("disconnected") : void connect());
streamButton.onclick = () => (pc ? closeStream() : void openStream());
async function meshCommand(action: () => Promise<void>, label: string): Promise<void> {
  startButton.disabled = stopButton.disabled = true;
  try { await action(); log(label); }
  catch (error) { log(`${label} failed: ${String(error)}`); }
  finally {
    startButton.disabled = stopButton.disabled = rpc === null;
    await poll();
  }
}
startButton.onclick = () => liveProgram
  ? void meshCommand(() => scenePanel.startRotation(speed()), "mesh rotation started")
  : void command((client) => client.startRotation({ angular_velocity_rad_per_s: speed() }), "start rotation");
stopButton.onclick = () => liveProgram
  ? void meshCommand(() => scenePanel.stopRotation(), "mesh rotation stopped")
  : void command((client) => client.stopRotation(), "stop rotation");
setConnected(false, "disconnected");
setMonitor(null, "monitor disconnected");
showStats();

if (liveProgram) {
  document.title = "chr software SDI simulation";
  $("h1").textContent = "chr software SDI simulation";
  $(".pipeline").textContent =
    "MP4 / FFmpeg / GStreamer → renderer → simulated SDI → program tap → chr-monitor H.264 → WebRTC";
  const rotationPanel = startButton.closest<HTMLElement>(".panel")!;
  rotationPanel.querySelector("h2")!.textContent = calibrated ? "Calibrated model rotation" : "Orange mesh rotation";
  rotationPanel.querySelector(".pipeline")!.textContent =
    calibrated
      ? "Server advances the GLB every input tick using the SDI frame clock. The shadow catcher stays fixed."
      : "Server advances the orange mesh using the SDI frame clock. The gray receiver stays fixed.";
  document.querySelector("aside")!.prepend(rotationPanel);
  $<HTMLDetailsElement>("#program-scene").open = true;
  $("#pitch-provenance").textContent =
    calibrated
      ? "Owner-local diagnostic calibration with per-frame distortion/warp. Authored lighting; no occlusion, physical SDI or live tracking. Camera locked to the clip."
      : "Software video input and simulated SDI output. The sample overlay is not calibrated to the source camera.";
  if (calibrated) {
    for (const button of document.querySelectorAll<HTMLButtonElement>("[data-camera]")) button.disabled = true;
  }
  if (params.get("auto") !== "1") void (async () => {
    await connect();
    $<HTMLButtonElement>("#scene-load").click();
    await openStream();
  })();
}

if (params.get("auto") === "1") {
  void (async () => {
    await connect();
    if (liveProgram) $<HTMLButtonElement>("#scene-load").click();
    await openStream();
    if (!liveProgram) await command((client) => client.startRotation({ angular_velocity_rad_per_s: speed() }), "start rotation");
    const seconds = Number(params.get("seconds") ?? 4);
    await new Promise((done) => setTimeout(done, seconds * 1000));
    if (params.get("program") === "1") {
      try {
        if (rpc === null) throw new Error("No RPC connection");
        $("#program-result").textContent = JSON.stringify(await programSelfTest(rpc, video));
      } catch (error) {
        $("#program-result").textContent = JSON.stringify({ passed: false, error: String(error) });
      }
    }
    await poll();
    const result = $("#result");
    result.textContent = JSON.stringify([stats]);
    result.hidden = false;
  })();
}
