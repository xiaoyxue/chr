// NativeSocket (Unix Domain Socket) transport for Node.js. Each message is a 4-byte
// big-endian payload length followed by UTF-8 JSON, matching `common/src/framing.rs`.

import { createConnection } from "node:net";
import type { MessageTransport } from "../rpc.ts";

export const MAX_MESSAGE_BYTES = 1 << 20;

export function encodeFrame(message: string): Buffer {
  const payload = Buffer.from(message, "utf8");
  if (payload.length > MAX_MESSAGE_BYTES) {
    throw new Error(`message of ${payload.length} bytes exceeds ${MAX_MESSAGE_BYTES}`);
  }
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.length);
  return Buffer.concat([header, payload]);
}

/** Splits a byte stream into frames; keeps partial frames until they complete. */
export class FrameDecoder {
  #buffer = Buffer.alloc(0);

  push(chunk: Buffer): string[] {
    this.#buffer = Buffer.concat([this.#buffer, chunk]);
    const messages: string[] = [];
    while (this.#buffer.length >= 4) {
      const length = this.#buffer.readUInt32BE(0);
      if (length > MAX_MESSAGE_BYTES) {
        throw new Error(`frame of ${length} bytes exceeds ${MAX_MESSAGE_BYTES}`);
      }
      if (this.#buffer.length < 4 + length) break;
      messages.push(this.#buffer.subarray(4, 4 + length).toString("utf8"));
      this.#buffer = this.#buffer.subarray(4 + length);
    }
    return messages;
  }
}

export function connectNativeSocket(path: string): Promise<MessageTransport> {
  return new Promise((resolve, reject) => {
    const socket = createConnection(path);
    const decoder = new FrameDecoder();
    const transport: MessageTransport = {
      send: (message) => socket.write(encodeFrame(message)),
      close: () => socket.end(),
      onMessage: null,
      onClose: null,
    };
    socket.once("connect", () => resolve(transport));
    socket.once("error", (error) => reject(error));
    socket.on("data", (chunk: Buffer) => {
      try {
        for (const message of decoder.push(chunk)) transport.onMessage?.(message);
      } catch (error) {
        socket.destroy(error as Error);
      }
    });
    socket.on("close", () => transport.onClose?.("socket closed"));
  });
}
