//! The score transport: start, release, stop - and where in the score we are.
//!
//! One component for both views, because it is the same three buttons and the same four numbers.
//! `full` heads the score view; `compact` is the strip the live view puts above its track cards, so
//! that running a score never means giving up the meters and the layers (see `App.tsx`).
//!
//! **The release button is the one control that matters on stage.** It is deliberately the largest
//! thing on either screen: while a section loops, this is the button that says "now" - everything
//! else in this program can wait until the song is over. Its keys follow the CLI: space or `n`.
//!
//! What it renders comes from the status hub, not from a reply: the runner's own view rides along
//! in every status event, so the section, the bar inside it and the level meters are always from
//! the same instant.

import { useEffect } from "react";
import { useStatusSlice } from "../status";
import { PHASE_LABEL, type LooperStatus, type Phase } from "../types";

interface Props {
  /** True once a score sits in the runner. Without one there is nothing to start. */
  loaded: boolean;
  onStart: () => void;
  onNext: () => void;
  onStopAll: () => void;
  /** The strip inside the live view rather than the header of the score view. */
  compact?: boolean;
}

/** Everything the transport draws, flattened so an unchanged section costs no render. */
interface View {
  phase: Phase | "none";
  phaseLabel: string;
  section: number | null;
  sectionId: string;
  sectionCount: number;
  bar: number;
  barsTotal: number;
  pass: number;
  armed: string;
  armedCountIn: boolean;
  title: string;
}

const NOTHING: View = {
  phase: "none",
  phaseLabel: "keine Partitur",
  section: null,
  sectionId: "",
  sectionCount: 0,
  bar: 0,
  barsTotal: 0,
  pass: 0,
  armed: "",
  armedCountIn: false,
  title: "",
};

function pick(s: LooperStatus): View {
  const score = s.score;
  if (!score) return NOTHING;
  return {
    phase: score.phase,
    // The UI's own word, not the engine's: the Rust side spells German without umlauts because the
    // same strings go to a console. See `SCORE_STATE_LABEL` in types.ts.
    phaseLabel: PHASE_LABEL[score.phase] ?? score.phase_label,
    section: score.section,
    sectionId: score.section_id,
    sectionCount: score.section_count,
    bar: score.bar,
    barsTotal: score.bars_total,
    pass: score.pass,
    armed: score.armed?.text ?? "",
    armedCountIn: score.armed?.count_in ?? false,
    title: score.title,
  };
}

function same(a: View, b: View): boolean {
  return (
    a.phase === b.phase &&
    a.section === b.section &&
    a.sectionId === b.sectionId &&
    a.sectionCount === b.sectionCount &&
    a.bar === b.bar &&
    a.barsTotal === b.barsTotal &&
    a.pass === b.pass &&
    a.armed === b.armed &&
    a.armedCountIn === b.armedCountIn &&
    a.title === b.title
  );
}

/** True while the score owns the transport: started and not over. */
function playing(phase: Phase | "none"): boolean {
  return phase === "count_in" || phase === "running";
}

/** A key press that belongs to a text field is none of our business. */
function typing(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  if (!element) return false;
  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    element.isContentEditable ||
    element.closest(".cm-editor") !== null
  );
}

export function Transport({ loaded, onStart, onNext, onStopAll, compact = false }: Props) {
  const view = useStatusSlice(pick, same);
  const runs = playing(view.phase);
  const canStart = loaded && (view.phase === "idle" || view.phase === "none");

  // The same keys the CLI has: space or `n` releases, `s` stops everything. `s` carries Shift here
  // because the live view already spends the bare letter on "stop the selected track", and the two
  // must not be one keystroke apart - one ends a track, the other ends the song.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
      if (typing(e.target)) return;
      const key = e.key.toLowerCase();
      if ((key === " " || key === "n") && !e.shiftKey) {
        // preventDefault also keeps space from re-clicking whatever button has focus.
        e.preventDefault();
        if (runs) onNext();
        else if (canStart) onStart();
        return;
      }
      if (key === "s" && e.shiftKey && runs) {
        e.preventDefault();
        onStopAll();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [runs, canStart, onNext, onStart, onStopAll]);

  const position =
    view.section !== null
      ? `Sektion ${view.section + 1}/${view.sectionCount}`
      : loaded
        ? view.phaseLabel
        : "keine Partitur geladen";

  return (
    <section className={`score-transport${compact ? " score-transport-compact" : ""}`}>
      <div className={`score-where${view.armedCountIn ? " score-where-countin" : ""}`}>
        <div className="score-where-line">
          <span className="score-where-position">{position}</span>
          {view.sectionId && <span className="score-where-id">{view.sectionId}</span>}
        </div>
        <div className="score-where-numbers mono">
          {view.section !== null ? (
            <>
              <b>
                {view.bar}/{view.barsTotal}
              </b>{" "}
              Takte · Durchlauf <b>{view.pass}</b>
            </>
          ) : (
            <span className="muted">{loaded ? view.title : "—"}</span>
          )}
        </div>
      </div>

      <div className={`score-armed${view.armed ? " score-armed-on" : ""}`} aria-live="polite">
        {view.armed || (runs ? "kein Wechsel armiert" : "")}
      </div>

      <div className="score-buttons">
        <button className="btn btn-score btn-score-start" onClick={onStart} disabled={!canStart}>
          Start
          <kbd>Leer</kbd>
        </button>
        <button
          className="btn btn-score btn-score-next"
          onClick={onNext}
          disabled={!runs}
          title="Der Release-Knopf: armiert den Wechsel in die nächste Sektion. Leertaste oder N."
        >
          Release
          <kbd>Leer / N</kbd>
        </button>
        <button
          className="btn btn-score btn-score-stop"
          onClick={onStopAll}
          disabled={!runs}
          title="Alle Tracks stoppen und den Lauf beenden. Umschalt+S."
        >
          Alles stoppen
          <kbd>⇧S</kbd>
        </button>
      </div>
    </section>
  );
}
