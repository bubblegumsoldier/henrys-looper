//! The score view: write the score, see what it compiles to, run it.
//!
//! ```text
//!  YAML  ──(entprellt)──►  score_compile  ──►  Marker im Text + Blockvorschau
//!    │                                              │
//!    └──────────► score_load ──► Runner ──► Status ──┘  (Transport, laufende Sektion)
//! ```
//!
//! **Why it compiles while you type rather than on a key press.** The compiler is a pure function
//! over a text buffer: it opens no device, allocates nothing the audio thread can see, and a score
//! is a few dozen lines - running it continuously costs nothing measurable. And errors are the
//! point of this editor: the compiler reports *every* problem at once, each with a line, a column
//! and often "meintest du 'bass'?", so a mistake should be under the cursor while the finger is
//! still on the key, not after a trip to a button. The 400 ms of delay exist only so a half-typed
//! track name does not flash red on every keystroke. Ctrl+Enter (⌘+Enter on the Mac) still compiles at once, for the
//! moment one wants to know *now*, and Ctrl+Shift+Enter loads. Ctrl+N/O/S/Shift+S are New, Open,
//! Save and Save as (⌘ on the Mac); the file itself is `scoreFile.ts` plus `files.rs`.
//!
//! Loading is the one deliberate act and stays a button: it moves the engine's tempo, time
//! signature and layer length to what the score says, which is not something a keystroke should do
//! by accident.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { filesApi, insideTauri, scoreApi } from "../api";
import { useStatusSlice } from "../status";
import { Editor } from "./Editor";
import { Blocks } from "./Blocks";
import { Transport } from "./Transport";
import { ScoreLive } from "./ScoreLive";
import type { CompiledScore, NoticeEvent, RecentFile, ScoreIssue } from "../types";
import { IS_MAC, MOD_LABEL } from "../platform";
import { confirmAction, pickScoreToOpen, pickScoreToSave } from "../dialogs";
import { currentStatus } from "../status";
import { isDirty, scoreFile, updateScoreFile, useScoreFile } from "../scoreFile";

/** Long enough that a half-typed key does not flash red, short enough to feel immediate. */
const COMPILE_DELAY_MS = 400;

interface Note {
  text: string;
  kind: "engine" | "error";
  seq: number;
}

let noteSeq = 0;

