// Program monitor: a read-only view of the SDI program output that chr-monitor streams over
// WebRTC, with chr-server and chr-monitor diagnostics. It sends no control commands.
// chr-server and chr-monitor are separate processes, so each connection is independent and
// reconnects on its own: losing chr-server never stops the video, and losing chr-monitor
// never stops the server diagnostics.
// `?ws=` is chr-server's WebSocket and `?monitor=` the monitor's. `?auto=1` connects, opens
// the stream and writes a JSON report to #result for the headless browser check
// (src/harness/browser.ts).

import { ErrorCode, type MonitorStatus, type SdiStatus, type TrackingStatus, type WorkerStatus } from "../protocol.ts";
import { RpcClient, RpcError, connectWebSocket } from "../rpc.ts";

const POLL_MS = 1000;
const RETRY_MS = 2000;

const params = new URLSearchParams(location.search);
const $ = <T extends HTMLElement>(selector: string) => {
  const element = document.querySelector<T>(selector);
  if (element === null) throw new Error(`missing ${selector}`);
  return element;
};

const wsInput = $<HTMLInputElement>("#ws-url");
wsInput.value = params.get("ws") ?? `ws://${location.hostname || "127.0.0.1"}:8787`;
const monitorInput = $<HTMLInputElement>("#monitor-url");
monitorInput.value = params.get("monitor") ?? `ws://${location.hostname || "127.0.0.1"}:8789`;
const connectButton = $<HTMLButtonElement>("#connect");
const streamButton = $<HTMLButtonElement>("#stream");
const video = $<HTMLVideoElement>("video");
const overlay = $("#overlay");

