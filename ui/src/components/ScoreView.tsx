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
//! track name does not flash red on every keystroke. Ctrl+Enter still compiles at once, for the
//! moment one wants to know *now*, and Ctrl+S loads.
//!
//! Loading is the one deliberate act and stays a button: it moves the engine's tempo, time
//! signature and layer length to what the score says, which is not something a keystroke should do
//! by accident.

import { useCallback, useEffect, useRef, useState } from "react";
import { scoreApi } from "../api";
import { useStatusSlice } from "../status";
import { Editor } from "./Editor";
import { Blocks } from "./Blocks";
import { Transport } from "./Transport";
import { ScoreLive } from "./ScoreLive";
import type { CompiledScore, ScoreIssue } from "../types";

const LS_KEY = "looper.yaml";

/** Long enough that a half-typed key does not flash red, short enough to feel immediate. */
const COMPILE_DELAY_MS = 400;

interface Note {
  text: string;
  kind: "engine" | "error";
  seq: number;
}

let noteSeq = 0;

export function ScoreView() {
  const [yamlText, setYamlText] = useState<string>(() => {
    try {
      return localStorage.getItem(LS_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [editorValue] = useState<string>(yamlText); // value pushed INTO the editor, once
  const [compiled, setCompiled] = useState<CompiledScore | null>(null);
  const [errors, setErrors] = useState<ScoreIssue[]>([]);
  const [notes, setNotes] = useState<Note[]>([]);
  /** The text the runner actually holds, so the preview can say "nicht geladen". */
  const [loadedText, setLoadedText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * Whether the runner holds a score at all. Asked of the status rather than of `loadedText`,
   * because a score loaded before this component was mounted (or from the live tab) is just as
   * loaded - the runner is the authority, this view only remembers which text it sent.
   */
  const inRunner = useStatusSlice((s) => s.score !== null);

  const note = useCallback((text: string, kind: Note["kind"]) => {
    setNotes((list) => [{ text, kind, seq: ++noteSeq }, ...list].slice(0, 6));
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem(LS_KEY, yamlText);
    } catch {
      /* ignore quota errors */
    }
  }, [yamlText]);

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
      .load(text)
      .then((loaded) => {
        setCompiled(loaded.score);
        setErrors([]);
        setLoadedText(text);
        note(loaded.message, "engine");
      })
      .catch((e) => note((e as Error).message, "error"))
      .finally(() => setBusy(false));
  }, [yamlText, note]);

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

  return (
    <div className="score-view">
      <Transport loaded={inRunner} onStart={start} onNext={next} onStopAll={stopAll} />

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
          <div className="panel-title">
            Partitur (YAML)
            <span className="muted">
              {errors.length === 0 ? "übersetzt beim Tippen" : `${errors.length} Fehler`}
            </span>
            <div className="spacer" />
            <button className="btn" onClick={() => void compile(yamlText)} title="Ctrl+Enter">
              Übersetzen
            </button>
            <button
              className="btn btn-primary"
              onClick={load}
              disabled={busy || errors.length > 0 || yamlText.trim() === ""}
              title="Ctrl+S — lädt die Partitur in den Runner. Braucht eine laufende, leere Engine."
            >
              Laden
            </button>
          </div>
          <Editor
            value={editorValue}
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
