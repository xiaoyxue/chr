import { SceneControls, editCamera, type CameraEdit } from "../scene-controls.ts";
import type { RpcClient } from "../rpc.ts";
import type { RenderShadowMode } from "../scene-frame.ts";

export function installScenePanel(client: () => RpcClient | null): void {
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
  const report = () => {
    const snapshot = editor?.snapshot;
    status.textContent = snapshot ? `revision ${snapshot.scene_revision}, effect ${snapshot.effect.sequence}${editor?.uncertain ? " — reload required" : ""}` : "Not loaded";
  };
  panel.querySelector<HTMLButtonElement>("#scene-load")!.onclick = async () => {
    try {
      const rpc = client();
      if (rpc === null) throw new Error("Connect WebSocket first");
      if (editor === null || connection !== rpc || editor.target.runtime_id !== runtime.value || editor.target.scene_id !== scene.value) {
        editor = new SceneControls(rpc, { runtime_id: runtime.value, scene_id: scene.value });
        connection = rpc;
      }
      await editor.load();
      controls.disabled = false;
      report();
    } catch (error) { status.textContent = String(error); controls.disabled = true; }
  };
  for (const button of panel.querySelectorAll<HTMLButtonElement>("[data-camera]")) {
    button.onclick = async () => {
      try {
        if (editor === null || connection !== client()) throw new Error("Reload scene on this connection");
        await editor.mutate(snapshot => {
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
        const lighting = structuredClone(snapshot.state.lighting);
        const sun = lighting.lights.find(light => light.light_id === "sun" && light.kind === "directional");
        if (sun?.kind !== "directional") throw new Error("Synthetic directional sun is absent");
        sun.irradiance = [gain, gain, gain];
        return [{ kind: "lighting_set", lighting }, { kind: "shadow_mode_set", shadow_mode: shadow }];
      });
      report();
    } catch (error) { report(); status.textContent += ` — ${error}`; }
  };
}
