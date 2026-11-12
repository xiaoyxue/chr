// Trusted-local CLI/agent scene client using the same edit logic as the browser.
import { parseArgs } from "node:util";
import { RpcClient, connectWebSocket } from "../rpc.ts";
import { connectNativeSocket } from "../node/native-socket.ts";
import { SceneControls, editCamera, type CameraEdit } from "../scene-controls.ts";
import type { RenderSceneOperation, RenderShadowMode } from "../scene-frame.ts";

const { values } = parseArgs({ options: {
  socket: { type: "string" }, ws: { type: "string" },
  runtime: { type: "string" }, scene: { type: "string" },
  camera: { type: "string" }, sun: { type: "string" }, shadow: { type: "string" },
  help: { type: "boolean" },
} });
if (values.help) {
  console.log("npm run scene -- (--socket PATH | --ws URL) --runtime ID --scene ID [--camera zoom-in|zoom-out|dolly-in|dolly-out|orbit-left|orbit-right|orbit-up|orbit-down] [--sun 0..20] [--shadow off|hard|pcf|pcss]\nNo edits: query the current scene. Edits form one atomic mutation.");
} else {
  if (!values.runtime || !values.scene || Boolean(values.socket) === Boolean(values.ws)) {
    throw new Error("Supply runtime/scene IDs and exactly one socket or ws transport");
  }
  const client = new RpcClient(values.socket ? await connectNativeSocket(values.socket) : await connectWebSocket(values.ws!), { idPrefix: "scene-cli" });
  try {
    const editor = new SceneControls(client, { runtime_id: values.runtime, scene_id: values.scene });
    let snapshot = await editor.load();
    if (values.camera || values.sun !== undefined || values.shadow) {
      snapshot = await editor.mutate(scene => {
        const operations: RenderSceneOperation[] = [];
        if (values.camera) {
          if (!["zoom-in", "zoom-out", "dolly-in", "dolly-out", "orbit-left", "orbit-right", "orbit-up", "orbit-down"].includes(values.camera)) throw new Error("Unknown camera edit");
          if (scene.state.camera.mode !== "manual") throw new Error("Manual camera required");
          operations.push({ kind: "camera_set", camera: editCamera(scene.state.camera.camera, values.camera as CameraEdit) });
        }
        if (values.sun !== undefined) {
          const gain = Number(values.sun);
          if (!Number.isFinite(gain) || gain < 0 || gain > 20) throw new Error("Sun must be 0..20");
          const lighting = structuredClone(scene.state.lighting);
          const sun = lighting.lights.find(light => light.light_id === "sun");
          if (sun?.kind !== "directional") throw new Error("Synthetic sun is absent");
          sun.irradiance = [gain, gain, gain];
          operations.push({ kind: "lighting_set", lighting });
        }
        if (values.shadow) {
          if (!["off", "hard", "pcf", "pcss"].includes(values.shadow)) throw new Error("Unknown shadow mode");
          operations.push({ kind: "shadow_mode_set", shadow_mode: values.shadow as RenderShadowMode });
        }
        return operations;
      });
    }
    console.log(JSON.stringify(snapshot));
  } finally { client.close(); }
}