/** A pasted URL without surrounding whitespace or quotes. */
const cleanUrl = (input: HTMLInputElement) => (input.value = input.value.replace(/^[\s"']+|[\s"']+$/g, ""));

function log(message: string): void {
  const line = `${new Date().toLocaleTimeString()}  ${message}\n`;
  const element = $("#log");
  element.textContent = line + (element.textContent ?? "");
}

/** Label, value and whether the value signals a problem. */
type Entry = [string, string, boolean?];

function renderList(element: HTMLElement, entries: Entry[]): void {
  element.replaceChildren(
    ...entries.flatMap(([key, value, warn]) => {
      const dt = document.createElement("dt");
      dt.textContent = key;
      const dd = document.createElement("dd");
      dd.textContent = value;
      if (warn) dd.className = "warn";
      return [dt, dd];
    }),
  );
}

const lastCounts = new Map<string, { at: number; value: number }>();

/** Per-second change of a monotonic counter since the previous poll. */
function rate(key: string, value: number): number | null {
  const now = performance.now();
  const last = lastCounts.get(key);
  lastCounts.set(key, { at: now, value });
  // A lower value means the process restarted and its counters began again.
  if (last === undefined || value < last.value || now <= last.at) return null;
  return (value - last.value) / ((now - last.at) / 1000);
}

/** A counter with its rate; `bad` counters warn while rising. */
function count(label: string, key: string, value: number, bad = false): Entry {
  const perSecond = rate(key, value);
  if (perSecond === null) return [label, String(value)];
  return [label, `${value}  (${perSecond.toFixed(1)}/s)`, bad && perSecond > 0];
}

const duration = (ms: number) => {
  const seconds = Math.floor(ms / 1000);
  return seconds < 60 ? `${seconds}s` : seconds < 3600 ? `${Math.floor(seconds / 60)}m ${seconds % 60}s`
    : `${Math.floor(seconds / 3600)}h ${Math.floor(seconds / 60) % 60}m`;
};

// ---------------------------------------------------------------------------------------
// Connections

/** One WebSocket JSON-RPC endpoint that reconnects by itself while wanted. */
class Link {
  client: RpcClient | null = null;
  since = 0;
  readonly name: string;
  readonly input: HTMLInputElement;
  readonly label: HTMLElement;
  readonly onChange: (client: RpcClient | null) => void;
  #wanted = false;
  #timer: ReturnType<typeof setTimeout> | undefined;

  constructor(name: string, input: HTMLInputElement, label: HTMLElement, onChange: (client: RpcClient | null) => void) {
    this.name = name;
    this.input = input;
    this.label = label;
    this.onChange = onChange;
    this.#show(false, "disconnected");
  }

  get wanted(): boolean {
    return this.#wanted;
  }

  start(): void {
    this.#wanted = true;
    void this.#connect();
  }

  stop(): void {
    this.#wanted = false;
    clearTimeout(this.#timer);
    this.#drop("disconnected");
  }

  /** A request on `client` failed without an RPC error: the process or socket went away. */
  lost(client: RpcClient, reason: string): void {
    if (client !== this.client) return;
    log(`${this.name} lost: ${reason}`);
    this.#drop("lost, retrying");
    this.#retry();
  }

  async #connect(): Promise<void> {
    if (!this.#wanted || this.client !== null) return;
    const url = cleanUrl(this.input);
    this.#show(false, "connecting…");
    try {
      const client = new RpcClient(await connectWebSocket(url), { idPrefix: this.name, timeoutMs: 3000 });
      if (!this.#wanted || this.client !== null) {
        client.close();
        return;
      }
      this.client = client;
      this.since = Date.now();
      this.#show(true, "connected");
      log(`${this.name} connected at ${url}`);
      this.onChange(client);
    } catch (error) {
      this.#show(false, `${(error as Error).message}, retrying`);
      this.#retry();
    }
  }

  #retry(): void {
    clearTimeout(this.#timer);
    if (this.#wanted) this.#timer = setTimeout(() => void this.#connect(), RETRY_MS);
  }

  #drop(reason: string): void {
    const client = this.client;
    this.client = null;
    client?.close();
    this.#show(false, reason);
    if (client !== null) this.onChange(null);
  }

  #show(connected: boolean, text: string): void {
    this.label.textContent = `${this.name} ${text}`;
    this.label.className = `status ${connected ? "ok" : "bad"}`;
  }
}

const server = new Link("server", wsInput, $("#connection"), (client) => {
  trackingServed = true;
  lastWorker = null;
  if (client === null) for (const id of ["#sdi", "#worker", "#tracking"]) renderList($(id), []);
});
const monitor = new Link("monitor", monitorInput, $("#monitor-connection"), (client) => {
  if (client === null) {
    closeStream();
    renderList($("#monitor"), []);
  } else if (streamWanted) {
    void openStream();
  }
});

connectButton.onclick = () => {
  if (server.wanted || monitor.wanted) {
    server.stop();
    monitor.stop();
    connectButton.textContent = "Connect";
  } else {
    server.start();
    monitor.start();
    connectButton.textContent = "Disconnect";
  }
};

// ---------------------------------------------------------------------------------------
// chr-server diagnostics

/** False once the server answered that it does not run a tracked program. */
let trackingServed = true;
let lastWorker: WorkerStatus | null = null;
let serverRoundTripMs = 0;

function showSdi(status: SdiStatus): void {
  renderList($("#sdi"), [
    ["mode", status.video_mode],
    ["format", status.pixel_format],
    count("slots out", "sdi.out", status.frames_output),
    count("repeated", "sdi.repeated", status.repeated_frames, true),
    count("dropped", "sdi.dropped", status.dropped_frames, true),
    ["source frame", String(status.last_source_frame ?? "-")],
  ]);
}

function showWorker(status: WorkerStatus): void {
  const last = lastWorker;
  if (last !== null) {
    if (status.pid !== last.pid || status.state !== last.state) {
      log(`render worker ${status.state}${status.pid === null ? "" : ` (pid ${status.pid})`}`);
    }
    if (status.last_error !== null && status.last_error !== last.last_error) log(`render worker error: ${status.last_error}`);
  }
  lastWorker = status;
  renderList($("#worker"), [
    ["state", `${status.state}${status.pid === null ? "" : ` (pid ${status.pid})`}`, status.state !== "ready"],
    count("restarts", "worker.restarts", status.restarts, true),
    count("deadline misses", "worker.misses", status.deadline_misses, true),
    count("clean feed", "worker.clean", status.clean_feed_frames, true),
    count("repeated (no clean feed)", "worker.unavailable", status.repeated_for_unavailable_clean_feed, true),
    ["last error", status.last_error ?? "-", status.last_error !== null],
  ]);
}

function showTracking(status: TrackingStatus | null): void {
  if (status === null) {
    renderList($("#tracking"), [["status", "not a tracked program"]]);
    return;
  }
  renderList($("#tracking"), [
    count("samples received", "tracking.received", status.samples_received),
    count("samples late", "tracking.late", status.samples_late, true),
    count("frames joined", "tracking.joined", status.frames_joined),
    count("without tracking", "tracking.missing", status.frames_without_tracking, true),
    count("dropped", "tracking.dropped", status.frames_dropped, true),
    count("overwritten", "tracking.overwritten", status.frames_overwritten, true),
    count("poses fresh", "tracking.fresh", status.poses_fresh),
    count("poses interpolated", "tracking.interpolated", status.poses_interpolated),
    count("poses reused", "tracking.reused", status.poses_reused, true),
    count("poses unavailable", "tracking.unavailable", status.poses_unavailable, true),
    count("poses overdue", "tracking.overdue", status.poses_overdue, true),
  ]);
}

async function trackingStatus(client: RpcClient): Promise<TrackingStatus | null> {
  if (!trackingServed) return null;
  try {
    return await client.getTrackingStatus();
  } catch (error) {
    if (!(error instanceof RpcError) || error.code !== ErrorCode.MethodNotFound) throw error;
    trackingServed = false;
    return null;
  }
}

async function pollServer(): Promise<void> {
  const client = server.client;
  if (client === null) return;
  try {
    const started = performance.now();
    const sdi = await client.getSdiStatus();
    serverRoundTripMs = performance.now() - started;
    const worker = await client.getWorkerStatus();
    const tracking = await trackingStatus(client);
    if (server.client !== client) return;
    showSdi(sdi);
    showWorker(worker);
    showTracking(tracking);
  } catch (error) {
    if (error instanceof RpcError) log(`server: ${error.message}`);
    else server.lost(client, (error as Error).message);
  }
}

// ---------------------------------------------------------------------------------------
// chr-monitor and its WebRTC stream

function showMonitor(status: MonitorStatus): void {
  renderList($("#monitor"), [
    count("encoded", "monitor.encoded", status.frames_encoded),
    count("skipped", "monitor.skipped", status.frames_skipped, true),
    ["WebRTC viewers", String(status.viewers)],
    ["SRT callers", String(status.srt_callers)],
    ["preview", `${status.preview_width}x${status.preview_height} ${status.encoder}`],
  ]);
}

/** The report the headless browser check reads. */
interface StreamStats {
  source: "program";
  state: string;
  framesDecoded: number;
  fps: number;
  width: number;
  height: number;
  decoder: string;
  path: string;
}

const emptyStats = (state: string): StreamStats => ({
  source: "program",
  state,
  framesDecoded: 0,
  fps: 0,
  width: 0,
  height: 0,
  decoder: "-",
  path: "-",
});

let pc: RTCPeerConnection | null = null;
let streamWanted = false;
let stats = emptyStats("closed");
let lastPath = "";

function setStreamWanted(wanted: boolean): void {
  streamWanted = wanted;
  streamButton.textContent = wanted ? "Close stream" : "Open stream";
  if (wanted) void openStream();
  else closeStream();
}

async function openStream(): Promise<void> {
  const client = monitor.client;
  if (client === null || pc !== null) {
    if (client === null) overlay.textContent = "Waiting for chr-monitor…";
    return;
  }
  const peer = new RTCPeerConnection();
  pc = peer;
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
    log(`stream ${peer.connectionState}`);
    if (peer.connectionState === "failed" && pc === peer) {
      closeStream();
      setTimeout(() => void (streamWanted && openStream()), RETRY_MS);
    }
  };
  peer.oniceconnectionstatechange = () => log(`stream ICE ${peer.iceConnectionState}`);
  try {
    await peer.setLocalDescription(await peer.createOffer());
    await iceGatheringComplete(peer);
    const offer = peer.localDescription;
    if (offer === null) throw new Error("no local description");
    const answer = await client.offer({ type: "offer", sdp: candidates(offer.sdp) });
    await peer.setRemoteDescription({ type: "answer", sdp: candidates(answer.sdp) });
  } catch (error) {
    log(`stream failed: ${(error as Error).message}`);
    if (pc === peer) closeStream();
  }
}

function closeStream(): void {
  pc?.close();
  pc = null;
  lastPath = "";
  video.srcObject = null;
  overlay.textContent = streamWanted ? "Waiting for chr-monitor…" : "Stream closed";
  overlay.hidden = false;
  stats = emptyStats("closed");
  renderList($("#transport"), [["state", "closed"]]);
  renderList($("#stats"), []);
}

type Report = Record<string, unknown>;

async function pollStream(): Promise<void> {
  const peer = pc;
  if (peer === null) return;
  const reports = new Map<string, Report>();
  let inbound: Report | undefined;
  let transport: Report | undefined;
  (await peer.getStats()).forEach((report: Report) => {
    reports.set(String(report.id), report);
    if (report.type === "inbound-rtp" && report.kind === "video") inbound = report;
    if (report.type === "transport") transport = report;
  });
  if (pc !== peer) return;
  const pair = reports.get(String(transport?.selectedCandidatePairId));
  const local = reports.get(String(pair?.localCandidateId));
  const remote = reports.get(String(pair?.remoteCandidateId));
  const codec = reports.get(String(inbound?.codecId));
  const number = (key: string, report = inbound) => Number(report?.[key] ?? 0);
  const path = local && remote
    ? `${local.protocol} ${local.candidateType} → ${remote.address}:${remote.port} (${remote.candidateType})`
    : "-";
  if (path !== lastPath && path !== "-") log(`stream path ${path}`);
  lastPath = path;

  const decoded = number("framesDecoded");
  const emitted = number("jitterBufferEmittedCount");
  stats = {
    source: "program",
    state: peer.connectionState,
    framesDecoded: decoded,
    fps: number("framesPerSecond"),
    width: number("frameWidth"),
    height: number("frameHeight"),
    decoder: String(inbound?.decoderImplementation ?? "-"),
    path,
  };
  const bytesPerSecond = rate("rtc.bytes", number("bytesReceived"));
  renderList($("#transport"), [
    ["state", `${peer.connectionState} / ICE ${peer.iceConnectionState}`, peer.connectionState !== "connected"],
    ["path", path],
    ["RTT", pair?.currentRoundTripTime === undefined ? "-" : `${(number("currentRoundTripTime", pair) * 1000).toFixed(1)} ms`],
    ["bitrate", bytesPerSecond === null ? "-" : `${((bytesPerSecond * 8) / 1000).toFixed(0)} kbit/s`],
  ]);
  renderList($("#stats"), [
    ["resolution", stats.width ? `${stats.width}×${stats.height}` : "-"],
    ["fps", stats.fps.toFixed(1)],
    ["codec", String(codec?.mimeType ?? "-")],
    ["decoder", stats.decoder],
    count("decoded", "rtc.decoded", decoded),
    count("dropped", "rtc.dropped", number("framesDropped"), true),
    count("key frames", "rtc.keyframes", number("keyFramesDecoded")),
    ["decode time", decoded ? `${((number("totalDecodeTime") / decoded) * 1000).toFixed(2)} ms/frame` : "-"],
    ["jitter buffer", emitted ? `${((number("jitterBufferDelay") / emitted) * 1000).toFixed(0)} ms` : "-"],
    ["network jitter", `${(number("jitter") * 1000).toFixed(1)} ms`],
    count("packets", "rtc.packets", number("packetsReceived")),
    count("packets lost", "rtc.lost", number("packetsLost"), true),
    count("NACK / PLI", "rtc.nack", number("nackCount") + number("pliCount"), true),
    count("freezes", "rtc.freezes", number("freezeCount"), true),
  ]);
}

/**
 * `?transport=tcp` drops UDP candidates from both descriptions, so neither side can
 * find a UDP path: what a viewer behind an SSH port forward gets, proven end to end.
 */
function candidates(sdp: string): string {
  if (params.get("transport") !== "tcp") return sdp;
  return sdp.split("\r\n").filter((line) => !/^a=candidate:\S+ \d+ udp /i.test(line)).join("\r\n");
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

async function pollMonitor(): Promise<void> {
  const client = monitor.client;
  if (client === null) return;
  try {
    const status = await client.getMonitorStatus();
    if (monitor.client !== client) return;
    showMonitor(status);
    await pollStream();
  } catch (error) {
    if (error instanceof RpcError) log(`monitor: ${error.message}`);
    else monitor.lost(client, (error as Error).message);
  }
}

// ---------------------------------------------------------------------------------------

function showLinks(): void {
  const since = (link: Link) => (link.client === null ? "-" : duration(Date.now() - link.since));
  renderList($("#links"), [
    ["server up for", since(server)],
    ["server RTT", server.client === null ? "-" : `${serverRoundTripMs.toFixed(1)} ms`],
    ["monitor up for", since(monitor)],
  ]);
}

async function poll(): Promise<void> {
  await Promise.all([pollServer(), pollMonitor()]);
  showLinks();
}

streamButton.onclick = () => setStreamWanted(!streamWanted);
setInterval(() => void poll(), POLL_MS);
closeStream();
showLinks();

if (params.get("auto") === "1") {
  void (async () => {
    connectButton.click();
    setStreamWanted(true);
    const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));
    const connected = () => stats.state === "connected";
    for (let waited = 0; !connected() && waited < 15000; waited += 100) {
      await sleep(100);
      await pollStream();
    }
    await sleep(Number(params.get("seconds") ?? 4) * 1000);
    await poll();
    const result = $("#result");
    result.textContent = JSON.stringify([stats]);
    result.hidden = false;
  })();
}
