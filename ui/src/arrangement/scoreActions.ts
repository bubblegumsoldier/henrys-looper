//! The score file and the runner's "Load", for the Arrangement mode.
//!
//! What the old score view did besides editing: New (from the template, with the engine's tracks),
//! Open, Open recent, Save, Save as - each asking before unsaved changes are thrown away - and
//! loading the score into the runner with a count-in. The text and the file it belongs to live in
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
import { fail, note, say } from "../shell/notes";
import { setLoadedScore } from "../shell/scoreModel";
import { currentStatus, useStatusSlice } from "../status";
import type { RecentFile } from "../types";
import { docStore } from "./useDoc";
import { markLoaded, withoutMixer } from "../mixer/scoreMix";

export interface ScoreActions {
  recent: RecentFile[];
  newScore: () => Promise<void>;
  openFile: () => Promise<void>;
  openRecent: (path: string) => Promise<void>;
  save: () => Promise<void>;
  saveAs: () => Promise<void>;
  load: () => Promise<void>;
  loading: boolean;
  /** Count-in bars for the next start; the runner's own value while it is idle. */
  countIn: number;
  setCountIn: (bars: number) => void;
  /** The runner holds a score, and it is not the text in the editor. */
  stale: boolean;
}

/** The text the runner was last given from here. */
let loadedText: string | null = null;

export function useScoreActions(): ScoreActions {
  const [recent, setRecent] = useState<RecentFile[]>([]);
  const [defaultDir, setDefaultDir] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [countIn, setCountInRaw] = useState(1);
  const [, setLoadedRev] = useState(0);
  const runnerCountIn = useStatusSlice((s) => s.score?.count_in ?? null);
  const runnerIdle = useStatusSlice((s) => s.score?.phase === "idle");
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

  const openPath = useCallback(async (path: string) => {
    try {
      const opened = await filesApi.open(path);
      updateScoreFile({ text: opened.yaml, path: opened.path, name: opened.name, savedText: opened.yaml });
      setRecent(opened.recent);
      note(`Opened ${opened.name}.`);
    } catch (e) {
      fail((e as Error).message);
      filesApi.forget(path).then(setRecent).catch(() => undefined);
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
    try {
      const saved = await filesApi.save(path, text);
      updateScoreFile({ path: saved.path, name: saved.name, savedText: text });
      setRecent(saved.recent);
      note(`Saved ${saved.name}.`);
    } catch (e) {
      fail((e as Error).message);
    }
  }, []);

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
    const tracks = currentStatus().tracks.map((t) => ({
      name: t.name,
      input: t.input_channels[0] ?? t.input_channel,
      input_right: t.input_channels.length > 1 ? t.input_channels[1] : undefined,
    }));
    try {
      const text = await filesApi.template(tracks.length > 0 ? tracks : undefined);
      updateScoreFile({ text, path: null, name: null, savedText: text });
    } catch (e) {
      fail((e as Error).message);
    }
  }, [mayDiscard]);

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

  const shownCountIn = runnerIdle && runnerCountIn !== null ? runnerCountIn : countIn;
  const setCountIn = useCallback(
    (bars: number) => {
      const v = Math.max(1, Math.min(8, Math.round(bars) || 1));
      setCountInRaw(v);
      if (runnerIdle) say(() => scoreApi.countIn(v));
    },
    [runnerIdle],
  );

  const load = useCallback(async () => {
    setLoading(true);
    try {
      await docStore.flush();
      await docStore.settled();
      const text = scoreFile().text;
      const loaded = await scoreApi.load(text, shownCountIn);
      loadedText = text;
      // The host has applied the score's `mixer:`; from now on fader and section move together.
      markLoaded();
      setLoadedRev((n) => n + 1);
      setLoadedScore(loaded.score);
      note(loaded.message);
    } catch (e) {
      fail((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [shownCountIn]);

  // The mix is kept in step with the engine both ways, so it does not make the loaded score stale.
  const stale = inRunner && loadedText !== null && withoutMixer(loadedText) !== withoutMixer(scoreFile().text);
  return { recent, newScore, openFile, openRecent, save, saveAs, load, loading, countIn: shownCountIn, setCountIn, stale };
}
