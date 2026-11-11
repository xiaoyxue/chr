import assert from "node:assert/strict";
import { test } from "node:test";
import { H264Depacketizer } from "./h264.ts";
import { FrameDecoder, encodeFrame } from "./native-socket.ts";

const sps = Buffer.from([0x67, 1, 2]);
const pps = Buffer.from([0x68, 3]);
const idr = Buffer.from([0x65, 9, 8, 7, 6, 5]);

test("reassembles STAP-A and FU-A into one Annex B keyframe", () => {
  const depacketizer = new H264Depacketizer();
  const stapA = Buffer.concat([
    Buffer.from([0x78]),
    Buffer.from([0, sps.length]),
    sps,
    Buffer.from([0, pps.length]),
    pps,
  ]);
  assert.equal(depacketizer.push(10, stapA, false), null);
  // FU indicator keeps NRI (0x60) with type 28; FU header carries S/E bits and type 5.
  assert.equal(depacketizer.push(11, Buffer.from([0x7c, 0x85, 9, 8, 7]), false), null);
  const unit = depacketizer.push(12, Buffer.from([0x7c, 0x45, 6, 5]), true);
  assert.ok(unit);
  assert.deepEqual(unit.nalTypes, [7, 8, 5]);
  assert.equal(unit.keyframe, true);
  const start = Buffer.from([0, 0, 0, 1]);
  assert.deepEqual(unit.data, Buffer.concat([start, sps, start, pps, start, idr]));
});

test("drops access units with missing packets and recovers", () => {
  const depacketizer = new H264Depacketizer();
  assert.equal(depacketizer.push(1, Buffer.from([0x7c, 0x85, 1]), false), null);
  // Sequence 2 is lost.
  assert.equal(depacketizer.push(3, Buffer.from([0x7c, 0x45, 2]), true), null);
  assert.equal(depacketizer.dropped, 1);
  const next = depacketizer.push(4, Buffer.from([0x41, 1, 2]), true);
  assert.deepEqual(next?.nalTypes, [1]);
  assert.equal(next?.keyframe, false);
});

test("native socket frames round-trip across arbitrary chunk boundaries", () => {
  const bytes = Buffer.concat([encodeFrame('{"a":1}'), encodeFrame(""), encodeFrame("é")]);
  const decoder = new FrameDecoder();
  const messages = [...bytes].flatMap((byte) => decoder.push(Buffer.from([byte])));
  assert.deepEqual(messages, ['{"a":1}', "", "é"]);
  const oversized = Buffer.alloc(4);
  oversized.writeUInt32BE((1 << 20) + 1);
  assert.throws(() => new FrameDecoder().push(oversized));
});
