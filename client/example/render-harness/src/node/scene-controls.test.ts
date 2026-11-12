import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SceneControls, editCamera, type SceneRpc } from "../scene-controls.ts";
import { parseSceneFrameResult, type RenderSceneSnapshot } from "../scene-frame.ts";
import { ObjectRotation } from "../object-rotation.ts";

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

test("mesh controller sends one command per action and displays server-owned phase", async () => {
  const scene = snapshot();
  const target = { runtime_id: scene.runtime_id, scene_id: scene.scene_id, instance_id: "mesh" };
  const calls: string[] = [];
  const state = { ...target, rotating: false, angle_rad: 1.25, angular_velocity_rad_per_s: 0, frame: 7 };
  const rpc: SceneRpc = {
    async call(method, params) {
      calls.push(method);
      if (method === "mesh.start_rotation") {
        state.rotating = true;
        state.angular_velocity_rad_per_s = (params as { angular_velocity_rad_per_s: number }).angular_velocity_rad_per_s;
      } else if (method === "mesh.stop_rotation") state.rotating = false;
      else assert.equal(method, "mesh.get_rotation", "no page-driven scene mutations");
      assert.deepEqual({ ...params as object, angular_velocity_rad_per_s: undefined },
        { ...target, angular_velocity_rad_per_s: undefined });
      return structuredClone(state);
    },
  };
  const rotation = new ObjectRotation(new SceneControls(rpc,
    { runtime_id: target.runtime_id, scene_id: target.scene_id }), () => {});
  await rotation.start(Math.PI);
  assert.equal(rotation.running, true);
  state.angle_rad = 2;
  state.frame = 22;
  await rotation.refresh();
  assert.equal(rotation.angle, 2);
  await rotation.start(-Math.PI);
  assert.equal(rotation.angle, 2, "direction change does not reset the server pose");
  await rotation.stop();
  assert.equal(rotation.running, false);
  assert.deepEqual(calls, ["mesh.start_rotation", "mesh.get_rotation", "mesh.start_rotation", "mesh.stop_rotation"]);
  await assert.rejects(rotation.start(NaN), /finite/);
});
