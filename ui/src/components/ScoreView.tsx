//! The score view: the runner's transport, the score file, and the arrangement editor.
//!
//! ```text
//!  Transport (start / release / stop / back to start)          ── the runner, via the status hub
//!  file bar  (new, open, save, recent · count-in · load)       ── scoreFile.ts + files.rs
//!  Arrangement (Grid | Text, inspector)                        ── arrangement/docStore
//! ```
//!
//! The text and the file it belongs to live in `scoreFile.ts`; the arrangement keeps its document
//! store in step with it (`arrangement/useDoc.ts`), so opening a file here, or anywhere else,
//! replaces what the grid shows and starts a fresh undo history.
//!
//! Loading is the one deliberate act and stays a button: it moves the engine's tempo, time
//! signature and layer length to what the score says, which is not something a keystroke should do
//! by accident.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { filesApi, insideTauri, scoreApi } from "../api";
import { useStatusSlice, currentStatus } from "../status";
import { Transport } from "./Transport";
import type { NoticeEvent, RecentFile } from "../types";
import { IS_MAC, MOD_LABEL } from "../platform";
import { confirmAction, pickScoreToOpen, pickScoreToSave } from "../dialogs";
import { isDirty, scoreFile, updateScoreFile, useScoreFile } from "../scoreFile";
import { Arrangement } from "../arrangement/Arrangement";
import { docStore, useDoc } from "../arrangement/useDoc";

interface Note {
  text: string;
  kind: "engine" | "error";
  seq: number;
}

let noteSeq = 0;

