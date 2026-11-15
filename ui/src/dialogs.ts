//! Native questions and file pickers, through Tauri's dialog plugin.
//!
//! In a plain browser tab (the dev server without the app window) there is no plugin; the questions
//! then fall back to `window.confirm`, and the pickers report that there is nothing to pick from.

import { ask, open, save } from "@tauri-apps/plugin-dialog";
import { insideTauri } from "./api";

/** A yes/no question with a named "yes" button. Resolves true for yes. */
export async function confirmAction(message: string, title: string, okLabel: string): Promise<boolean> {
  if (!insideTauri()) return window.confirm(message);
  return ask(message, { title, kind: "warning", okLabel, cancelLabel: "Cancel" });
}

const SCORE_FILTER = [{ name: "Looper score", extensions: ["yaml", "yml"] }];

/** "Open" picker for one score file. Null when cancelled. */
export async function pickScoreToOpen(defaultPath?: string): Promise<string | null> {
  if (!insideTauri()) return null;
  const picked = await open({
    title: "Open score",
    multiple: false,
    directory: false,
    filters: SCORE_FILTER,
    defaultPath,
  });
  return typeof picked === "string" ? picked : null;
}

/** "Save as" picker. Null when cancelled. `defaultPath` may be a directory or a full file path. */
export async function pickScoreToSave(defaultPath?: string): Promise<string | null> {
  if (!insideTauri()) return null;
  const picked = await save({ title: "Save score as", filters: SCORE_FILTER, defaultPath });
  return picked ?? null;
}

/** Folder picker. Null when cancelled. */
export async function pickFolder(title: string, defaultPath?: string): Promise<string | null> {
  if (!insideTauri()) return null;
  const picked = await open({ title, multiple: false, directory: true, defaultPath });
  return typeof picked === "string" ? picked : null;
}
