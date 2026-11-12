import { SceneControls, editCamera, type CameraEdit } from "../scene-controls.ts";
import type { RpcClient } from "../rpc.ts";
import type { RenderShadowMode } from "../scene-frame.ts";
import { PitchControls, type RigEdit } from "../pitch-controls.ts";
import { ObjectRotation } from "../object-rotation.ts";

let pitch: PitchControls | null = null;
export const pitchMetadata = () => pitch?.metadata ?? null;

export function installScenePanel(client: () => RpcClient | null) {
  const panel = document.querySelector<HTMLElement>("#program-scene")!;
  const runtime = panel.querySelector<HTMLInputElement>("#runtime-id")!;
  const scene = panel.querySelector<HTMLInputElement>("#scene-id")!;
  const status = panel.querySelector<HTMLElement>("#scene-status")!;
  const controls = panel.querySelector<HTMLFieldSetElement>("fieldset")!;
  const params = new URLSearchParams(location.search);
  runtime.value = params.get("runtime") ?? "";
  scene.value = params.get("scene") ?? "";
  let editor: SceneControls | null = null;
  let connection: RpcClient | null = null;
  let rotation: ObjectRotation | null = null;
  const number = (id: string) => Number(panel.querySelector<HTMLInputElement>(id)!.value);
  panel.querySelector<HTMLInputElement>("#pitch-file")!.onchange = async event => {
    try {
      if (editor !== null) throw new Error("Reload page before replacing immutable installation metadata");
      const file = (event.target as HTMLInputElement).files?.[0];
      if (!file || file.size > 1024 * 1024) throw new Error("Select native-written pitch controls JSON (max 1 MiB)");
      pitch = new PitchControls(JSON.parse(await file.text()));
      const metadata = pitch.metadata;
      runtime.value = metadata.target.runtime_id;
      scene.value = metadata.target.scene_id;
      runtime.readOnly = scene.readOnly = true;
      panel.querySelector<HTMLElement>("#pitch-options")!.hidden = false;
      panel.querySelector<HTMLElement>("#pitch-provenance")!.textContent =
        `${metadata.source_frame}; ${metadata.center_semantics}; ${metadata.ground_status}. Original pinhole is immutable; pose controls are a derived preview, not measured K/R.`;
      const select = panel.querySelector<HTMLSelectElement>("#pitch-model")!;
      select.replaceChildren(...metadata.models.map((_, i) => new Option(`Registered model ${i + 1}`, String(i))));
      panel.dataset.pitchReady = "true";
    } catch (error) { status.textContent = String(error); }
  };
  const report = () => {
    const snapshot = editor?.snapshot;
    controls.disabled = !snapshot || rotation?.running === true;
    status.textContent = snapshot ? `revision ${snapshot.scene_revision}, effect ${snapshot.effect.sequence}${editor?.uncertain ? " — reload required" : ""}` : "Not loaded";
    if (rotation?.error) status.textContent += ` — ${rotation.error}`;
  };
  panel.querySelector<HTMLButtonElement>("#scene-load")!.onclick = async () => {
    try {
      const rpc = client();
      if (rpc === null) throw new Error("Connect WebSocket first");
      if (editor === null || connection !== rpc || editor.target.runtime_id !== runtime.value || editor.target.scene_id !== scene.value) {
        editor = new SceneControls(rpc, { runtime_id: runtime.value, scene_id: scene.value });
        connection = rpc;
        rotation = new ObjectRotation(editor, report);
      }
      const snapshot = await editor.load();
      pitch?.check(snapshot);
      controls.disabled = false;
      report();
    } catch (error) { status.textContent = String(error); controls.disabled = true; }
  };
  for (const button of panel.querySelectorAll<HTMLButtonElement>("[data-camera]")) {
    button.onclick = async () => {
      try {
        if (editor === null || connection !== client()) throw new Error("Reload scene on this connection");
        await editor.mutate(snapshot => {
          if (pitch !== null) return pitch.camera(snapshot, button.dataset.camera as CameraEdit | "reset");
          if (snapshot.state.camera.mode !== "manual") throw new Error("Manual camera required");
          const action = button.dataset.camera!;
          const camera = action === "reset" ? editor!.initial!.state.camera : snapshot.state.camera;
          if (camera.mode !== "manual") throw new Error("Manual reset camera required");
          return [{ kind: "camera_set", camera: action === "reset" ? camera.camera : editCamera(camera.camera, action as CameraEdit) }];
        });
        report();
      } catch (error) { report(); status.textContent += ` — ${error}`; }
    };
  }
  panel.querySelector<HTMLButtonElement>("#scene-lighting")!.onclick = async () => {
    try {
      if (editor === null || connection !== client()) throw new Error("Reload scene on this connection");
      const gain = Number(panel.querySelector<HTMLInputElement>("#sun-gain")!.value);
      if (!Number.isFinite(gain) || gain < 0 || gain > 20) throw new Error("Sun irradiance must be 0..20");
      const shadow = panel.querySelector<HTMLSelectElement>("#shadow-mode")!.value as RenderShadowMode;
      await editor.mutate(snapshot => {
        if (pitch !== null) {
          const mode = panel.querySelector<HTMLSelectElement>("#pitch-rig")!.value;
          const operations = pitch.lighting(snapshot, {
            rig: mode === "sun" ? "none" : mode.startsWith("points") ? "points" : "spots",
            sun: mode === "sun" || mode.endsWith("+sun"),
            height: number("#tower-height"), intensity: number("#tower-emission"),
            radius: number("#source-radius"), inner: number("#cone-inner"), outer: number("#cone-outer"),
            targets: JSON.parse(panel.querySelector<HTMLInputElement>("#spot-targets")!.value) as RigEdit["targets"],
            sunEmission: gain, sunRadius: number("#sun-radius"),
            sunDirection: JSON.parse(panel.querySelector<HTMLInputElement>("#sun-direction")!.value) as RigEdit["sunDirection"],
          });
          return [...operations, { kind: "shadow_mode_set", shadow_mode: shadow }];
        }
        const lighting = structuredClone(snapshot.state.lighting);
        const sun = lighting.lights.find(light => light.light_id === "sun" && light.kind === "directional");
        if (sun?.kind !== "directional") throw new Error("Synthetic directional sun is absent");
        sun.irradiance = [gain, gain, gain];
        return [{ kind: "lighting_set", lighting }, { kind: "shadow_mode_set", shadow_mode: shadow }];
      });
      report();
    } catch (error) { report(); status.textContent += ` — ${error}`; }
  };
  panel.querySelector<HTMLButtonElement>("#model-apply")!.onclick = async () => {
    try {
      if (!editor || !pitch || connection !== client()) throw new Error("Load pitch controls on this connection");
      const index = number("#pitch-model"), scale = number("#model-scale"), yaw = number("#model-yaw") * Math.PI / 180;
      const mirror = panel.querySelector<HTMLInputElement>("#model-mirror")!.checked;
      await editor.mutate(snapshot => pitch!.model(snapshot, index, scale, yaw, mirror));
      report();
    } catch (error) { report(); status.textContent += ` — ${error}`; }
  };
  return {
    async startRotation(speed: number) {
      if (!rotation || connection !== client()) throw new Error("Load the program scene first");
      await rotation.start(speed);
    },
    async stopRotation() { await rotation?.stop(); },
    async rotationState() {
      await rotation?.refresh();
      return rotation;
    },
  };
}