export function ScoreView() {
  const file = useScoreFile();
  const doc = useDoc();
  const [recent, setRecent] = useState<RecentFile[]>([]);
  const [defaultDir, setDefaultDir] = useState<string | null>(null);
  const [notes, setNotes] = useState<Note[]>([]);
  const [busy, setBusy] = useState(false);
  /** Count-in for the next load and start. Sent with `load`, and to the runner when it is idle. */
  const [countIn, setCountIn] = useState(1);
  const runnerCountIn = useStatusSlice((s) => s.score?.count_in ?? null);
  const runnerIdle = useStatusSlice((s) => s.score?.phase === "idle");
  /** Whether the runner holds a score at all - the runner is the authority. */
  const inRunner = useStatusSlice((s) => s.score !== null);
  /** The text the runner was given from here, so the bar can say "changed since loading". */
  const [loadedText, setLoadedText] = useState<string | null>(null);

  const note = useCallback((text: string, kind: Note["kind"]) => {
    setNotes((list) => [{ text, kind, seq: ++noteSeq }, ...list].slice(0, 6));
  }, []);

  // What the runner says on its own at a section boundary - no button reply carries it.
  useEffect(() => {
    if (!insideTauri()) return;
    let off: (() => void) | null = null;
    let disposed = false;
    void listen<NoticeEvent>("looper://notice", (e) => note(e.payload.text, "engine")).then((unlisten) => {
      if (disposed) unlisten();
      else off = unlisten;
    });
    return () => {
      disposed = true;
      off?.();
    };
  }, [note]);

  // --- files ---------------------------------------------------------------
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

  /** Replace the whole text, e.g. after Open or New. The arrangement follows the file layer. */
  const replaceText = useCallback((text: string, path: string | null, name: string | null, saved: string | null) => {
    updateScoreFile({ text, path, name, savedText: saved });
  }, []);

  /** "Discard unsaved changes?" - true when there is nothing to lose or the user agrees. */
  const mayDiscard = useCallback(async () => {
    if (!isDirty()) return true;
    const name = scoreFile().name ?? "the untitled score";
    return confirmAction(`Discard unsaved changes to ${name}?`, "Unsaved changes", "Discard");
  }, []);

  const openPath = useCallback(
    async (path: string) => {
      try {
        const opened = await filesApi.open(path);
        replaceText(opened.yaml, opened.path, opened.name, opened.yaml);
        setRecent(opened.recent);
        setLoadedText(null);
        note(`Opened ${opened.name}.`, "engine");
      } catch (e) {
        note((e as Error).message, "error");
        filesApi.forget(path).then(setRecent).catch(() => undefined);
      }
    },
    [replaceText, note],
  );

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

  const saveTo = useCallback(
    async (path: string) => {
      // Grid edits on the wire belong in the file.
      await docStore.settled();
      const text = scoreFile().text;
      try {
        const saved = await filesApi.save(path, text);
        updateScoreFile({ path: saved.path, name: saved.name, savedText: text });
        setRecent(saved.recent);
        note(`Saved ${saved.name}.`, "engine");
      } catch (e) {
        note((e as Error).message, "error");
      }
    },
    [note],
  );

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
      replaceText(text, null, null, text);
      setLoadedText(null);
    } catch (e) {
      note((e as Error).message, "error");
    }
  }, [mayDiscard, replaceText, note]);

  // ⌘N / ⌘O / ⌘S / ⌘⇧S (Ctrl on Windows). Captured before the editor sees them.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(IS_MAC ? e.metaKey : e.ctrlKey) || e.altKey) return;
      const key = e.key.toLowerCase();
      const act =
        key === "n" ? newScore : key === "o" ? openFile : key === "s" ? (e.shiftKey ? saveAs : save) : null;
      if (!act) return;
      e.preventDefault();
      e.stopPropagation();
      void act();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [newScore, openFile, save, saveAs]);

  // --- the runner ----------------------------------------------------------
  const load = useCallback(async () => {
    setBusy(true);
    await docStore.flush();
    await docStore.settled();
    const text = scoreFile().text;
    scoreApi
      .load(text, countIn)
      .then((loaded) => {
        setLoadedText(text);
        note(loaded.message, "engine");
      })
      .catch((e) => note((e as Error).message, "error"))
      .finally(() => setBusy(false));
  }, [note, countIn]);

  /** Every transport press answers with a German sentence - the runner never fails, it explains. */
  const say = useCallback(
    (work: () => Promise<string>) => {
      work()
        .then((message) => note(message, "engine"))
        .catch((e) => note((e as Error).message, "error"));
    },
    [note],
  );

  const start = useCallback(() => say(() => scoreApi.start()), [say]);
  const next = useCallback(() => say(() => scoreApi.next()), [say]);
  const stopAll = useCallback(() => say(() => scoreApi.stopAll()), [say]);
  const goto = useCallback((section: number) => say(() => scoreApi.goto(section)), [say]);
  const rewind = useCallback(() => {
    void confirmAction(
      "Back to start clears ALL tracks - every recorded loop is gone - and returns to the first section.",
      "Back to start",
      "Clear tracks & rewind",
    ).then((yes) => {
      if (yes) say(() => scoreApi.rewind());
    });
  }, [say]);

  const stale = inRunner && loadedText !== null && loadedText !== file.text;
  const canLoad = !busy && doc.errors.length === 0 && doc.inSync && file.text.trim() !== "";

  return (
    <div className="score-view">
      <Transport loaded={inRunner} onStart={start} onNext={next} onStopAll={stopAll} onRewind={rewind} />

      {notes.length > 0 && (
        <section className="live-notes">
          <div
            className={`live-note live-note-${notes[0].kind} live-note-new`}
            onClick={() => setNotes([])}
            title="Click to clear"
          >
            {notes[0].text}
          </div>
          {notes.length > 1 && (
            <ul className="live-note-old">
              {notes.slice(1).map((n) => (
                <li key={n.seq} className={`live-note-${n.kind}`}>
                  {n.text}
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      <div className="panel-title score-files">
        <button className="btn" onClick={() => void newScore()} title={`New score (${MOD_LABEL}+N)`}>
          New
        </button>
        <button className="btn" onClick={() => void openFile()} title={`Open a score file (${MOD_LABEL}+O)`}>
          Open…
        </button>
        <button className="btn" onClick={() => void save()} title={`Save (${MOD_LABEL}+S)`}>
          Save
        </button>
        <button className="btn" onClick={() => void saveAs()} title={`Save as (${MOD_LABEL}+Shift+S)`}>
          Save as…
        </button>
        <select
          className="score-recent"
          value=""
          onChange={(e) => {
            if (e.target.value) void openRecent(e.target.value);
          }}
          title="Recently opened scores"
        >
          <option value="">Open recent…</option>
          {recent.map((r) => (
            <option key={r.path} value={r.path} disabled={!r.exists}>
              {r.name}
              {r.exists ? "" : " (missing)"}
            </option>
          ))}
        </select>
        <span className="score-file-name" title={file.path ?? "Not saved yet"}>
          {file.name ?? "Untitled"}
          <span className={isDirty(file) ? "file-dirty" : "file-clean"}>{isDirty(file) ? " • unsaved" : " · saved"}</span>
        </span>
        {stale && (
          <span className="badge badge-warn" title="The score was changed after it was loaded into the runner.">
            changed since loading
          </span>
        )}
        <label className="score-countin" title="Bars of click before the first section (1 to 8)">
          Count-in
          <input
            type="number"
            min={1}
            max={8}
            value={runnerIdle && runnerCountIn !== null ? runnerCountIn : countIn}
            onChange={(e) => {
              const bars = Math.max(1, Math.min(8, Number(e.target.value) || 1));
              setCountIn(bars);
              if (runnerIdle) say(() => scoreApi.countIn(bars));
            }}
          />
          bars
        </label>
        <button
          className="btn btn-primary"
          onClick={() => void load()}
          disabled={!canLoad}
          title="Loads the score into the runner. Needs a running engine with empty tracks."
        >
          Load
        </button>
      </div>

      <Arrangement onGoto={goto} onSave={() => void save()} fileName={file.name} />
    </div>
  );
}
