//! The block preview: the compiled score as one card per section.
//!
//! **Read-only, and that is a rule rather than a shortcut.** The text is the score; a card that
//! could be edited would be a second place where a section is defined, and the two would disagree
//! the first time somebody types in the editor while a card is open. The only thing a card *does*
//! is jump to its section, which changes nothing about the score.
//!
//! While the runner plays, three things are drawn on top of the static picture:
//!
//! * the sounding section, with the bar inside the current pass and the pass number;
//! * the **armed** section, with how far away it still is - "scharf" alone does not say when;
//! * whether a jump is possible at all. The runner refuses one while a change is armed (its
//!   commands are already in the audio thread with timestamps on them), so the card says so before
//!   the click instead of failing quietly afterwards.

import { useEffect, useRef } from "react";
import { gotoAddress } from "../midi/addresses";
import { useStatusSlice } from "../status";
import { SCORE_STATE_LABEL, type CompiledScore, type CompiledSection, type LooperStatus } from "../types";

interface Props {
  score: CompiledScore | null;
  /** True while the score in the editor is not the one the runner holds. */
  stale: boolean;
  /** Zero-based. The answer is a German sentence, which the caller shows. */
  onGoto: (section: number) => void;
}

/** Everything the preview needs from the running score, flattened. */
interface Live {
  section: number | null;
  bar: number;
  beat: number;
  pass: number;
  armedSection: number | null;
  armedEnd: boolean;
  armedText: string;
  /** True while the runner would refuse a jump. */
  blocked: boolean;
  running: boolean;
}

const IDLE: Live = {
  section: null,
  bar: 0,
  beat: 0,
  pass: 0,
  armedSection: null,
  armedEnd: false,
  armedText: "",
  blocked: false,
  running: false,
};

function pick(s: LooperStatus): Live {
  const score = s.score;
  if (!score) return IDLE;
  return {
    section: score.section,
    bar: score.bar,
    beat: score.beat,
    pass: score.pass,
    armedSection: score.armed?.section ?? null,
    armedEnd: score.armed !== null && score.armed.section === null,
    armedText: score.armed?.text ?? "",
    blocked: score.armed !== null,
    running: score.phase === "count_in" || score.phase === "running",
  };
}

function same(a: Live, b: Live): boolean {
  return (
    a.section === b.section &&
    a.bar === b.bar &&
    a.beat === b.beat &&
    a.pass === b.pass &&
    a.armedSection === b.armedSection &&
    a.armedEnd === b.armedEnd &&
    a.armedText === b.armedText &&
    a.blocked === b.blocked &&
    a.running === b.running
  );
}

export function Blocks({ score, stale, onGoto }: Props) {
  const live = useStatusSlice(pick, same);

  // A score is a row that scrolls, and the sounding section is the one thing that must never be off
  // screen - by section four nobody is going to reach for a scrollbar. Only on a *change* of
  // section or armed target, so a scroll by hand survives the rest of the section.
  const row = useRef<HTMLDivElement | null>(null);
  // The sounding section is what has to stay visible; the armed one is usually its neighbour and
  // comes along. During the count-in nothing sounds yet, so the armed first section stands in.
  const focus = live.section ?? live.armedSection;
  // The section count is a dependency too: a score that arrives *after* the runner is already
  // playing would otherwise be scrolled before its cards exist.
  const cardCount = score?.sections.length ?? 0;
  useEffect(() => {
    if (focus === null || !row.current) return;
    const card = row.current.querySelector(`[data-section="${focus}"]`);
    card?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "center" });
  }, [focus, cardCount]);

  if (!score) {
    return (
      <section className="blocks">
        <div className="panel-title">Blockvorschau</div>
        <div className="blocks-empty">
          Noch keine übersetzte Partitur. Links YAML schreiben — übersetzt wird beim Tippen.
        </div>
      </section>
    );
  }

  const beatsPerBar = score.beats_per_bar || 4;

  return (
    <section className="blocks">
      <div className="panel-title">
        Blockvorschau
        <span className="muted">
          {score.title} · {score.bpm} BPM · {score.time_signature} · {score.sections.length} Sektionen ·{" "}
          {score.tracks.map((t) => t.name).join(", ")}
        </span>
        {stale && (
          <span className="badge badge-warn" title="Der Text im Editor ist neuer als das, was der Runner spielt.">
            nicht geladen
          </span>
        )}
        <div className="spacer" />
        <span className="muted">nur Anzeige — der Text ist die Quelle</span>
      </div>
      <div className="blocks-row" ref={row}>
        {score.sections.map((section) => (
          <SectionCard
            key={section.index}
            section={section}
            tracks={score.tracks.map((t) => t.name)}
            beatsPerBar={beatsPerBar}
            live={live}
            onGoto={onGoto}
          />
        ))}
        {live.armedEnd && (
          <article className="card card-pending card-end">
            <header className="card-head">
              <span className="card-index">▮</span>
              <h3 className="card-title">Ende</h3>
            </header>
            <div className="pending-label">{live.armedText}</div>
          </article>
        )}
      </div>
    </section>
  );
}

