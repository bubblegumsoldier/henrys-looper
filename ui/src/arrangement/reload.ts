//! "Reload" in the Arrangement: reset everything, then load the file again - one step, no question.
//!
//! The owner's wish: Reload always resets and loads. The Rust side does the reset (score stopped,
//! every track cleared, a running recording finished and saved, the engine rebuilt when the track
//! list changed) and only once the text has compiled - a file with errors leaves everything as it
//! was and only the error is shown. This side decides which text goes: the file as it is on disk
//! now, unless there is typing here that was not saved (that is never thrown away by a Reload) or
//! "Keep mine" was chosen against the disk.

export interface ReloadSteps {
  /** The open file: its path (null for an untitled score), unsaved typing, "Keep mine" chosen. */
  file: { path: string | null; dirty: boolean; keptMine: boolean };
  /** The file as it is on disk now; null when it cannot be read. */
  peek: (path: string) => Promise<string | null>;
  /** Put the disk's text into the editor, as opening the file does. */
  adopt: (text: string) => void;
  /** Hand the editor's text to the runner with the reset (`scoreApi.load(…, reset = true)`). */
  loadWithReset: () => Promise<void>;
}

/** Whether a Reload reads the file from disk first, or loads the editor's text as it is. */
export function readsDisk(file: ReloadSteps["file"]): boolean {
  return file.path !== null && !file.dirty && !file.keptMine;
}

/** Reset and load again. Rejects with the runner's sentence (nothing was reset then). */
export async function reloadEverything(steps: ReloadSteps): Promise<void> {
  if (readsDisk(steps.file) && steps.file.path !== null) {
    const disk = await steps.peek(steps.file.path);
    if (disk !== null) steps.adopt(disk);
  }
  await steps.loadWithReset();
}
