//! The score editor, as it was written against the Python backend.
//!
//! **Nothing here reaches the engine yet.** `compile`, `load` and the transport are phase 3; the
//! Rust side has no compiler and no runner, so `scoreApi` rejects every call locally with one
//! German sentence instead of hammering a command that does not exist. The buttons are disabled
//! for the same reason - a disabled button explains itself, a failing one only annoys.
//!
//! The editor itself still works: YAML can be written and is kept in local storage, so the text
//! survives until the compiler arrives.

import { useCallback, useEffect, useState } from "react";
import { SCORE_AVAILABLE, SCORE_HINT, scoreApi } from "../api";
import { Editor } from "./Editor";
import { Blocks } from "./Blocks";
import { MidiMonitor } from "./MidiMonitor";
import { IDLE_STATE, type LogRow, type MidiRow, type Score, type ScoreErr, type StateEvent } from "../types";

const LS_KEY = "looper.yaml";

// Monotonic row id; seeded from the clock so it stays unique across Vite HMR module reloads.
let seq = Date.now();

export function ScoreView() {
  const [yamlText, setYamlText] = useState<string>(() => {
    try {
      return localStorage.getItem(LS_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [editorValue] = useState<string>(yamlText); // value pushed INTO the editor
  const [compiled, setCompiled] = useState<Score | null>(null);
  const [errors, setErrors] = useState<ScoreErr[]>([]);
  const [state] = useState<StateEvent>(IDLE_STATE);
  const [midiRows, setMidiRows] = useState<MidiRow[]>([]);
  const [logs, setLogs] = useState<LogRow[]>([]);
  const [banner, setBanner] = useState<string | null>(null);

  useEffect(() => {
    try {
      localStorage.setItem(LS_KEY, yamlText);
    } catch {
      /* ignore quota errors */
    }
  }, [yamlText]);

  // Kept so switching the compiler on again is a one-line change, not a rewrite.
  const compile = useCallback(async () => {
    if (!SCORE_AVAILABLE) {
      setBanner(SCORE_HINT);
      return;
    }
    try {
      const res = await scoreApi.compile(yamlText);
      if (res.ok) {
        setErrors([]);
        setCompiled(res.score);
        setLogs((l) => [
          { type: "log" as const, level: "info", message: "Kompiliert.", seq: ++seq, ts: Date.now() },
          ...l,
        ]);
      } else {
        setErrors(res.errors);
      }
    } catch (e) {
      setBanner((e as Error).message);
    }
  }, [yamlText]);

  return (
    <div className="score-view">
      {banner && (
        <div className="banner" role="alert" onClick={() => setBanner(null)}>
          {banner}
        </div>
      )}
      <div className="phase-hint">Partitur, Blockvorschau und Transport {SCORE_HINT}</div>
      <main className="main">
        <section className="editor">
          <div className="panel-title">
            Partitur (YAML)
            <span className="muted">wird gespeichert, aber noch nicht kompiliert</span>
            <div className="spacer" />
            <button className="btn" onClick={() => void compile()} disabled={!SCORE_AVAILABLE} title={SCORE_HINT}>
              Kompilieren
            </button>
            <button className="btn btn-primary" disabled title={SCORE_HINT}>
              Laden
            </button>
          </div>
          <Editor
            value={editorValue}
            onChange={setYamlText}
            errors={errors}
            onCompile={() => void compile()}
            onLoad={() => setBanner(SCORE_HINT)}
          />
          <div className={`errors ${errors.length ? "" : "errors-empty"}`}>
            {errors.length === 0 ? (
              <span className="ok">✓ Keine Fehler</span>
            ) : (
              <ul>
                {errors.map((e, i) => (
                  <li key={i}>
                    <span className="err-line">{e.line ? `Zeile ${e.line}${e.column ? `:${e.column}` : ""}` : "—"}</span>
                    <span className="err-msg">{e.message}</span>
                    {e.suggestion && <span className="err-sugg">{e.suggestion}</span>}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </section>
        <Blocks score={compiled} state={state} stale={false} pool={null} />
        <MidiMonitor rows={midiRows} logs={logs} onClear={() => setMidiRows([])} />
      </main>
    </div>
  );
}
