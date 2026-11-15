export interface UndoEntry {
  /** What the step did, shown on the undo button, e.g. "移动 广告位 A". */
  label: string;
  /** Restores the state from before the step; throws if it cannot right now. */
  restore: () => void | Promise<void>;
  /** Makes the step again after it was undone; throws if it cannot right now. */
  reapply: () => void | Promise<void>;
}

/** Undo and redo history of draft edits. Only the newest entries up to `limit` are kept; a new step clears redo. */
export class UndoStack {
  private entries: UndoEntry[] = [];
  private undone: UndoEntry[] = [];
  private running = false;

  constructor(private readonly onChange: () => void = () => {}, private readonly limit = 100) {}

  get size() { return this.entries.length; }
  get label() { return this.entries.at(-1)?.label; }
  get redoLabel() { return this.undone.at(-1)?.label; }
  get canUndo() { return !this.running && this.entries.length > 0; }
  get canRedo() { return !this.running && this.undone.length > 0; }

  push(entry: UndoEntry) {
    this.entries.push(entry);
    if (this.entries.length > this.limit) this.entries.shift();
    this.undone = [];
    this.onChange();
  }

  clear() {
    this.entries = [];
    this.undone = [];
    this.onChange();
  }

  /** Restores the newest step; it stays on the stack if restoring fails. */
  async undo() {
    if (!this.canUndo) return;
    await this.move(this.entries, this.undone, (entry) => entry.restore());
  }

  /** Makes the newest undone step again; it stays undone if that fails. */
  async redo() {
    if (!this.canRedo) return;
    await this.move(this.undone, this.entries, (entry) => entry.reapply());
  }

  private async move(from: UndoEntry[], to: UndoEntry[], run: (entry: UndoEntry) => void | Promise<void>) {
    const entry = from.pop();
    this.running = true;
    this.onChange();
    try {
      await run(entry);
      to.push(entry);
    } catch (error) {
      from.push(entry);
      throw error;
    } finally {
      this.running = false;
      this.onChange();
    }
  }
}
