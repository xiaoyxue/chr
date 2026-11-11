// End-to-end verification of a running chr-server: JSON-RPC over WebSocket and
// NativeSocket, the simulated SDI output, and the WebRTC stream of that output
// (render -> SDI (UYVY) -> encoder (H.264) -> WebRTC). Run with `npm run verify -- --help`.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { RTCPeerConnection, RTCRtpCodecParameters } from "werift";
import { type AccessUnit, H264Depacketizer } from "../node/h264.ts";
import { connectNativeSocket } from "../node/native-socket.ts";
import { ErrorCode, Methods } from "../protocol.ts";
import { RpcClient, RpcError, connectWebSocket } from "../rpc.ts";

const HELP = `Usage: npm run verify -- [options]

  --ws <url>          WebSocket endpoint      [env CHR_WS_URL, default ws://127.0.0.1:8787]
  --socket <path>     NativeSocket path       [env CHR_SOCKET, default $XDG_RUNTIME_DIR/chr/render.sock]
  --fps <n>           Server frame rate       [default 50]
  --seconds <n>       Capture time per phase  [default 2]
  --out <dir>         Captured H.264 output   [default <repo>/output/render-harness]
  --skip-native       Skip NativeSocket checks
  --skip-webrtc       Skip WebRTC checks`;

const { values: args } = parseArgs({
  options: {
    ws: { type: "string", default: process.env.CHR_WS_URL ?? "ws://127.0.0.1:8787" },
    socket: { type: "string", default: process.env.CHR_SOCKET ?? defaultSocketPath() },
    fps: { type: "string", default: "50" },
    seconds: { type: "string", default: "2" },
    out: { type: "string", default: fileURLToPath(new URL("../../../../../output/render-harness", import.meta.url)) },
    "skip-native": { type: "boolean", default: false },
    "skip-webrtc": { type: "boolean", default: false },
    help: { type: "boolean", short: "h", default: false },
  },
});

function defaultSocketPath(): string {
  const runtime = process.env.XDG_RUNTIME_DIR;
  return runtime ? join(runtime, "chr", "render.sock") : "";
}

const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));
const TAU = 2 * Math.PI;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function expectRpcError(promise: Promise<unknown>, code: number): Promise<void> {
  try {
    await promise;
  } catch (error) {
    assert(error instanceof RpcError, `expected RpcError, got ${String(error)}`);
    assert(error.code === code, `expected error ${code}, got ${error.code}: ${error.message}`);
    return;
  }
  throw new Error(`expected error ${code}, but the call succeeded`);
}

type Outcome = "PASS" | "FAIL" | "SKIP";
const results: { name: string; outcome: Outcome; detail: string }[] = [];

async function check(name: string, run: () => Promise<string | void>): Promise<boolean> {
  try {
    const detail = (await run()) ?? "";
    results.push({ name, outcome: "PASS", detail });
    console.log(`PASS  ${name}${detail ? `  (${detail})` : ""}`);
    return true;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    results.push({ name, outcome: "FAIL", detail });
    console.log(`FAIL  ${name}  (${detail})`);
    return false;
  }
}

function skip(name: string, reason: string): void {
  results.push({ name, outcome: "SKIP", detail: reason });
  console.log(`SKIP  ${name}  (${reason})`);
}

// ---------------------------------------------------------------------------------------
// IPC over WebSocket

