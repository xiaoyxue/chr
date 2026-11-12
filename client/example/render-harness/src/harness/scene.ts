// Trusted-local CLI/agent scene client using the same edit logic as the browser.
import { parseArgs } from "node:util";
import { readFileSync } from "node:fs";
import { RpcClient, connectWebSocket } from "../rpc.ts";
import { connectNativeSocket } from "../node/native-socket.ts";
import { SceneControls, editCamera, type CameraEdit } from "../scene-controls.ts";
import type { RenderSceneOperation, RenderShadowMode } from "../scene-frame.ts";
import { PitchControls, type RigEdit } from "../pitch-controls.ts";

const { values } = parseArgs({ options: {
  socket: { type: "string" }, ws: { type: "string" },
  runtime: { type: "string" }, scene: { type: "string" },
  camera: { type: "string" }, sun: { type: "string" }, shadow: { type: "string" },
  "pitch-controls": { type: "string" }, rig: { type: "string" }, height: { type: "string", default: "35" },
  "rig-settings": { type: "string" }, model: { type: "string" }, scale: { type: "string", default: "1" },
  yaw: { type: "string", default: "0" }, mirror: { type: "boolean", default: false }, summary: { type: "boolean" },
  help: { type: "boolean" },
} });
if (values.help) {
  console.log("npm run scene -- (--socket PATH | --ws URL) (--runtime ID --scene ID | --pitch-controls LOCAL.json) [--camera zoom-in|zoom-out|dolly-in|dolly-out|orbit-left|orbit-right|orbit-up|orbit-down|yaw-left|yaw-right|pitch-up|pitch-down|roll-left|roll-right|reset] [--sun 0..20] [--shadow off|hard|pcf|pcss] [--rig sun|points|spots|points+sun|spots+sun --height 30..40 --rig-settings JSON] [--model INDEX --scale 0.05..10 --yaw DEGREES --mirror] [--summary]\nPitch rig-settings is a local RigEdit JSON object; not a new RPC method. No edits: query. Edits form one atomic mutation (maximum 8 operations). Full snapshots contain local camera data; use --summary for shareable aggregates.");
} else {
  const pitch = values["pitch-controls"] ? new PitchControls(JSON.parse(readFileSync(values["pitch-controls"], "utf8"))) : null;
  const target = pitch?.metadata.target ?? { runtime_id: values.runtime ?? "", scene_id: values.scene ?? "" };
  if (!target.runtime_id || !target.scene_id || Boolean(values.socket) === Boolean(values.ws)) {
    throw new Error("Supply runtime/scene IDs and exactly one socket or ws transport");
  }
  const client = new RpcClient(values.socket ? await connectNativeSocket(values.socket) : await connectWebSocket(values.ws!), { idPrefix: "scene-cli" });
  try {
    const editor = new SceneControls(client, target);
    let snapshot = await editor.load();
    pitch?.check(snapshot);
    if (values.camera || values.sun !== undefined || values.shadow || values.rig || values["rig-settings"] || values.model !== undefined) {
      snapshot = await editor.mutate(scene => {
        const operations: RenderSceneOperation[] = [];
        if (values.camera) {
          if (!["zoom-in", "zoom-out", "dolly-in", "dolly-out", "orbit-left", "orbit-right", "orbit-up", "orbit-down",
            "yaw-left","yaw-right","pitch-up","pitch-down","roll-left","roll-right","reset"].includes(values.camera)) throw new Error("Unknown camera edit");
          if (scene.state.camera.mode !== "manual") throw new Error("Manual camera required");
          if (pitch) operations.push(...pitch.camera(scene,values.camera as CameraEdit | "reset"));
          else {
            if (values.camera === "reset") throw new Error("CLI reset requires native-written pitch metadata");
            operations.push({ kind: "camera_set", camera: editCamera(scene.state.camera.camera, values.camera as CameraEdit) });
          }
        }
        if (values.rig || values["rig-settings"]) {
          if (!pitch) throw new Error("Rig edits require pitch metadata");
          const mode = values.rig ?? "spots+sun";
          if (!["sun","points","spots","points+sun","spots+sun"].includes(mode)) throw new Error("Unknown rig");
          const settings: RigEdit = values["rig-settings"] ? JSON.parse(values["rig-settings"]) as RigEdit : {
            rig: mode === "sun" ? "none" : mode.startsWith("points") ? "points" : "spots",
            sun: mode === "sun" || mode.endsWith("+sun"), height: Number(values.height),
            targets: [[0,0],[0,0],[0,0],[0,0]], intensity: 4200, radius: 0.5, inner: 0.4, outer: 0.9,
            sunEmission: Number(values.sun ?? "3"), sunDirection: [0.2,-0.4,1], sunRadius: 0.00465,
          };
          operations.push(...pitch.lighting(scene,settings));
        } else if (values.sun !== undefined) {
          const gain = Number(values.sun);
          if (!Number.isFinite(gain) || gain < 0 || gain > 20) throw new Error("Sun must be 0..20");
          const lighting = structuredClone(scene.state.lighting);
          const sun = lighting.lights.find(light => light.light_id === "sun");
          if (sun?.kind !== "directional") throw new Error("Synthetic sun is absent");
          sun.irradiance = [gain, gain, gain];
          operations.push({ kind: "lighting_set", lighting });
        }
        if (values.model !== undefined) {
          if (!pitch) throw new Error("Model edits require pitch metadata");
          operations.push(...pitch.model(scene,Number(values.model),Number(values.scale),Number(values.yaw)*Math.PI/180,values.mirror));
        }
        if (values.shadow) {
          if (!["off", "hard", "pcf", "pcss"].includes(values.shadow)) throw new Error("Unknown shadow mode");
          operations.push({ kind: "shadow_mode_set", shadow_mode: values.shadow as RenderShadowMode });
        }
        return operations;
      });
    }
    console.log(JSON.stringify(values.summary ? { scene_revision: snapshot.scene_revision, effect: snapshot.effect.sequence,
      instances: snapshot.state.instances.length, lights: snapshot.state.lighting.lights.length, shadow_mode: snapshot.state.shadow_mode } : snapshot));
  } finally { client.close(); }
}
