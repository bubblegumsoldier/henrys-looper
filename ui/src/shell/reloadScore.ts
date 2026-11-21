//! "App öffnen → Leertaste": a score that was loaded when the app was closed is loaded again once
//! the engine has started by itself (Setup › General: "Reload the last score on start").
//!
//! While the engine runs, the window keeps a note of whether a score is loaded and which file it
//! came from (`looper.loadedScore`). Closing the app stops the engine but leaves that note alone;
//! stopping the engine by hand, or running without a score, removes it. At the next start the note
//! decides: the same file is open, it still exists, it compiles - then its tracks are what the
//! stage shows from the first moment (the preview sent with the setup), the engine is started with
//! them, and the score is loaded exactly as the Load button does. Anything else is said in one
//! sentence, and the app opens in free looping.

import { api, filesApi, scoreApi } from "../api";
import { loadOpenScore } from "../arrangement/scoreActions";
import { scoreFile } from "../scoreFile";
import { subscribe } from "../status";
import { bootEngine, enginePrefs, setPreviewScore } from "./engine";
import { fail } from "./notes";

const KEY = "looper.loadedScore";

interface LoadedNote {
  /** The file the loaded score came from; null for an untitled one. */
  path: string | null;
  name: string | null;
}

function read(): LoadedNote | null {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as LoadedNote) : null;
  } catch {
    return null;
  }
}

function write(value: LoadedNote | null): void {
  try {
    if (value) localStorage.setItem(KEY, JSON.stringify(value));
    else localStorage.removeItem(KEY);
  } catch {
    /* only costs the reload */
  }
}

let watching = false;

/** Mount once: keep the note in step with what the running engine holds. */
export function watchLoadedScore(): void {
  if (watching) return;
  watching = true;
  let last = "";
  subscribe((s) => {
    // Only a running engine says anything about what is loaded; a stopped one (the app closing)
    // must leave the note as it was.
    if (!s.running) return;
    const file = scoreFile();
    const now = s.score ? `${file.path ?? ""}\u0000${file.name ?? ""}` : "";
    if (now === last) return;
    last = now;
    write(s.score ? { path: file.path, name: file.name } : null);
  });
}

/** What decides the reload, apart from the files - pure, for the tests. */
export function shouldReload(
  prefs: { autostart: boolean; reloadScore: boolean },
  note: LoadedNote | null,
  openPath: string | null,
): "reload" | "skip" | "other-file" {
  if (!prefs.autostart || !prefs.reloadScore || !note) return "skip";
  return note.path === openPath ? "reload" : "other-file";
}

/**
 * Hand the setup to the Rust side (the engine starts by itself) and, when the last session ended
 * with a score loaded, load it again. Called once by the frame.
 */
export async function bootWithLastScore(): Promise<void> {
  const note = read();
  const file = scoreFile();
  const label = note?.name ?? "the last score";
  const decision = shouldReload(enginePrefs(), note, file.path);
  if (decision !== "reload") {
    if (decision === "other-file") write(null);
    return bootEngine();
  }
  if (note?.path) {
    const disk = await filesApi.peek(note.path).catch(() => null);
    if (disk === null) {
      write(null);
      fail(`${label} is no longer at ${note.path} - not loaded again. Free looping.`);
      return bootEngine();
    }
  }
  const text = file.text;
  const compiled = text.trim() ? await scoreApi.compile(text).catch(() => null) : null;
  if (!compiled?.ok) {
    write(null);
    fail(`${label} has errors and was not loaded again - free looping. The Arrangement shows what is wrong.`);
    return bootEngine();
  }
  // The stage shows the score's tracks from the first moment, and the engine starts with them.
  setPreviewScore(text);
  await bootEngine();
  setPreviewScore(null);
  // The boot start runs on the Rust side after the answer; this question waits behind it.
  const view = await api.engineState().catch(() => null);
  if (view?.phase !== "running") return;
  try {
    await loadOpenScore();
  } catch (e) {
    fail(`${label} was not loaded again: ${(e as Error).message}`);
  }
}
