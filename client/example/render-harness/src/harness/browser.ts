// Real-browser check: drives a local headless Chrome over the DevTools protocol, opens the
// program monitor in auto mode, and checks that Chrome decoded the WebRTC stream.
// Needs the viewer (`npm run dev`) and chr-server running. `npm run verify:browser -- --help`.

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const HELP = `Usage: npm run verify:browser -- [options]

  --url <url>         Viewer URL               [default http://localhost:5173/]
  --chrome <path>     Chrome binary            [env CHROME, default google-chrome]
  --seconds <n>       Rotation time to observe [default 4]
  --fps <n>           Server frame rate        [default 50]
  --tcp-only          Forbid UDP in Chrome, as through an SSH port forward
                      (needs chr-server --webrtc-tcp)
  --pitch-controls <json> Load owner-local native-written pitch metadata through the file picker
  --screenshot <png>  Save a screenshot        [default <repo>/output/render-harness/browser.png]`;

const { values: args } = parseArgs({
  options: {
    url: { type: "string", default: "http://localhost:5173/" },
    chrome: { type: "string", default: process.env.CHROME ?? "google-chrome" },
    seconds: { type: "string", default: "4" },
    fps: { type: "string", default: "50" },
    "tcp-only": { type: "boolean", default: false },
    "pitch-controls": { type: "string" },
    screenshot: {
      type: "string",
      default: fileURLToPath(new URL("../../../../../output/render-harness/browser.png", import.meta.url)),
    },
    help: { type: "boolean", short: "h", default: false },
  },
});

interface PreviewStats {
  source: string;
  state: string;
  framesDecoded: number;
  fps: number;
  width: number;
  height: number;
  decoder: string;
}

/** Minimal DevTools protocol client over the browser's WebSocket endpoint. */
class Cdp {
  readonly #socket: WebSocket;
  readonly #pending = new Map<number, { resolve(value: unknown): void; reject(error: Error): void }>();
  #nextId = 1;
  /** Console output and uncaught exceptions from the page, for failure reports. */
  readonly pageLog: string[] = [];

  private constructor(socket: WebSocket) {
    this.#socket = socket;
    socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as {
        id?: number;
        method?: string;
        params?: Record<string, unknown>;
        result?: unknown;
        error?: { message: string };
      };
      if (message.method === "Runtime.consoleAPICalled") {
        const values = (message.params?.args as { value?: unknown; description?: string }[]) ?? [];
        this.pageLog.push(`console.${message.params?.type}: ${values.map((arg) => arg.value ?? arg.description).join(" ")}`);
      } else if (message.method === "Runtime.exceptionThrown") {
        const details = message.params?.exceptionDetails as { exception?: { description?: string }; text?: string };
        this.pageLog.push(`exception: ${details?.exception?.description ?? details?.text}`);
      }
      const pending = message.id === undefined ? undefined : this.#pending.get(message.id);
      if (pending === undefined) return;
      this.#pending.delete(message.id as number);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    };
  }

  static connect(url: string): Promise<Cdp> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.onopen = () => resolve(new Cdp(socket));
      socket.onerror = () => reject(new Error(`cannot connect to ${url}`));
    });
  }

  send<T>(method: string, params: object = {}, sessionId?: string): Promise<T> {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      this.#socket.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }

  close(): void {
    this.#socket.close();
  }
}

function launchChrome(profile: string): Promise<{ endpoint: string; stop(): Promise<void> }> {
  const flags = [
    "--headless=new",
    "--remote-debugging-port=0",
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--autoplay-policy=no-user-gesture-required",
    "--window-size=1400,1000",
  ];
  if (args["tcp-only"]) flags.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp");
  const chrome = spawn(args.chrome, flags, { stdio: ["ignore", "ignore", "pipe"] });
  const exited = new Promise<void>((done) => chrome.once("exit", () => done()));
  const stop = async () => {
    chrome.kill("SIGTERM");
    await exited;
  };
  return new Promise((resolve, reject) => {
    let stderr = "";
    let started = false;
    const timer = setTimeout(() => {
      chrome.kill("SIGTERM");
      reject(new Error(`Chrome did not start: ${stderr}`));
    }, 15_000);
    chrome.on("error", (error) => { clearTimeout(timer); reject(error); });
    chrome.once("exit", (code, signal) => {
      if (!started) {
        clearTimeout(timer);
        reject(new Error(`Chrome exited before readiness (${code ?? signal}): ${stderr}`));
      }
    });
    // Keep draining stderr after startup so Chrome never blocks on a full pipe.
    chrome.stderr.on("data", (chunk: Buffer) => {
      if (started) return;
      stderr += chunk.toString();
      const match = /DevTools listening on (ws:\/\/\S+)/.exec(stderr);
      if (match?.[1]) {
        started = true;
        clearTimeout(timer);
        resolve({ endpoint: match[1], stop });
      }
    });
  });
}

