export interface UndoEntry {
  /** What the step did, shown on the undo button, e.g. "移动 广告位 A". */
  label: string;
  /** Restores the state from before the step; throws if it cannot right now. */
  restore: () => void | Promise<void>;
}

/** Undo history of draft edits. Only the newest entries up to `limit` are kept. */
export class UndoStack {
  private entries: UndoEntry[] = [];
  private running = false;

  constructor(private readonly onChange: () => void = () => {}, private readonly limit = 100) {}

  get size() { return this.entries.length; }
  get label() { return this.entries.at(-1)?.label; }
  get canUndo() { return !this.running && this.entries.length > 0; }

  push(entry: UndoEntry) {
    this.entries.push(entry);
    if (this.entries.length > this.limit) this.entries.shift();
    this.onChange();
  }

  clear() {
    this.entries = [];
    this.onChange();
  }

  /** Restores the newest step; it stays on the stack if restoring fails. */
  async undo() {
    if (!this.canUndo) return;
    const entry = this.entries.pop();
    this.running = true;
    this.onChange();
    try {
      await entry.restore();
    } catch (error) {
      this.entries.push(entry);
      throw error;
    } finally {
      this.running = false;
      this.onChange();
    }
  }
}