interface CardProps {
  section: CompiledSection;
  /** Track order of the score, so every card lists its tracks in the same order. */
  tracks: string[];
  beatsPerBar: number;
  live: Live;
  onGoto: (section: number) => void;
}

function SectionCard({ section, tracks, beatsPerBar, live, onGoto }: CardProps) {
  const active = live.section === section.index;
  const armed = live.armedSection === section.index;
  const bar = active ? live.bar : 0;
  const beat = active ? live.beat : 0;
  const progress = active && section.bars > 0 ? Math.min(1, ((bar - 1) * beatsPerBar + beat) / (section.bars * beatsPerBar)) : 0;

  const classes = ["card", "card-jump", active && "card-active", armed && "card-pending"].filter(Boolean).join(" ");
  const jumpTitle = !live.running
    ? "Die Partitur läuft nicht — erst starten."
    : live.blocked
      ? `Sprung nicht möglich: ${live.armedText}. Ein armierter Wechsel liegt schon in der Engine.`
      : `Zu „${section.id}" springen`;

  return (
    <article
      className={classes}
      data-section={section.index}
      // A card is a jump, so it is bindable like every other control: `transport.goto.<n>`. See
      // `midi/addresses.ts` for how the address gets from here into the learn mode.
      data-midi={gotoAddress(section.index)}
      onClick={() => onGoto(section.index)}
      title={jumpTitle}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter") onGoto(section.index);
      }}
      aria-disabled={live.blocked || !live.running}
    >
      <header className="card-head">
        <span className="card-index">{section.index + 1}</span>
        <h3 className="card-title" title={section.source_line ? `Zeile ${section.source_line}` : undefined}>
          {section.id}
        </h3>
        <span className="card-bars">{section.bars} Takte</span>
      </header>
      <div className="card-badges">
        <span
          className={`badge ${section.autorelease ? "badge-auto" : "badge-manual"}`}
          title={
            section.autorelease
              ? "Der Folgewechsel liegt sample-genau am Sektionsende und wird sofort armiert."
              : "Die Sektion loopt, bis der Release-Knopf gedrückt wird."
          }
        >
          {section.autorelease ? "autorelease" : "Release"}
        </span>
        <span
          className="badge badge-quant"
          title={
            section.quantize === "bar"
              ? "Ein Release rastet auf die nächste Taktgrenze ein."
              : "Ein Release rastet auf das Ende des laufenden Durchlaufs ein."
          }
        >
          quantize: {section.quantize}
        </span>
      </div>
      <ul className="card-tracks">
        {tracks.map((name) => {
          const state = section.tracks[name] ?? "stop";
          return (
            <li key={name}>
              <span className="track-name">{name}</span>
              <span className={`state state-${state}`}>{SCORE_STATE_LABEL[state] ?? state}</span>
            </li>
          );
        })}
      </ul>
      {armed && <div className="pending-label">{live.armedText}</div>}
      {active && (
        <div className="card-progress">
          <div className="progress">
            <div className="progress-fill" style={{ width: `${progress * 100}%` }} />
          </div>
          <div className="progress-text">
            Takt {bar}/{section.bars} · Durchlauf {live.pass}
          </div>
          <div className="beats">
            {Array.from({ length: beatsPerBar }, (_, i) => (
              <span key={i} className={`beat-dot ${beat === i + 1 ? "beat-on" : ""} ${i === 0 ? "beat-one" : ""}`}>
                {i + 1}
              </span>
            ))}
          </div>
        </div>
      )}
    </article>
  );
}
