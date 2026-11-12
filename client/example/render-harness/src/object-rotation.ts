import { SceneControls } from "./scene-controls.ts";
import { Methods, isTriangleState, type MeshRotationState } from "./protocol.ts";

/** Commands the server animation; browser timers never drive model poses. */
export class ObjectRotation {
  readonly editor: SceneControls;
  readonly changed: () => void;
  running = false;
  angle = 0;
  speed = 0;
  frame = 0;
  error = "";

  constructor(editor: SceneControls, changed: () => void) {
    this.editor = editor;
    this.changed = changed;
  }

  async #command(method: string, speed?: number): Promise<void> {
    const target = { ...this.editor.target, instance_id: "mesh" };
    try {
      const state = await this.editor.rpc.call(method, speed === undefined ? target :
        { ...target, angular_velocity_rad_per_s: speed }) as MeshRotationState;
      if (!isTriangleState(state) || state.runtime_id !== target.runtime_id ||
          state.scene_id !== target.scene_id || state.instance_id !== target.instance_id) {
        throw new Error("Rotation reply belongs to another target or has invalid state");
      }
      this.running = state.rotating;
      this.angle = state.angle_rad;
      this.speed = state.angular_velocity_rad_per_s;
      this.frame = state.frame;
      this.error = "";
      this.changed();
    } catch (error) {
      this.error = String(error);
      this.changed();
      throw error;
    }
  }

  async start(speed: number): Promise<void> {
    if (!Number.isFinite(speed)) throw new Error("Rotation speed must be finite");
    await this.#command(Methods.mesh.startRotation, speed);
  }

  async stop(): Promise<void> {
    await this.#command(Methods.mesh.stopRotation);
  }

  async refresh(): Promise<void> {
    await this.#command(Methods.mesh.getRotation);
  }
}
