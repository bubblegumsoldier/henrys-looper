//! The score file and the runner's "Load", for the Arrangement mode.
//!
//! What the old score view did besides editing: New (from the template, with the engine's tracks),
//! Open, Open recent, Save, Save as - each asking before unsaved changes are thrown away - and
//! loading the score into the runner. The count-in is the score's own `count_in:` (see
//! `countIn.ts`); the toolbar field edits it like any other head field. The text and the file it belongs to live in
//! `scoreFile.ts`; the arrangement's document store follows that file (`useDoc.ts`), so opening a
//! file replaces what the grid shows and starts a fresh undo history.
//!
//! Loading is the one deliberate act and stays a button: it moves the engine's tempo, time
//! signature and layer length to what the score says, which is not something a keystroke should do
//! by accident. Messages go to the shell's notes (toasts), like every other command's.

import { useCallback, useEffect, useState } from "react";
import { filesApi, scoreApi } from "../api";
import { confirmAction, pickScoreToOpen, pickScoreToSave } from "../dialogs";
import { IS_MAC } from "../platform";
import { isDirty, scoreFile, updateScoreFile } from "../scoreFile";
import { fail, note } from "../shell/notes";
import { setLoadedScore } from "../shell/scoreModel";
import { useEngine } from "../shell/engine";
import { currentStatus, useStatusSlice } from "../status";
import type { RecentFile } from "../types";
import { docStore, useDoc } from "./useDoc";
import { docCountIn, setDocCountIn, withoutCountIn } from "./countIn";
import { compareDisk } from "./diskWatch";
import { markLoaded, mixIsLoaded, setLoadedTextCheck, withoutMixer } from "../mixer/scoreMix";
import { reloadEverything } from "./reload";

export interface ScoreActions {
  recent: RecentFile[];
  newScore: () => Promise<void>;
  openFile: () => Promise<void>;
  openRecent: (path: string) => Promise<void>;
  save: () => Promise<void>;
  saveAs: () => Promise<void>;
  load: () => Promise<void>;
  loading: boolean;
  /** The score's `count_in:` (2 when it does not say); what "Load" counts in. */
  countIn: number;
  setCountIn: (bars: number) => void;
  /** The runner holds a score, and it is not the text in the editor. */
  stale: boolean;
  /** The open file was changed on disk since it was opened or saved here (its name), else null. */
  diskChanged: string | null;
  /** Throw away the text here and open the file as it is on disk now. */
  reloadFromDisk: () => Promise<void>;
  /** Keep the text here; the next ⌘S writes over the file. */
  keepMine: () => void;
}

/** The disk text "Keep mine" was chosen against; forgotten on open and save. */
let kept: string | null = null;

/** The text the runner was last given from here. */
let loadedText: string | null = null;

const plain = (t: string) => withoutCountIn(withoutMixer(t));

setLoadedTextCheck((text) => loadedText !== null && plain(loadedText) === plain(text));

/**
 * The runner holds a score, and it is exactly the text in the editor. The `mixer:` block is
 * compared by what it means, not by its text: a fader written back or an undo sent to the engine
 * is no difference, a value changed elsewhere (the file on disk, another song) is.
 */
export function editorIsLoaded(): boolean {
  return (
    loadedText !== null &&
    currentStatus().score !== null &&
    plain(loadedText) === plain(scoreFile().text) &&
    mixIsLoaded(docStore.get().doc)
  );
}

/**
 * Follow an edit the engine can take at once (a track's note grid, its transposition): call this
 * right after the edit, while the editor still holds the loaded text. Sends `push` to the engine
 * and, once the edit is rendered, counts the new text as loaded - engine and score agree again, so
 * the score does not turn stale. Does nothing when the editor was not the loaded score.
 */
export function followLoaded(wasLoaded: boolean, push: () => Promise<void>): void {
  if (!wasLoaded) return;
  void (async () => {
    try {
      await push();
      await docStore.settled();
      loadedText = scoreFile().text;
      for (const l of loadedListeners) l();
    } catch (e) {
      fail((e as Error).message);
    }
  })();
}
const loadedListeners = new Set<() => void>();

/**
 * "Load" without the button: the open score goes to the runner (the engine starts first when it
 * does not run). The Load button and the reload after the app opened (`shell/reloadScore.ts`) both
 * come through here. Rejects with the runner's sentence.
 */
