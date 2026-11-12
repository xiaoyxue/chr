// Bounded browser-only acceptance: operate real panel buttons and inspect decoded SDI video.
import type { RpcClient } from "../rpc.ts";
import { parseSceneFrameResult } from "../scene-frame.ts";

export async function programSelfTest(rpc: RpcClient, video: HTMLVideoElement): Promise<object> {
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
  const before = pixels();
  const initial = await query();
  const revisions: string[] = [];
  const apply = async (selector: string) => {
    const previous = await query();
    document.querySelector<HTMLButtonElement>(selector)!.click();
    await waitFor(async () => BigInt((await query()).scene_revision) > BigInt(previous.scene_revision));
    revisions.push((await query()).scene_revision);
  };
  for (const action of ["zoom-in", "zoom-out", "dolly-in", "dolly-out", "orbit-up", "orbit-down", "orbit-left", "orbit-right", "reset"]) {
    await apply(`[data-camera="${action}"]`);
  }
  if (JSON.stringify((await query()).state.camera) !== JSON.stringify(initial.state.camera)) {
    throw new Error("Panel reset did not restore loaded camera");
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
  return { passed: true, revisions, decodedMeanAbsoluteDifference: difference };
}