async function verifyWebSocket(ws: RpcClient): Promise<void> {
  await check("ws: get_state returns the triangle state", async () => {
    const state = await ws.getState();
    return `frame ${state.frame}, rotating ${state.rotating}`;
  });

  await check("ws: start_rotation turns the triangle in frame time", async () => {
    await ws.stopRotation();
    const started = await ws.startRotation({ angular_velocity_rad_per_s: Math.PI });
    assert(started.rotating, "start_rotation did not set rotating");
    assert(started.angular_velocity_rad_per_s === Math.PI, "velocity was not applied");
    await sleep(500);
    const later = await ws.getState();
    const frames = later.frame - started.frame;
    assert(frames > 0, "frame did not advance");
    const expected = (started.angle_rad + (Math.PI * frames) / Number(args.fps)) % TAU;
    const error = Math.abs(later.angle_rad - expected);
    assert(Math.min(error, TAU - error) < 1e-6, `angle ${later.angle_rad} != ${expected}`);
    return `${frames} frames, angle ${started.angle_rad.toFixed(3)} -> ${later.angle_rad.toFixed(3)} rad`;
  });

  await check("ws: stop_rotation freezes the angle", async () => {
    const stopped = await ws.stopRotation();
    assert(!stopped.rotating, "stop_rotation left rotating set");
    await sleep(300);
    const later = await ws.getState();
    assert(later.frame > stopped.frame, "frame did not advance");
    assert(later.angle_rad === stopped.angle_rad, "angle changed while stopped");
    return `angle held at ${stopped.angle_rad.toFixed(3)} rad`;
  });

  await check("ws: notification executes without a reply, in order", async () => {
    ws.notify(Methods.triangle.startRotation);
    // Same connection: the server runs messages in order, so this sees the notification.
    const state = await ws.getState();
    assert(state.rotating, "notification was not applied before the next request");
    await ws.stopRotation();
  });

  await check("ws: standard JSON-RPC errors", async () => {
    await expectRpcError(ws.call("triangle.spin"), ErrorCode.MethodNotFound);
    await expectRpcError(
      ws.call(Methods.triangle.startRotation, { angular_velocity_rad_per_s: "fast" }),
      ErrorCode.InvalidParams,
    );
    await expectRpcError(ws.call(Methods.triangle.stopRotation, { now: true }), ErrorCode.InvalidParams);
    await expectRpcError(ws.call(Methods.webrtc.offer, { type: "answer", sdp: "" }), ErrorCode.InvalidParams);
    const reply = await rawWebSocketExchange(args.ws, "{oops");
    assert(reply.error?.code === ErrorCode.ParseError, `expected parse error, got ${JSON.stringify(reply)}`);
    assert(reply.id === null, "parse errors must carry a null id");
  });
}

function rawWebSocketExchange(url: string, text: string): Promise<{ id?: unknown; error?: { code: number } }> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const timer = setTimeout(() => reject(new Error("no reply to raw message")), 3000);
    socket.onopen = () => socket.send(text);
    socket.onerror = () => reject(new Error(`cannot connect to ${url}`));
    socket.onmessage = (event) => {
      clearTimeout(timer);
      socket.close();
      resolve(JSON.parse(String(event.data)));
    };
  });
}

// ---------------------------------------------------------------------------------------
// IPC over NativeSocket

async function verifyNativeSocket(ws: RpcClient): Promise<void> {
  const name = "native: NativeSocket commands drive the same scene as WebSocket";
  if (args["skip-native"]) return skip(name, "--skip-native");
  if (!args.socket || !existsSync(args.socket)) {
    return skip(name, `no socket at ${args.socket || "(unset)"}`);
  }
  await check(name, async () => {
    const native = new RpcClient(await connectNativeSocket(args.socket), { idPrefix: "native" });
    try {
      const started = await native.startRotation();
      assert((await ws.getState()).rotating, "WebSocket client did not see the NativeSocket start");
      const stopped = await native.stopRotation();
      const seen = await ws.getState();
      assert(!seen.rotating, "WebSocket client did not see the NativeSocket stop");
      assert(seen.angle_rad === stopped.angle_rad, "clients disagree on the angle");
      await expectRpcError(native.call("triangle.spin"), ErrorCode.MethodNotFound);
      return `rotated from frame ${started.frame} to ${stopped.frame}`;
    } finally {
      native.close();
    }
  });
}

// ---------------------------------------------------------------------------------------
// WebRTC preview

interface Captured {
  unit: AccessUnit;
  phase: string;
  atMs: number;
}

// ---------------------------------------------------------------------------------------
// Simulated SDI output

