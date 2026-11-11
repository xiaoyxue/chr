// Transport-agnostic JSON-RPC 2.0 client. Runs in browsers and in Node.js.

import {
  JSONRPC_VERSION,
  Methods,
  type RpcErrorObject,
  type RpcRequest,
  type RpcResponse,
  type SdiStatus,
  type SessionDescription,
  type StartRotationParams,
  type TriangleState,
  isTriangleState,
} from "./protocol.ts";

/** Carries whole JSON-RPC messages; framing is the transport's job. */
export interface MessageTransport {
  send(message: string): void;
  close(): void;
  onMessage: ((message: string) => void) | null;
  onClose: ((reason: string) => void) | null;
}

export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;

  constructor(error: RpcErrorObject) {
    super(`JSON-RPC error ${error.code}: ${error.message}`);
    this.name = "RpcError";
    this.code = error.code;
    this.data = error.data;
  }
}

interface Pending {
  resolve(result: unknown): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

export class RpcClient {
  readonly #transport: MessageTransport;
  readonly #prefix: string;
  readonly #timeoutMs: number;
  readonly #pending = new Map<string, Pending>();
  #nextId = 1;
  #closed: string | null = null;

  constructor(transport: MessageTransport, options: { idPrefix?: string; timeoutMs?: number } = {}) {
    this.#transport = transport;
    this.#prefix = options.idPrefix ?? "req";
    this.#timeoutMs = options.timeoutMs ?? 10_000;
    transport.onMessage = (message) => this.#receive(message);
    transport.onClose = (reason) => this.#fail(reason);
  }

  call(method: string, params?: unknown): Promise<unknown> {
    if (this.#closed !== null) {
      return Promise.reject(new Error(`connection closed: ${this.#closed}`));
    }
    const id = `${this.#prefix}-${this.#nextId++}`;
    const request: RpcRequest = { jsonrpc: JSONRPC_VERSION, method, id };
    if (params !== undefined) request.params = params;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`${method} timed out after ${this.#timeoutMs} ms`));
      }, this.#timeoutMs);
      this.#pending.set(id, { resolve, reject, timer });
      this.#transport.send(JSON.stringify(request));
    });
  }

  /** Fire-and-forget: the server executes it but never replies. */
  notify(method: string, params?: unknown): void {
    const request: RpcRequest = { jsonrpc: JSONRPC_VERSION, method };
    if (params !== undefined) request.params = params;
    this.#transport.send(JSON.stringify(request));
  }

  close(): void {
    this.#transport.close();
    this.#fail("closed by client");
  }

  async startRotation(params: StartRotationParams = {}): Promise<TriangleState> {
    return expectState(await this.call(Methods.triangle.startRotation, params));
  }

  async stopRotation(): Promise<TriangleState> {
    return expectState(await this.call(Methods.triangle.stopRotation));
  }

  async getState(): Promise<TriangleState> {
    return expectState(await this.call(Methods.triangle.getState));
  }

  async getSdiStatus(): Promise<SdiStatus> {
    return (await this.call(Methods.sdi.getStatus)) as SdiStatus;
  }

  /** Negotiates a receive-only stream of the program (SDI) output. */
  async offer(offer: SessionDescription): Promise<SessionDescription> {
    const answer = (await this.call(Methods.webrtc.offer, offer)) as SessionDescription;
    if (answer?.type !== "answer" || typeof answer.sdp !== "string") {
      throw new Error("webrtc.offer did not return an answer");
    }
    return answer;
  }

  #receive(message: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(message);
    } catch {
      console.warn("ignoring non-JSON message", message);
      return;
    }
    for (const response of Array.isArray(parsed) ? parsed : [parsed]) {
      const { id } = response as RpcResponse;
      const pending = typeof id === "string" ? this.#pending.get(id) : undefined;
      if (pending === undefined) {
        console.warn("unmatched response", response);
        continue;
      }
      this.#pending.delete(id as string);
      clearTimeout(pending.timer);
      if ("error" in response) {
        pending.reject(new RpcError((response as { error: RpcErrorObject }).error));
      } else {
        pending.resolve((response as { result: unknown }).result);
      }
    }
  }

  #fail(reason: string): void {
    this.#closed ??= reason;
    for (const [id, pending] of this.#pending) {
      clearTimeout(pending.timer);
      pending.reject(new Error(`connection closed: ${reason}`));
      this.#pending.delete(id);
    }
  }
}

function expectState(result: unknown): TriangleState {
  if (!isTriangleState(result)) {
    throw new Error(`unexpected triangle state: ${JSON.stringify(result)}`);
  }
  return result;
}

/** One JSON-RPC message per text frame. Uses the global WebSocket (browsers, Node ≥ 22). */
export function connectWebSocket(url: string): Promise<MessageTransport> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const transport: MessageTransport = {
      send: (message) => socket.send(message),
      close: () => socket.close(),
      onMessage: null,
      onClose: null,
    };
    socket.onopen = () => resolve(transport);
    socket.onerror = () => reject(new Error(`cannot connect to ${url}`));
    socket.onmessage = (event) => {
      if (typeof event.data === "string") transport.onMessage?.(event.data);
    };
    socket.onclose = (event) => transport.onClose?.(event.reason || `code ${event.code}`);
  });
}
