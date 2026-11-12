// Bounded browser-only acceptance: operate real panel buttons and inspect decoded SDI video.
import type { RpcClient } from "../rpc.ts";
import { parseSceneFrameResult } from "../scene-frame.ts";
import { pitchMetadata } from "./scene-panel.ts";

export async function programSelfTest(rpc: RpcClient, video: HTMLVideoElement): Promise<object> {
  if (new URLSearchParams(location.search).get("pitch") === "1") {
    const deadline = Date.now() + 15000;
    while (pitchMetadata() === null && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 100));
    if (pitchMetadata() === null) throw new Error("Missing local pitch metadata");
  }
  const pitch = pitchMetadata();
  const runtime_id = document.querySelector<HTMLInputElement>("#runtime-id")!.value;
  const scene_id = document.querySelector<HTMLInputElement>("#scene-id")!.value;
  const target = { runtime_id, scene_id };
  const query = async () => parseSceneFrameResult("scene.get", await rpc.call("scene.get", target));
  const waitFor = async (test: () => Promise<boolean>) => {
    const deadline = Date.now() + 7000;
    while (Date.now() < deadline) {
      if (await test()) return;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    throw new Error(`program UI timeout: ${document.querySelector("#scene-status")!.textContent}`);
  };
  document.querySelector<HTMLDetailsElement>("#program-scene")!.open = true;
  document.querySelector<HTMLButtonElement>("#scene-load")!.click();
  await waitFor(async () => !document.querySelector<HTMLFieldSetElement>("#program-scene fieldset")!.disabled);
  await waitFor(async () => video.videoWidth > 0 && video.readyState >= 2);
  const canvas = document.createElement("canvas");
  canvas.width = 64; canvas.height = 48;
  const context = canvas.getContext("2d", { willReadFrequently: true })!;
  const pixels = () => {
    context.drawImage(video, 0, 0, 64, 48);
    return context.getImageData(0, 0, 64, 48).data;
  };
  await new Promise(resolve => setTimeout(resolve, 300));
  const before = pixels();
  const initial = await query();
  const revisions: string[] = [];
  const apply = async (selector: string) => {
    const previous = await query();
    document.querySelector<HTMLButtonElement>(selector)!.click();
    await waitFor(async () => BigInt((await query()).scene_revision) > BigInt(previous.scene_revision));
    revisions.push((await query()).scene_revision);
  };
  for (const action of ["zoom-in", "zoom-out", "dolly-in", "dolly-out", "orbit-up", "orbit-down", "orbit-left", "orbit-right",
    "yaw-left", "yaw-right", "pitch-up", "pitch-down", "roll-left", "roll-right", "reset"]) {
    await apply(`[data-camera="${action}"]`);
  }
  const expectedCamera = pitch ? { mode: "manual", camera: pitch.original_camera } : initial.state.camera;
  if (JSON.stringify((await query()).state.camera) !== JSON.stringify(expectedCamera)) {
    throw new Error("Panel reset did not restore original camera");
  }
  const meanDifference = (a: Uint8ClampedArray, b: Uint8ClampedArray) =>
    a.reduce((sum,n,i) => sum + Math.abs(n-b[i]!),0)/a.length;
  await apply('[data-camera="zoom-in"]');
  let zoomDifference = 0;
  await waitFor(async () => { zoomDifference = meanDifference(pixels(),before); return zoomDifference > 0.5; });
  await apply('[data-camera="reset"]');
  await waitFor(async () => meanDifference(pixels(),before) < Math.max(2,zoomDifference*0.8));
  if (pitch) {
    for (const rig of ["sun","points","spots","points+sun","spots+sun"]) {
      document.querySelector<HTMLSelectElement>("#pitch-rig")!.value = rig;
      await apply("#scene-lighting");
      const state = (await query()).state;
      const towers = state.lighting.lights.filter(light => light.kind !== "directional");
      const suns = state.lighting.lights.filter(light => light.kind === "directional");
      if (towers.length !== (rig === "sun" ? 0 : 4) || suns.length !== (rig === "sun" || rig.endsWith("+sun") ? 1 : 0)) throw new Error("Double energy or missing rig");
    }
    for (const shadow of ["hard","pcf","pcss"]) {
      document.querySelector<HTMLSelectElement>("#shadow-mode")!.value = shadow;
      await apply("#scene-lighting");
      if ((await query()).state.shadow_mode !== shadow) throw new Error("Shadow selection missing");
    }
    for (let index = 0; index < pitch.models.length; index++) {
      document.querySelector<HTMLSelectElement>("#pitch-model")!.value = String(index);
      document.querySelector<HTMLInputElement>("#model-scale")!.value = "1.2";
      document.querySelector<HTMLInputElement>("#model-yaw")!.value = "33";
      document.querySelector<HTMLInputElement>("#model-mirror")!.checked = true;
      await apply("#model-apply");
      if ((await query()).state.instances.find(i => i.instance_id === "model")?.asset_ref !== pitch.models[index]!.instance.asset_ref) throw new Error("Model selection missing");
    }
    document.querySelector<HTMLSelectElement>("#pitch-model")!.value = "0";
    document.querySelector<HTMLInputElement>("#model-scale")!.value = "1";
    document.querySelector<HTMLInputElement>("#model-yaw")!.value = "0";
    document.querySelector<HTMLInputElement>("#model-mirror")!.checked = false;
    await apply("#model-apply");
    document.querySelector<HTMLSelectElement>("#pitch-rig")!.value = "sun";
  }
  document.querySelector<HTMLInputElement>("#sun-gain")!.value = "0";
  document.querySelector<HTMLSelectElement>("#shadow-mode")!.value = "off";
  await apply("#scene-lighting");
  let difference = 0;
  await waitFor(async () => {
    const after = pixels();
    difference = after.reduce((sum, n, i) => sum + Math.abs(n - before[i]!), 0) / after.length;
    return difference > 5;
  });
  const dark = await query();
  if (dark.state.shadow_mode !== "off") throw new Error("Atomic shadow edit missing");
  document.querySelector<HTMLInputElement>("#sun-gain")!.value = "3";
  document.querySelector<HTMLSelectElement>("#shadow-mode")!.value = "pcf";
  await apply("#scene-lighting");
  await apply('[data-camera="reset"]');
  if (JSON.stringify((await query()).state.camera) !== JSON.stringify(expectedCamera)) throw new Error("Original camera changed after rig/model edits");
  return { passed: true, revisions, decodedMeanAbsoluteDifference: difference, zoomDecodedDifference: zoomDifference, pitch: pitch !== null };
}