async function verifySdi(ws: RpcClient): Promise<void> {
  await check("sdi: output ticks at the video mode cadence", async () => {
    const before = await ws.getSdiStatus();
    assert(before.pixel_format.startsWith("UYVY"), `unexpected format ${before.pixel_format}`);
    await sleep(1000);
    const after = await ws.getSdiStatus();
    const fps = Number(args.fps);
    const slots = after.frames_output - before.frames_output;
    assert(Math.abs(slots - fps) <= fps * 0.2, `${slots} slots in 1 s, expected about ${fps}`);
    assert(
      (after.last_source_frame ?? -1) > (before.last_source_frame ?? -1),
      "output did not advance to newer render frames",
    );
    return `${after.video_mode}, ${after.pixel_format}; ${slots} slots/s, ${after.repeated_frames} repeated, ${after.dropped_frames} dropped in total`;
  });
}

// ---------------------------------------------------------------------------------------
// WebRTC stream of the program output

async function verifyWebRtc(ws: RpcClient): Promise<void> {
  const names = {
    connect: "webrtc: offer/answer over WebSocket and ICE connects",
    stream: "webrtc: H.264 starts with an IDR frame and arrives at the frame rate",
    content: "webrtc: decoded video changes only while rotating",
  };
  if (args["skip-webrtc"]) {
    for (const name of Object.values(names)) skip(name, "--skip-webrtc");
    return;
  }

  const fps = Number(args.fps);
  const phaseMs = Number(args.seconds) * 1000;
  const captured: Captured[] = [];
  const depacketizer = new H264Depacketizer();
  let phase = "connecting";
  let firstKeyframe: () => void = () => {};
  const gotKeyframe = new Promise<void>((done) => (firstKeyframe = done));

  const pc = new RTCPeerConnection({
    codecs: {
      video: [
        new RTCRtpCodecParameters({
          mimeType: "video/H264",
          clockRate: 90000,
          rtcpFeedback: [{ type: "nack" }, { type: "nack", parameter: "pli" }],
          parameters: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
        }),
      ],
    },
  });
  const transceiver = pc.addTransceiver("video", { direction: "recvonly" });
  transceiver.onTrack.subscribe((track) => {
    track.onReceiveRtp.subscribe((rtp) => {
      const unit = depacketizer.push(rtp.header.sequenceNumber, rtp.payload, rtp.header.marker);
      if (unit === null) return;
      if (captured.length === 0 && !unit.keyframe) return;
      captured.push({ unit, phase, atMs: performance.now() });
      if (unit.keyframe) firstKeyframe();
    });
  });

  try {
    const connected = await check(names.connect, async () => {
      await ws.stopRotation();
      await pc.setLocalDescription(await pc.createOffer());
      if (pc.iceGatheringState !== "complete") {
        await pc.iceGatheringStateChange.watch((state) => state === "complete", 5000);
      }
      const offer = pc.localDescription;
      assert(offer, "no local description");
      const answer = await ws.offer({ type: "offer", sdp: offer.sdp });
      await pc.setRemoteDescription(answer);
      if (pc.connectionState !== "connected") {
        await pc.connectionStateChange.watch((state) => state === "connected", 10_000);
      }
      const candidates = answer.sdp.split("\n").filter((line) => line.startsWith("a=candidate")).length;
      return `${candidates} server candidates`;
    });
    if (!connected) {
      skip(names.stream, "not connected");
      skip(names.content, "not connected");
      return;
    }

    let rotatingStats = { frames: 0, ms: 0 };
    const streaming = await check(names.stream, async () => {
      await Promise.race([
        gotKeyframe,
        sleep(5000).then(() => {
          throw new Error("no keyframe within 5 s");
        }),
      ]);
      phase = "static";
      await sleep(phaseMs);
      phase = "rotating";
      const rotateStart = performance.now();
      await ws.startRotation({ angular_velocity_rad_per_s: Math.PI });
      await sleep(phaseMs);
      await ws.stopRotation();
      phase = "stopped";
      rotatingStats = {
        frames: captured.filter((item) => item.phase === "rotating").length,
        ms: performance.now() - rotateStart,
      };
      await sleep(500);
      const first = captured[0];
      assert(first?.unit.nalTypes.includes(7) && first.unit.nalTypes.includes(8), "first frame lacks SPS/PPS");
      const rate = (rotatingStats.frames * 1000) / rotatingStats.ms;
      assert(rate > fps * 0.75, `received ${rate.toFixed(1)} fps, expected about ${fps}`);
      return `${captured.length} access units, ${rate.toFixed(1)} fps, ${depacketizer.dropped} dropped`;
    });
    if (!streaming) {
      skip(names.content, "stream check failed");
      return;
    }

    const outDir = resolve(args.out);
    mkdirSync(outDir, { recursive: true });
    const file = join(outDir, "program.h264");
    writeFileSync(file, Buffer.concat(captured.map((item) => item.unit.data)));
    await check(names.content, async () => analyzeContent(file, captured));
  } finally {
    await pc.close();
  }
}