export async function loadOpenScore(reset = false): Promise<void> {
  await docStore.flush();
  await docStore.settled();
  const text = scoreFile().text;
  // No count-in of its own: the score's `count_in:` is what the runner takes. `reset`: Reload -
  // the Rust side resets everything first (see `reload.ts`).
  const loaded = await scoreApi.load(text, undefined, reset);
  loadedText = text;
  // The host has applied the score's `mixer:`; from now on fader and section move together.
  markLoaded();
  for (const l of loadedListeners) l();
  setLoadedScore(loaded.score);
  note(loaded.message);
}

export function useScoreActions(): ScoreActions {
  const [recent, setRecent] = useState<RecentFile[]>([]);
  const [defaultDir, setDefaultDir] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const engineInfo = useEngine().info;
  const [, setLoadedRev] = useState(0);
  const docNow = useDoc().doc;
  const countIn = docCountIn(docNow);
  const runnerCountIn = useStatusSlice((s) => s.score?.count_in ?? null);
  const inRunner = useStatusSlice((s) => s.score !== null);

  useEffect(() => {
    filesApi
      .info()
      .then((info) => {
        setRecent(info.recent);
        setDefaultDir(info.default_dir);
      })
      .catch(() => {
        /* no app window: no files */
      });
  }, []);

  const mayDiscard = useCallback(async () => {
    if (!isDirty()) return true;
    const name = scoreFile().name ?? "the untitled score";
    return confirmAction(`Discard unsaved changes to ${name}?`, "Unsaved changes", "Discard");
  }, []);

  /** The file on disk now, if it differs from what was opened or saved here (and was not kept). */
  const [diskText, setDiskText] = useState<string | null>(null);
  const checkDisk = useCallback(async (): Promise<string | null> => {
    const file = scoreFile();
    if (!file.path) return null;
    let disk: string | null;
    try {
      disk = await filesApi.peek(file.path);
    } catch {
      return null; // no app window
    }
    // The file may have been switched while the answer was on its way.
    if (scoreFile().path !== file.path) return null;
    const changed = compareDisk(scoreFile().savedText, disk, kept) === "changed" ? disk : null;
    setDiskText(changed);
    return changed;
  }, []);

  // Back in the window (after editing the file elsewhere): has it changed?
  useEffect(() => {
    const onFocus = () => void checkDisk();
    const onVisible = () => {
      if (document.visibilityState === "visible") void checkDisk();
    };
    void checkDisk();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [checkDisk]);

  const openPath = useCallback(async (path: string): Promise<boolean> => {
    try {
      const opened = await filesApi.open(path);
      kept = null;
      setDiskText(null);
      updateScoreFile({ text: opened.yaml, path: opened.path, name: opened.name, savedText: opened.yaml });
      setRecent(opened.recent);
      note(`Opened ${opened.name}.`);
      return true;
    } catch (e) {
      fail((e as Error).message);
      filesApi.forget(path).then(setRecent).catch(() => undefined);
      return false;
    }
  }, []);

  const openFile = useCallback(async () => {
    if (!(await mayDiscard())) return;
    const current = scoreFile().path;
    const path = await pickScoreToOpen(current ? current.replace(/[^/\\]+$/, "") : (defaultDir ?? undefined));
    if (path) await openPath(path);
  }, [mayDiscard, openPath, defaultDir]);

  const openRecent = useCallback(
    async (path: string) => {
      if (!(await mayDiscard())) return;
      await openPath(path);
    },
    [mayDiscard, openPath],
  );

  const saveTo = useCallback(async (path: string) => {
    // Typing not parsed yet and grid edits on the wire belong in the file.
    await docStore.flush();
    await docStore.settled();
    const text = scoreFile().text;
    // Never write silently over a change made elsewhere: the bar asks first.
    if (path === scoreFile().path && (await checkDisk()) !== null) {
      fail(`Not saved: ${scoreFile().name ?? "the file"} was changed on disk since it was opened. Choose "Reload from disk" or "Keep mine" first.`);
      return;
    }
    try {
      const saved = await filesApi.save(path, text);
      kept = null;
      setDiskText(null);
      updateScoreFile({ path: saved.path, name: saved.name, savedText: text });
      setRecent(saved.recent);
      note(`Saved ${saved.name}.`);
    } catch (e) {
      fail((e as Error).message);
    }
  }, [checkDisk]);

  /** Reset everything and load the score again - see `reload.ts`. */
  const reloadAndLoad = useCallback(async () => {
    const file = scoreFile();
    await reloadEverything({
      file: { path: file.path, dirty: isDirty(), keptMine: kept !== null },
      peek: (path) => filesApi.peek(path).catch(() => null),
      adopt: (text) => {
        if (text === scoreFile().text) return;
        kept = null;
        setDiskText(null);
        updateScoreFile({ text, savedText: text });
      },
      loadWithReset: () => loadOpenScore(true),
    });
  }, []);

  /**
   * The bar's "Reload from disk": the file as it is on disk now - and when a score is in the
   * runner, everything is reset and the file is loaded again, the same as the Reload button.
   */
  const reloadFromDisk = useCallback(async () => {
    const path = scoreFile().path;
    if (!path || !(await openPath(path))) return;
    if (currentStatus().score === null) return;
    setLoading(true);
    try {
      await loadOpenScore(true);
    } catch (e) {
      fail((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [openPath]);

  const keepMine = useCallback(() => {
    kept = diskText;
    setDiskText(null);
  }, [diskText]);

  const saveAs = useCallback(async () => {
    const current = scoreFile();
    const title = docStore.get().doc?.title?.trim() || "Untitled";
    const fileName = `${title.replace(/[\\/:*?"<>|]+/g, "-")}.yaml`;
    const proposal = current.path ?? (defaultDir ? `${defaultDir}/${fileName}` : fileName);
    const path = await pickScoreToSave(proposal);
    if (path) await saveTo(path);
  }, [defaultDir, saveTo]);

  const save = useCallback(async () => {
    const path = scoreFile().path;
    if (path) await saveTo(path);
    else await saveAs();
  }, [saveTo, saveAs]);

  const newScore = useCallback(async () => {
    if (!(await mayDiscard())) return;
    // The running engine's tracks, so a new score loads without rebuilding anything.
    // An instrument track goes in as one, with the keyboard that plays it and its instrument.
    const tracks = currentStatus().tracks.map((t) => ({
      name: t.name,
      input: t.input_channels[0] ?? t.input_channel,
      input_right: t.input_channels.length > 1 ? t.input_channels[1] : undefined,
      instrument: t.instrument ? (engineInfo?.tracks.find((c) => c.name === t.name)?.instrument ?? { device: null, channel: null, monitor: true }) : undefined,
    }));
    try {
      const text = await filesApi.template(tracks.length > 0 ? tracks : undefined);
      kept = null;
      setDiskText(null);
      updateScoreFile({ text, path: null, name: null, savedText: text });
    } catch (e) {
      fail((e as Error).message);
    }
  }, [mayDiscard, engineInfo]);

  // ⌘N / ⌘O / ⌘S / ⌘⇧S (Ctrl on Windows). Captured before the editor sees them.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(IS_MAC ? e.metaKey : e.ctrlKey) || e.altKey) return;
      const key = e.key.toLowerCase();
      const act = key === "n" ? newScore : key === "o" ? openFile : key === "s" ? (e.shiftKey ? saveAs : save) : null;
      if (!act) return;
      e.preventDefault();
      e.stopPropagation();
      void act();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [newScore, openFile, save, saveAs]);

  const setCountIn = useCallback((bars: number) => setDocCountIn(bars), []);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // The listeners of `loadOpenScore` bump `loadedRev`. With a score in the runner this is
      // Reload: everything is reset and the score loaded again, without asking (the owner's wish).
      if (currentStatus().score !== null) await reloadAndLoad();
      else await loadOpenScore();
    } catch (e) {
      fail((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [reloadAndLoad]);

  // The mix and the count-in are kept in step with the engine both ways, so following them does
  // not make the loaded score stale - a mix that differs from what the engine got from here does
  // (the file changed on disk and opened again), and so does a count-in the runner does not have.
  useEffect(() => {
    const l = () => setLoadedRev((n) => n + 1);
    loadedListeners.add(l);
    return () => void loadedListeners.delete(l);
  }, []);
  const stale =
    inRunner &&
    loadedText !== null &&
    (plain(loadedText) !== plain(scoreFile().text) ||
      !mixIsLoaded(docNow) ||
      (runnerCountIn !== null && runnerCountIn !== countIn));
  const diskChanged = diskText !== null ? (scoreFile().name ?? "The score file") : null;
  return {
    recent,
    newScore,
    openFile,
    openRecent,
    save,
    saveAs,
    load,
    loading,
    countIn,
    setCountIn,
    stale,
    diskChanged,
    reloadFromDisk,
    keepMine,
  };
}
