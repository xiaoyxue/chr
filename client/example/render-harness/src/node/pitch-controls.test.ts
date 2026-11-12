import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { PitchControls, type RigEdit } from "../pitch-controls.ts";
import { SceneFrameSchemas as S } from "../scene-frame.ts";

const source = JSON.parse(readFileSync(new URL("../../../../../server/runtime/tests/fixtures/pitch-camera.json",import.meta.url),"utf8"));
const metadata = () => ({
  profile: "pitch-demo-v1", target: { runtime_id: "run",scene_id: "pitch" }, configuration_id: "pitch-config",
  source_frame: source.source_frame, center_semantics: source.center_semantics, ground_status: source.ground_status, pitch: source.pitch,
  original_camera: { kind: "ti_c", extent: {width:source.width,height:source.height},near_m:0.1,far_m:10000,ti:source.ti.flat(),camera_center:source.center },
  preview_camera: { kind: "intrinsics_pose",extent:{width:640,height:360},near_m:0.1,far_m:10000,
    intrinsics:{fx:480,fy:480,skew:0,cx:320,cy:180},camera_to_world:[[1,0,0,0],[0,1,0,0],[0,0,1,-100],[0,0,0,1]] },
  models:[{instance:{instance_id:"model",asset_ref:"asset",model:[[-3,0,0,10],[0,0,-2,4],[0,5,0,-5],[0,0,0,1]],visible:true,material_overrides:[]},
    bounds:{min:[-1,-1,-2],max:[1,1,0]}}],
});
const scene = () => S.sceneSnapshot({
  runtime_id:"run",scene_id:"pitch",scene_revision:"5",effect:{runtime_id:"run",sequence:"6"},
  state:{world:{id:"world",revision:"1"},configuration_id:"pitch-config",
    camera:{mode:"manual",camera:metadata().original_camera},instances:[],lighting:{lights:[],environment:null},shadow_mode:"pcf"},
});
const rig = (): RigEdit => ({
  rig:"spots",sun:true,height:35,targets:[[10,5],[-10,5],[10,-5],[-10,-5]],
  intensity:4200,radius:0.5,inner:0.4,outer:0.9,sunEmission:3,sunDirection:[0.2,-0.4,1],sunRadius:0.00465,
});

test("pitch original remains immutable across camera edits, reconnects and returned metadata changes", () => {
  const input = metadata(), controls = new PitchControls(input), state = scene();
  const original = structuredClone(input.original_camera);
  input.original_camera.ti[0] = 123;
  const copy = controls.metadata;
  copy.original_camera.ti[0] = 987;
  const zoom = controls.camera(state,"zoom-in")[0]!;
  assert.equal(zoom.kind,"camera_set");
  if (zoom.kind !== "camera_set") throw new Error("camera");
  assert.equal(zoom.camera.kind,"intrinsics_pose");
  state.state.camera = { mode:"manual",camera:zoom.camera };
  assert.deepEqual(controls.camera(state,"reset"),[{kind:"camera_set",camera:original}]);
  const reconnected = new PitchControls(controls.metadata);
  assert.deepEqual(reconnected.camera(state,"reset"),controls.camera(state,"reset"));
  state.runtime_id = "other";
  assert.throws(() => controls.camera(state,"reset"),/another installation/);
  const unknown = scene();
  if (unknown.state.camera.mode === "manual" && unknown.state.camera.camera.kind === "ti_c") unknown.state.camera.camera.ti[0] = 1;
  assert.throws(() => controls.camera(unknown,"zoom-in"),/Unknown imported/);
});

test("pitch model edits ground transformed all-parts bounds after rotated nonuniform mirrored bases", () => {
  const controls = new PitchControls(metadata());
  for (const mirror of [false,true]) {
    const operation = controls.model(scene(),0,1.7,0.7,mirror)[0]!;
    if (operation.kind !== "instance_put") throw new Error("model");
    const model = operation.instance.model;
    const bounds = controls.metadata.models[0]!.bounds;
    const zs = Array.from({length:8},(_,corner) =>
      model[2][3] + [0,1,2].reduce((sum,axis) => sum + model[2][axis]! * (corner & (1<<axis) ? bounds.max[axis]! : bounds.min[axis]!),0));
    assert.ok(Math.abs(Math.max(...zs))<1e-10);
    assert.ok(Math.min(...zs)<0);
    assert.equal(model[0][3],10);
    assert.equal(model[1][3],4);
  }
  assert.throws(() => controls.model(scene(),0,0,0,false));
});

test("rig selection never doubles energy; corners, targets, independent sun and limits are enforced", () => {
  const controls = new PitchControls(metadata());
  for (const height of [30,35,40]) for (const kind of ["none","points","spots"] as const) for (const sun of [false,true]) {
    const edit = {...rig(),height,rig:kind,sun};
    const operations = controls.lighting(scene(),edit);
    const lighting = operations.find(op => op.kind==="lighting_set")!;
    if (lighting.kind!=="lighting_set") throw new Error("lighting");
    assert.equal(lighting.lighting.lights.length,(kind==="none"?0:4)+(sun?1:0));
    for (const [i,light] of lighting.lighting.lights.filter(l=>l.kind!=="directional").entries()) {
      assert.equal(light.position[2],-height);
      assert.equal(Math.abs(light.position[0]),50);
      assert.equal(Math.abs(light.position[1]),32);
      if (light.kind==="spot") {
        assert.deepEqual(light.position.map((n,j)=>n+light.direction[j]!),[...edit.targets[i]!,0]);
      }
    }
  }
  assert.throws(()=>controls.lighting(scene(),{...rig(),height:29}));
  assert.throws(()=>controls.lighting(scene(),{...rig(),height:41}));
  assert.throws(()=>controls.lighting(scene(),{...rig(),targets:[[90,0],[0,0],[0,0],[0,0]]}));
  const invalid = metadata(); invalid.source_frame = "";
  assert.throws(()=>new PitchControls(invalid));
});