/** Decodes the capture with ffmpeg and compares frames within each phase. */
function analyzeContent(file: string, captured: Captured[]): string {
  const [width, height] = [64, 36];
  const decoded = spawnSync(
    "ffmpeg",
    ["-v", "error", "-i", file, "-vf", `scale=${width}:${height},format=gray`, "-f", "rawvideo", "-"],
    { maxBuffer: 256 << 20 },
  );
  if (decoded.error) throw new Error(`ffmpeg unavailable (${decoded.error.message}); capture at ${file}`);
  assert(decoded.status === 0, `ffmpeg failed: ${decoded.stderr.toString()}`);
  const frameBytes = width * height;
  const frameCount = Math.floor(decoded.stdout.length / frameBytes);
  assert(frameCount === captured.length, `decoded ${frameCount} frames from ${captured.length} access units`);
  const frame = (index: number) => decoded.stdout.subarray(index * frameBytes, (index + 1) * frameBytes);
  const diff = (a: number, b: number) => {
    const [x, y] = [frame(a), frame(b)];
    let sum = 0;
    for (let i = 0; i < frameBytes; i++) sum += Math.abs((x[i] ?? 0) - (y[i] ?? 0));
    return sum / frameBytes;
  };
  // Skip each phase's first 300 ms: commands and video take a moment to line up.
  const span = (phase: string) => {
    const indices = captured.flatMap((item, index) => (item.phase === phase ? [index] : []));
    const start = (captured[indices[0] ?? 0]?.atMs ?? 0) + 300;
    const settled = indices.filter((index) => (captured[index]?.atMs ?? 0) >= start);
    assert(settled.length >= 2, `too few ${phase} frames`);
    return diff(settled[0] ?? 0, settled.at(-1) ?? 0);
  };
  const still = span("static");
  const moving = span("rotating");
  assert(still < 1, `static frames differ by ${still.toFixed(2)} (expected < 1)`);
  assert(moving > 3, `rotating frames differ by ${moving.toFixed(2)} (expected > 3)`);
  return `${frameCount} frames decoded; mean |Δ| static ${still.toFixed(2)}, rotating ${moving.toFixed(2)}; ${file}`;
}

// ---------------------------------------------------------------------------------------

async function main(): Promise<number> {
  if (args.help) {
    console.log(HELP);
    return 0;
  }
  console.log(`chr render harness: ${args.ws}${args.socket ? `, ${args.socket}` : ""}\n`);
  let ws: RpcClient;
  try {
    ws = new RpcClient(await connectWebSocket(args.ws), { idPrefix: "ws" });
  } catch (error) {
    console.error(`cannot reach the server: ${(error as Error).message}. Is chr-server running?`);
    return 1;
  }
  try {
    await verifyWebSocket(ws);
    await verifyNativeSocket(ws);
    await verifySdi(ws);
    await verifyWebRtc(ws);
  } finally {
    ws.close();
  }
  const count = (outcome: Outcome) => results.filter((result) => result.outcome === outcome).length;
  console.log(`\n${count("PASS")} passed, ${count("FAIL")} failed, ${count("SKIP")} skipped`);
  return count("FAIL") === 0 ? 0 : 1;
}

// werift keeps timers alive after close, so exit explicitly.
process.exit(await main());
