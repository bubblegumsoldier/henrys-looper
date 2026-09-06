import { isCountIn, type Score, type ScoreSection, type StateEvent, type TrackState } from "../types";

interface Props {
  score: Score | null;
  state: StateEvent;
  stale: boolean;
}

const STATE_LABEL: Record<TrackState, string> = {
  record: "record",
  overdub: "overdub",
  play: "play",
  stop: "stop",
  hear_through: "hear",
};

export function Blocks({ score, state, stale }: Props) {
  if (!score) {
    return (
      <section className="blocks">
        <div className="panel-title">Block-Vorschau</div>
        <div className="blocks-empty">Noch keine kompilierte Partitur — oben „Kompilieren" drücken.</div>
      </section>
    );
  }
  const countIn = isCountIn(state);
  const active = state.running && !countIn ? state.section_index : null;
  const pendingIdx = state.running && state.pending === "next" && active !== null ? active + 1 : null;
  const bpb = score.beats_per_bar || 4;

  return (
    <section className="blocks">
      <div className="panel-title">
        Block-Vorschau
        <span className="muted">
          {score.title} · {score.bpm} BPM · {score.sections.length} Sektionen
        </span>
        {stale && <span className="badge badge-warn">nicht geladen</span>}
        {countIn && <span className="badge badge-countin">Einzähler…</span>}
      </div>
      <div className="blocks-row">
        {score.sections.map((sec) => {
          const isActive = active === sec.index;
          const isPending = pendingIdx === sec.index;
          const isPendingEnd = state.pending === "next" && isActive && sec.index === score.sections.length - 1;
          return (
            <SectionCard
              key={`${sec.index}-${sec.id}`}
              sec={sec}
              bpb={bpb}
              isActive={isActive}
              isPending={isPending}
              isPendingEnd={isPendingEnd}
              state={state}
              isNext={countIn && sec.index === Math.max(0, state.section_index ?? 0)}
            />
          );
        })}
      </div>
    </section>
  );
}

interface CardProps {
  sec: ScoreSection;
  bpb: number;
  isActive: boolean;
  isPending: boolean;
  isPendingEnd: boolean;
  isNext: boolean;
  state: StateEvent;
}

function SectionCard({ sec, bpb, isActive, isPending, isPendingEnd, isNext, state }: CardProps) {
  const bar = isActive ? state.bar : 0;
  const beat = isActive ? state.beat : 0;
  const progress = isActive && sec.bars > 0 ? Math.min(1, ((bar - 1) * bpb + beat) / (sec.bars * bpb)) : 0;
  const cls = ["card", isActive && "card-active", isPending && "card-pending", isNext && "card-next"].filter(Boolean).join(" ");
  const tracks = isActive && Object.keys(state.tracks).length ? state.tracks : sec.tracks;

  return (
    <article className={cls}>
      <header className="card-head">
        <span className="card-index">{sec.index + 1}</span>
        <h3 className="card-title" title={sec.source_line ? `Zeile ${sec.source_line}` : undefined}>
          {sec.id}
        </h3>
        <span className="card-bars">{sec.bars} Takte</span>
      </header>
      <div className="card-badges">
        <span className={`badge ${sec.autorelease ? "badge-auto" : "badge-manual"}`}>{sec.autorelease ? "autorelease" : "manuell"}</span>
        <span className="badge badge-quant">quantize: {sec.quantize}</span>
      </div>
      <ul className="card-tracks">
        {Object.entries(sec.tracks).map(([name, st]) => {
          const live = (tracks as Record<string, TrackState>)[name] ?? st;
          return (
            <li key={name}>
              <span className="track-name">{name}</span>
              <span className={`state state-${live}`}>{STATE_LABEL[live] ?? live}</span>
            </li>
          );
        })}
      </ul>
      {(isPending || isPendingEnd) && <div className="pending-label">{isPendingEnd ? "Wechsel armiert → Ende" : "Wechsel armiert"}</div>}
      {isNext && <div className="pending-label next-label">startet an der Taktgrenze</div>}
      {isActive && (
        <div className="card-progress">
          <div className="progress">
            <div className="progress-fill" style={{ width: `${progress * 100}%` }} />
          </div>
          <div className="progress-text">
            Takt {bar}/{sec.bars}
          </div>
          <div className="beats">
            {Array.from({ length: bpb }, (_, i) => (
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