export function ScoreView() {
  // The text and the file it belongs to live in `scoreFile.ts`, outside this component, so the
  // window title and the close guard know about unsaved changes while the live tab is open.
  const file = useScoreFile();
  const yamlText = file.text;
  const setYamlText = useCallback((text: string) => updateScoreFile({ text }), []);
  /** What is pushed INTO the editor. A new `rev` remounts it - after Open and New, whose text
   *  replaces the document (and its undo history) as a whole. */
  const [editorValue, setEditorValue] = useState(() => ({ text: scoreFile().text, rev: 0 }));
  const [recent, setRecent] = useState<RecentFile[]>([]);
  const [defaultDir, setDefaultDir] = useState<string | null>(null);
  const [compiled, setCompiled] = useState<CompiledScore | null>(null);
  const [errors, setErrors] = useState<ScoreIssue[]>([]);
  const [notes, setNotes] = useState<Note[]>([]);
  /** The text the runner actually holds, so the preview can say "nicht geladen". */
  const [loadedText, setLoadedText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** Count-in for the next load and start. Sent with `load`, and to the runner when it is idle. */
  const [countIn, setCountIn] = useState(1);
  const runnerCountIn = useStatusSlice((s) => s.score?.count_in ?? null);
  const runnerIdle = useStatusSlice((s) => s.score?.phase === "idle");
  /**
   * Whether the runner holds a score at all. Asked of the status rather than of `loadedText`,
   * because a score loaded before this component was mounted (or from the live tab) is just as
   * loaded - the runner is the authority, this view only remembers which text it sent.
   */
  const inRunner = useStatusSlice((s) => s.score !== null);

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

  /** Replace the whole text, e.g. after Open or New. */
  const replaceText = useCallback((text: string, path: string | null, name: string | null, saved: string | null) => {
    updateScoreFile({ text, path, name, savedText: saved });
    setEditorValue((v) => ({ text, rev: v.rev + 1 }));
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
    const title = compiled?.title?.trim() || "Untitled";
    const fileName = `${title.replace(/[\\/:*?"<>|]+/g, "-")}.yaml`;
    const proposal = current.path ?? (defaultDir ? `${defaultDir}/${fileName}` : fileName);
    const path = await pickScoreToSave(proposal);
    if (path) await saveTo(path);
  }, [compiled, defaultDir, saveTo]);

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

  // --- compile -------------------------------------------------------------
  // One in flight at a time, and only the newest answer counts: typing while a compile is on the
  // wire must not resurrect the markers of the text before it.
  const run = useRef(0);
  const compile = useCallback(
    async (text: string) => {
      const ticket = ++run.current;
      if (text.trim() === "") {
        setErrors((prev) => (prev.length === 0 ? prev : []));
        setCompiled(null);
        return;
      }
      try {
        const result = await scoreApi.compile(text);
        if (ticket !== run.current) return;
        if (result.ok && result.score) {
          // Keep the identity when there was nothing to clear: a fresh `[]` every 400 ms would
          // re-run the editor's diagnostics for no reason.
          setErrors((prev) => (prev.length === 0 ? prev : []));
          setCompiled(result.score);
        } else {
          setErrors(result.errors);
          // The last good picture stays on screen. A preview that empties on every typo would
          // flicker through the whole editing session.
        }
      } catch (e) {
        if (ticket !== run.current) return;
        note((e as Error).message, "error");
      }
    },
    [note],
  );

  useEffect(() => {
    const timer = window.setTimeout(() => void compile(yamlText), COMPILE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [yamlText, compile]);

  // The same affordance `status.ts` has for the live view: while `npm run dev` runs, this view can
  // be driven from the console without an engine, without a device and without a sound -
  // `pushScore(compiled)` fills the preview, `pushErrors([...])` puts markers in the text. Stripped
  // from the production build.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const w = window as unknown as {
      pushScore?: (s: CompiledScore | null) => void;
      pushErrors?: (e: ScoreIssue[]) => void;
    };
    w.pushScore = setCompiled;
    w.pushErrors = setErrors;
    return () => {
      delete w.pushScore;
      delete w.pushErrors;
    };
  }, []);

  // --- the runner ----------------------------------------------------------
  const load = useCallback(() => {
    setBusy(true);
    const text = yamlText;
    scoreApi
      .load(text, countIn)
      .then((loaded) => {
        setCompiled(loaded.score);
        setErrors([]);
        setLoadedText(text);
        note(loaded.message, "engine");
      })
      .catch((e) => note((e as Error).message, "error"))
      .finally(() => setBusy(false));
  }, [yamlText, note, countIn]);

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

  return (
    <div className="score-view">
      <Transport loaded={inRunner} onStart={start} onNext={next} onStopAll={stopAll} onRewind={rewind} />

      {notes.length > 0 && (
        <section className="live-notes">
          <div
            className={`live-note live-note-${notes[0].kind} live-note-new`}
            onClick={() => setNotes([])}
            title="Klick leert die Liste"
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

      <main className="main">
        <section className="editor">
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
              {isDirty(file) ? " •" : ""}
            </span>
          </div>
          <div className="panel-title">
            Partitur (YAML)
            <span className="muted">
              {errors.length === 0 ? "übersetzt beim Tippen" : `${errors.length} Fehler`}
            </span>
            <div className="spacer" />
            <button className="btn" onClick={() => void compile(yamlText)} title={`${MOD_LABEL}+Enter`}>
              Übersetzen
            </button>
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
              onClick={load}
              disabled={busy || errors.length > 0 || yamlText.trim() === ""}
              title={`${MOD_LABEL}+Shift+Enter — loads the score into the runner. Needs a running engine with empty tracks.`}
            >
              Laden
            </button>
          </div>
          <Editor
            key={editorValue.rev}
            value={editorValue.text}
            onChange={setYamlText}
            errors={errors}
            onCompile={() => void compile(yamlText)}
            onLoad={load}
          />
          <div className={`errors ${errors.length ? "" : "errors-empty"}`}>
            {errors.length === 0 ? (
              <span className="ok">✓ Keine Fehler</span>
            ) : (
              <ul>
                {errors.map((e, i) => (
                  <li key={i}>
                    <span className="err-line">
                      {e.line ? `Zeile ${e.line}${e.column ? `:${e.column}` : ""}` : "—"}
                    </span>
                    <span className="err-msg">{e.message}</span>
                    {e.suggestion && <span className="err-sugg">{e.suggestion}</span>}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </section>

        <Blocks score={compiled} stale={loadedText !== null && loadedText !== yamlText} onGoto={goto} />
        <ScoreLive />
      </main>
    </div>
  );
}