async function main(): Promise<number> {
  if (args.help) {
    console.log(HELP);
    return 0;
  }
  const url = new URL(args.url);
  url.searchParams.set("auto", "1");
  url.searchParams.set("seconds", args.seconds);
  if (args["pitch-controls"]) {
    url.searchParams.set("program", "1");
    url.searchParams.set("pitch", "1");
  }
  const profile = mkdtempSync(join(tmpdir(), "chr-chrome-"));
  let chrome: Awaited<ReturnType<typeof launchChrome>>;
  try {
    chrome = await launchChrome(profile);
  } catch (error) {
    rmSync(profile, { recursive: true, force: true, maxRetries: 5 });
    throw error;
  }
  const cdp = await Cdp.connect(chrome.endpoint);
  try {
    // Attach to a blank page first so console output from the very start is captured.
    const { targetId } = await cdp.send<{ targetId: string }>("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await cdp.send<{ sessionId: string }>("Target.attachToTarget", {
      targetId,
      flatten: true,
    });
    await cdp.send("Runtime.enable", {}, sessionId);
    await cdp.send("Page.navigate", { url: url.href }, sessionId);
    if (args["pitch-controls"]) {
      await cdp.send("DOM.enable", {}, sessionId);
      const ready = Date.now() + 15000;
      let nodeId = 0;
      while (nodeId === 0 && Date.now() < ready) {
        await new Promise(done => setTimeout(done, 100));
        const doc = await cdp.send<{ root: { nodeId: number } }>("DOM.getDocument", {}, sessionId);
        const found = await cdp.send<{ nodeId: number }>("DOM.querySelector", { nodeId: doc.root.nodeId, selector: "#pitch-file" }, sessionId);
        nodeId = found.nodeId;
      }
      if (!nodeId) throw new Error("Pitch file picker did not become ready");
      await cdp.send("DOM.setFileInputFiles", { nodeId, files: [resolve(args["pitch-controls"])] }, sessionId);
    }
    const deadline = Date.now() + (Number(args.seconds) + (url.searchParams.get("program") === "1" ? 60 : 20)) * 1000;
    let report = "";
    while (Date.now() < deadline && report === "") {
      await new Promise((done) => setTimeout(done, 500));
      const evaluated = await cdp.send<{ result: { value?: string } }>(
        "Runtime.evaluate",
        { expression: "document.querySelector('#result')?.textContent ?? ''", returnByValue: true },
        sessionId,
      );
      report = evaluated.result.value ?? "";
    }
    const shot = await cdp.send<{ data: string }>("Page.captureScreenshot", { format: "png" }, sessionId);
    const screenshot = resolve(args.screenshot);
    mkdirSync(dirname(screenshot), { recursive: true });
    writeFileSync(screenshot, Buffer.from(shot.data, "base64"));
    if (report === "") {
      console.log(`FAIL  viewer produced no report (screenshot ${screenshot})`);
      for (const line of cdp.pageLog) console.log(`      ${line}`);
      return 1;
    }

    const fps = Number(args.fps);
    let failed = 0;
    if (url.searchParams.get("program") === "1") {
      const evaluated = await cdp.send<{ result: { value?: string } }>("Runtime.evaluate", {
        expression: "document.querySelector('#program-result')?.textContent ?? ''", returnByValue: true,
      }, sessionId);
      const program = JSON.parse(evaluated.result.value || "{}") as { passed?: boolean };
      console.log(`${program.passed ? "PASS" : "FAIL"}  program controls: ${evaluated.result.value}`);
      if (!program.passed) failed++;
    }
    for (const stats of JSON.parse(report) as PreviewStats[]) {
      const problems = [
        stats.state !== "connected" && `state ${stats.state}`,
        stats.framesDecoded < fps * Number(args.seconds) * 0.75 && `only ${stats.framesDecoded} frames decoded`,
        stats.width === 0 && "no video size",
      ].filter(Boolean);
      const summary = `${stats.width}x${stats.height}, ${stats.fps} fps, ${stats.framesDecoded} decoded by ${stats.decoder}`;
      console.log(`${problems.length ? "FAIL" : "PASS"}  chrome[${stats.source}]: ${summary}${problems.length ? ` (${problems.join(", ")})` : ""}`);
      if (problems.length) failed += 1;
    }
    console.log(`screenshot: ${screenshot}`);
    return failed === 0 ? 0 : 1;
  } finally {
    cdp.close();
    await chrome.stop();
    rmSync(profile, { recursive: true, force: true, maxRetries: 5 });
  }
}

process.exit(await main());
