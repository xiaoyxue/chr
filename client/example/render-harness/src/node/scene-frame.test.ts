import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  SceneFrameSchemas, SceneFrameMethods, parseSceneFrameParams, parseSceneFrameResult,
  type SceneFrameMethod,
} from "../scene-frame.ts";

interface Call { name: string; method: SceneFrameMethod; params: unknown; result: unknown }
interface Change { name: string; base: string; path: string; remove?: boolean; duplicate?: boolean; value?: unknown }
const fixtures = JSON.parse(readFileSync(
  new URL("../../../../../common/tests/fixtures/scene_frame.json", import.meta.url), "utf8",
)) as {
  calls: Call[]; valid_changes: (Change & { side: "params" | "result" })[]; invalid_params: Change[]; invalid_results: Change[];
  scalars: { u64_valid: unknown[]; u64_invalid: unknown[]; time_valid: unknown[]; time_invalid: unknown[] };
};
function changed(change: Change, side: "params" | "result"): { method: SceneFrameMethod; value: unknown } {
  const call = fixtures.calls.find(call => call.name === change.base)!;
  if (change.path === "") return { method: call.method, value: change.value };
  const value: unknown = structuredClone(call[side]);
  const keys = change.path.slice(1).split("/");
  const field = keys.pop()!;
  let parent = value as Record<string, unknown>;
  for (const key of keys) parent = parent[key] as Record<string, unknown>;
  if (change.duplicate) {
    const list = parent[field] as unknown[];
    list.push(structuredClone(list[0]));
  } else if (change.remove) delete parent[field];
  else parent[field] = change.value;
  return { method: call.method, value };
}
for (const positive of fixtures.valid_changes) {
  const base = structuredClone(fixtures.calls.find(call => call.name === positive.base)!);
  base[positive.side] = changed(positive, positive.side).value;
  base.name = positive.name;
  fixtures.calls.push(base);
}

test("Rust shared fixtures round trip exact params and results", () => {
  for (const call of fixtures.calls) {
    assert.deepEqual(parseSceneFrameParams(call.method, call.params), call.params, call.name);
    assert.deepEqual(parseSceneFrameResult(call.method, call.result), call.result, call.name);
  }
  for (const method of Object.keys(SceneFrameMethods) as SceneFrameMethod[]) {
    for (const invalid of [undefined, null, []]) {
      assert.throws(() => parseSceneFrameParams(method, invalid));
    }
  }
});
test("Rust shared negative fixtures reject stale, omitted nullable and excluded fields", () => {
  for (const change of fixtures.invalid_params) {
    const { method, value } = changed(change, "params");
    assert.throws(() => parseSceneFrameParams(method, value), change.name);
  }
  for (const change of fixtures.invalid_results) {
    const { method, value } = changed(change, "result");
    assert.throws(() => parseSceneFrameResult(method, value), change.name);
  }
});
test("Rust shared decimal U64 and reduced time boundaries", () => {
  for (const value of fixtures.scalars.u64_valid) assert.equal(SceneFrameSchemas.u64(value), value);
  for (const value of fixtures.scalars.u64_invalid) assert.throws(() => SceneFrameSchemas.u64(value));
  for (const value of fixtures.scalars.time_valid) assert.deepEqual(SceneFrameSchemas.timeBase(value), value);
  for (const value of fixtures.scalars.time_invalid) assert.throws(() => SceneFrameSchemas.timeBase(value));
  for (const number of [NaN, Infinity, -Infinity]) {
    const { method, value } = changed({ base: "create", name: "finite", path: "/state/camera/camera/near_m", value: number }, "params");
    assert.throws(() => parseSceneFrameParams(method, value));
  }
});
test("error kind/code coupling and required nullable frame", () => {
  const kinds = ["runtime_changed", "not_found", "conflict", "incompatible_input", "capacity", "invalid_state", "unsupported_profile"];
  for (const [index, kind] of kinds.entries()) {
    const error = { code: 2001 + index, message: "Synthetic rejection", data: { kind, frame: null } };
    assert.deepEqual(SceneFrameSchemas.rejection(error), error);
    assert.throws(() => SceneFrameSchemas.rejection({ ...error, code: 2000 }));
    assert.throws(() => SceneFrameSchemas.rejection({ ...error, data: { kind } }));
  }
});
