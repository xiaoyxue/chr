import { SceneControls } from "./scene-controls.ts";
import {
  parseSceneFrameResult, type RenderMatrix4, type RenderTimeBase,
} from "./scene-frame.ts";

/** Rotate in model-local XY; translation and the ground receiver are untouched. */
export function rotateModelZ(base: RenderMatrix4, angle: number): RenderMatrix4 {
  if (!Number.isFinite(angle)) throw new Error("Rotation angle must be finite");
  const result = structuredClone(base);
  const c = Math.cos(angle), s = Math.sin(angle);
  for (let row = 0; row < 3; row++) {
    result[row]![0] = base[row]![0] * c + base[row]![1] * s;
    result[row]![1] = -base[row]![0] * s + base[row]![1] * c;
  }
  return result;
}

export function advanceRotation(
  angle: number, speed: number, previous: number, frame: number, rate: RenderTimeBase,
): number {
  if (!Number.isSafeInteger(frame) || !Number.isSafeInteger(previous) || frame < previous ||
      previous < 0 || !Number.isFinite(speed) || !Number.isFinite(angle) ||
      rate.numerator <= 0 || rate.denominator <= 0) throw new Error("Invalid rotation timeline");
  const next = angle + (frame - previous) * speed * rate.numerator / rate.denominator;
  if (!Number.isFinite(next)) throw new Error("Rotation phase overflow");
  return ((next % (2 * Math.PI)) + 2 * Math.PI) % (2 * Math.PI);
}

/** Demo controller: one scene mutation at a time, phase follows simulated output slots. */
export class ObjectRotation {
  readonly editor: SceneControls;
  readonly changed: () => void;
  running = false;
  angle = 0;
  speed = 0;
  frame = 0;
  error = "";
  #epoch = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #pending: Promise<void> | null = null;
  #base: RenderMatrix4 | null = null;
  #lastModel: RenderMatrix4 | null = null;
  #asset = "";
  #rate: RenderTimeBase = { numerator: 1, denominator: 30 };

  constructor(editor: SceneControls, changed: () => void) {
    this.editor = editor;
    this.changed = changed;
  }

  async #outputSlot(): Promise<number> {
    const status = await this.editor.rpc.call("sdi.get_status") as { frames_output: number };
    const frame = status.frames_output;
    if (!Number.isSafeInteger(frame) || frame <= 0) {
      throw new Error("Wait for the simulated output clock");
    }
    return frame;
  }

  async start(speed: number): Promise<void> {
    if (!Number.isFinite(speed)) throw new Error("Rotation speed must be finite");
    const epoch = ++this.#epoch;
    this.running = false;
    clearTimeout(this.#timer);
    await this.#pending;
    if (epoch !== this.#epoch) return;
    const snapshot = await this.editor.load();
    const mesh = snapshot.state.instances.find(instance => instance.instance_id === "mesh");
    if (!mesh || !snapshot.state.instances.some(instance =>
      instance.instance_id === "ground" && instance.asset_ref !== mesh.asset_ref)) {
      throw new Error("This controller requires the updated software-video mesh/ground scene");
    }
    const configuration = parseSceneFrameResult("render.get_configuration",
      await this.editor.rpc.call("render.get_configuration", { runtime_id: this.editor.target.runtime_id }));
    const frame = await this.#outputSlot();
    if (epoch !== this.#epoch) return;
    this.#base = structuredClone(mesh.model);
    this.#lastModel = structuredClone(mesh.model);
    this.#asset = mesh.asset_ref;
    this.#rate = configuration.output_rate;
    this.angle = 0;
    this.speed = speed;
    this.frame = frame;
    this.error = "";
    this.running = true;
    this.changed();
    this.#schedule(epoch);
  }

  async stop(): Promise<void> {
    ++this.#epoch;
    this.running = false;
    clearTimeout(this.#timer);
    await this.#pending;
    this.changed();
  }

  #schedule(epoch: number): void {
    this.#timer = setTimeout(() => {
      this.#pending = this.#step(epoch).finally(() => {
        this.#pending = null;
        if (this.running && epoch === this.#epoch) this.#schedule(epoch);
      });
    }, 33);
  }

  async #step(epoch: number): Promise<void> {
    try {
      const frame = await this.#outputSlot();
      if (!this.running || epoch !== this.#epoch || frame === this.frame) return;
      const angle = advanceRotation(this.angle, this.speed, this.frame, frame, this.#rate);
      const model = rotateModelZ(this.#base!, angle);
      const applied = await this.editor.mutate(snapshot => {
        const mesh = snapshot.state.instances.find(instance => instance.instance_id === "mesh");
        if (!mesh || mesh.asset_ref !== this.#asset ||
            JSON.stringify(mesh.model) !== JSON.stringify(this.#lastModel)) {
          throw new Error("Mesh changed externally; restart rotation after reconciling");
        }
        return [{ kind: "instance_put", instance: { ...mesh, model } }];
      });
      const actual = applied.state.instances.find(instance => instance.instance_id === "mesh");
      if (!actual || actual.asset_ref !== this.#asset || actual.model.some((row, r) =>
        row.some((value, c) => Math.abs(value - model[r]![c]!) > 1e-12 * Math.max(1, Math.abs(value))))) {
        throw new Error("Mesh changed before its update was acknowledged");
      }
      // Retain the server's canonical floats, not pre-serialization coefficients.
      this.#lastModel = structuredClone(actual.model);
      this.frame = frame;
      this.angle = angle;
      this.changed();
    } catch (error) {
      this.running = false;
      this.error = String(error);
      this.changed();
    }
  }
}
