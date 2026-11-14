//! Where we are, readable from across the stage: the bar as a hero number, the section (or the
//! free loop), the beat as big circles, the bars of the section as a progress row.
//!
//! The bar progress inside the "now" segment moves with every event; it is the `--bp` custom
//! property the Live root writes without a render (see `LiveMode`).

import type { CSSProperties } from "react";
import { Icon } from "../shell/Icon";
import { GLOBAL } from "../midi/addresses";
import type { ScoreView } from "../shell/scoreTransport";
import { PHASE, T, bars, num } from "../shell/terms";
import type { CompiledScore } from "../types";

export interface Clock {
  bar: number;
  beat: number;
  bpb: number;
  unit: number;
  bars: number;
  bpm: number;
  click: boolean;
  loopSeconds: number;
  quantize: "bar" | "loop";
}

interface Props {
  clock: Clock;
  score: ScoreView;
  compiled: CompiledScore | null;
}

export function PositionPanel({ clock, score, compiled }: Props) {
  const withScore = score.loaded && score.section !== null;
  const barInLoop = clock.bars > 0 ? ((clock.bar - 1) % clock.bars) + 1 : clock.bar;
  const bar = withScore ? score.bar : barInLoop;
  const total = withScore ? score.barsTotal : clock.bars;
  const beat = withScore ? score.beat || clock.beat : clock.beat;
  const section = withScore && compiled ? compiled.sections[score.section!] : null;
  const loopsUntilRelease = section ? !section.autorelease : false;
  const armed = score.armed !== null;

  const segs = Math.max(1, Math.min(total || 1, 32));
  const facts = (
    <>
      <span data-midi={GLOBAL.tempo} title="MIDI Learn: map a knob to the tempo">
        {num(clock.bpm)} BPM
      </span>{" "}
      · {clock.bpb}/{clock.unit} · {clock.click ? T.clickOn : T.clickOff}
    </>
  );

  let where = "";
  if (withScore && compiled) {
    const before = compiled.sections.slice(0, score.section!).reduce((n, s) => n + s.bars, 0);
    const all = compiled.sections.reduce((n, s) => n + s.bars, 0);
    where = `${T.bar} ${before + score.bar} of ${all} in the arrangement`;
  } else if (withScore) {
    where = `${T.bar} ${clock.bar} since start`;
  } else {
    where = `${T.loop} ${bars(clock.bars)} · ${num(clock.loopSeconds, 2)} s`;
  }

  return (
    <section className="panel pos">
      <div className="kick">
        {score.loaded ? (
          <>
            <span className="k">
              {T.section}{" "}
              {score.section !== null ? (
                <>
                  <b>{score.section + 1}</b> / {score.sectionCount}
                </>
              ) : (
                <>– / {score.sectionCount}</>
              )}
            </span>
            {score.phase !== "none" && (
              <span className={`lv-chip ${score.phase === "running" || score.phase === "count_in" ? "run" : "idle"}`}>
                <Icon name={score.phase === "running" ? "play" : score.phase === "count_in" ? "metro" : "stop"} />
                {PHASE[score.phase]}
                {score.phase === "running" && score.pass > 0 ? ` · ${T.pass} ${score.pass}` : ""}
              </span>
            )}
            {loopsUntilRelease && score.phase === "running" && (
              <span className="lv-chip loop hide-xs">
                <Icon name="loop" />
                {T.loopsUntilRelease}
              </span>
            )}
          </>
        ) : (
          <>
            <span className="k">{T.loop}</span>
            <span className="lv-chip run">
              <Icon name="play" />
              Free looping
            </span>
            <span className="lv-chip idle hide-xs">
              <Icon name={clock.quantize === "loop" ? "loop" : "bar"} />
              {clock.quantize === "loop" ? T.quantizeLoop : T.quantizeBar}
            </span>
          </>
        )}
      </div>

      <div className="big">
        <div className="num-block">
          <label>{T.bar}</label>
          <div className="hero">
            {bar || "–"}
            {total > 0 && <small>/ {total}</small>}
          </div>
        </div>
        <div className="right">
          <h1 title={withScore ? score.sectionId : undefined}>
            {withScore ? score.sectionId : score.loaded ? score.title : "free loop"}
          </h1>
          <div className="num-block">
            <label>{T.beat}</label>
            <div className="bigbeats">
              {Array.from({ length: Math.min(clock.bpb || 4, 12) }, (_, i) => (
                <i key={i} className={`${i === 0 ? "one" : ""}${i + 1 === beat ? " on" : ""}`}>
                  {i + 1}
                </i>
              ))}
            </div>
          </div>
        </div>
      </div>

      <div>
        <div className="secbar" style={{ gridTemplateColumns: `repeat(${segs}, 1fr)` } as CSSProperties}>
          {Array.from({ length: segs }, (_, i) => {
            const n = i + 1;
            const cls = n < bar ? "done" : n === bar ? "now" : armed && withScore ? "tail" : "";
            return <i key={i} className={cls} />;
          })}
        </div>
        <div className="secbar-l">
          <span>{facts}</span>
          <span>{where}</span>
        </div>
      </div>
    </section>
  );
}
