import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SceneControls, editCamera, type SceneRpc } from "../scene-controls.ts";
import { parseSceneFrameResult, type RenderSceneSnapshot } from "../scene-frame.ts";
import { advanceRotation, rotateModelZ } from "../object-rotation.ts";

const fixtures = JSON.parse(readFileSync(new URL("../../../../../common/tests/fixtures/scene_frame.json", import.meta.url), "utf8"));
const create = fixtures.calls.find((call: { name: string }) => call.name === "create").params;
const snapshot = (): RenderSceneSnapshot => parseSceneFrameResult("scene.get", {
  runtime_id: create.runtime_id, scene_id: create.scene_id, scene_revision: "0",
  effect: { runtime_id: create.runtime_id, sequence: "0" }, state: structuredClone(create.state),
});

test("zoom preserves native principal point; dolly moves along pose forward; orbit round-trips", () => {
  const mode = snapshot().state.camera;
  assert.equal(mode.mode, "manual");
  if (mode.mode !== "manual" || mode.camera.kind !== "intrinsics_pose") throw new Error("fixture");
  const camera = mode.camera;
  const zoom = editCamera(camera, "zoom-in");
  assert.equal(zoom.intrinsics.cx, 640);
  assert.equal(zoom.intrinsics.cy, 360);
  assert.equal(zoom.intrinsics.fx, 1100);
  assert.equal(editCamera(camera, "dolly-in").camera_to_world[2][3], -9.6);
  let rotated = editCamera(camera, "orbit-up");
  assert.notEqual(rotated.camera_to_world[1][3], 0);
  rotated = editCamera(rotated, "orbit-down");
  for (let row = 0; row < 4; row++) for (let col = 0; col < 4; col++) {
    assert.ok(Math.abs(rotated.camera_to_world[row]![col]! - camera.camera_to_world[row]![col]!) < 1e-10);
  }
  assert.deepEqual(camera.camera_to_world[2], [0, 0, 1, -10], "original immutable");
});

test("serialized mutations query current revision; uncertain reply reconciles but never retries", async () => {
  let state = snapshot();
  const revisions: string[] = [];
  let loseReply = false;
  const rpc: SceneRpc = {
    async call(method, params) {
      if (method === "scene.get") return structuredClone(state);
      assert.equal(method, "scene.mutate");
      const request = params as { expected_revision: string };
      revisions.push(request.expected_revision);
      assert.equal(request.expected_revision, state.scene_revision);
      state.scene_revision = String(BigInt(state.scene_revision) + 1n);
      if (loseReply) throw new Error("reply lost");
      const { state: ignored, ...applied } = state;
      return structuredClone(applied);
    },
  };
  const editor = new SceneControls(rpc, { runtime_id: state.runtime_id, scene_id: state.scene_id });
  await editor.load();
  const mutate = () => editor.mutate(() => [{ kind: "shadow_mode_set", shadow_mode: "off" }]);
  await Promise.all([mutate(), mutate()]);
  assert.deepEqual(revisions, ["0", "1"]);
  loseReply = true;
  await assert.rejects(mutate(), /reply lost/);
  assert.equal(editor.snapshot?.scene_revision, "3");
  await assert.rejects(mutate(), /Reload/);
  assert.equal(revisions.length, 3);
  loseReply = false;
  await editor.load();
  await mutate();
  assert.equal(revisions[3], "3");
});

test("mesh rotation preserves translation/height and reverses without accumulating pose drift", () => {
  const model = snapshot().state.instances[0]!.model;
  const base = structuredClone(model);
  base[0][0] = 0.25; base[1][1] = 0.25; base[2][2] = 0.25;
  base[0][3] = 4; base[1][3] = 7; base[2][3] = -1;
  const rotated = rotateModelZ(base, Math.PI / 2);
  assert.deepEqual(rotated.map(row => row[3]), [4, 7, -1, 1]);
  assert.equal(rotated[2][2], 0.25);
  assert.ok(rotated[1][0] > 0);
  const restored = rotateModelZ(rotated, -Math.PI / 2);
  for (let row = 0; row < 4; row++) for (let col = 0; col < 4; col++) {
    assert.ok(Math.abs(restored[row]![col]! - base[row]![col]!) < 1e-12);
  }
  assert.deepEqual(model, snapshot().state.instances[0]!.model);
});

test("rotation phase follows output-slot gaps, pauses and signed speed rather than timer calls", () => {
  const rate = { numerator: 1, denominator: 30 };
  const forward = advanceRotation(0, Math.PI, 7, 22, rate);
  assert.ok(Math.abs(forward - Math.PI / 2) < 1e-12);
  assert.equal(advanceRotation(forward, Math.PI, 22, 22, rate), forward);
  assert.ok(advanceRotation(forward, -Math.PI, 22, 37, rate) < 1e-12);
  const intermediate = advanceRotation(0, Math.PI, 7, 10, rate);
  assert.ok(Math.abs(advanceRotation(intermediate, Math.PI, 10, 22, rate) - forward) < 1e-12);
  assert.throws(() => advanceRotation(0, 1, 8, 7, rate), /timeline/);
});
