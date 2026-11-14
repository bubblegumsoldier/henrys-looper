//! The arrangement, reduced to what a stage needs: which sections are done, which one sounds (with
//! its progress), which one comes next. Each block jumps to its section on click (`transport.goto.n`)
//! - the runner refuses while a switch is armed and says so.

import type { CSSProperties } from "react";
import { scoreApi } from "../api";
import { gotoAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { say } from "../shell/notes";
import type { ScoreView } from "../shell/scoreTransport";
import { T } from "../shell/terms";
import type { CompiledScore } from "../types";

interface Props {
  score: ScoreView;
  compiled: CompiledScore | null;
  onOpenArrangement: () => void;
}

export function ArrangementStrip({ score, compiled, onOpenArrangement }: Props) {
  if (!score.loaded) {
    return (
      <section className="panel lane-live">
        <div className="blk empty-lane" style={{ "--n": 1 } as CSSProperties}>
          <div className="bn">
            <b>No score loaded</b>
          </div>
          <div className="bm">
            Free looping. Write or load a score in{" "}
            <button className="linkish" onClick={onOpenArrangement}>
              <Icon name="grid" /> {T.arrangement}
            </button>
          </div>
        </div>
      </section>
    );
  }

  if (!compiled) {
    return (
      <section className="panel lane-live">
        <div className="blk now" style={{ "--n": 1 } as CSSProperties}>
          <div className="bn">
            <i>{score.section !== null ? score.section + 1 : "–"}</i>
            <b>{score.sectionId || score.title}</b>
          </div>
          <div className="bm">
            {score.sectionCount} sections · open the score in {T.arrangement} to see them all
          </div>
        </div>
      </section>
    );
  }

  const current = score.section;
  const armedTo = score.armed && !score.armed.countIn ? score.armed.section : score.armed?.countIn ? 0 : null;

  return (
    <section className="panel lane-live">
      {compiled.sections.map((s, i) => {
        const past = current !== null && i < current;
        const now = i === current;
        const next = armedTo !== null && i === armedTo && !now;
        const cls = `blk${past ? " past" : ""}${now ? " now" : ""}${next ? " next" : ""}`;
        const progress = now && score.barsTotal > 0 ? (score.bar / score.barsTotal) * 100 : 0;
        return (
          <button
            key={s.id + i}
            className={cls}
            style={{ "--n": Math.max(s.bars, 2.5), "--p": `${progress}%` } as CSSProperties}
            data-midi={gotoAddress(i)}
            onDoubleClick={() => say(() => scoreApi.goto(i))}
            title={`Double-click to jump to ${s.id}`}
          >
            <div className="bn">
              <i>{i + 1}</i>
              <b>{s.id}</b>
            </div>
            <div className="bm">
              {past ? (
                <>
                  <Icon name="check" />
                  {s.bars} {T.barShort}
                </>
              ) : (
                <>
                  {s.bars} {T.barShort} ·
                  {now && score.pass > 0 && (
                    <span className="hide-s">
                      {" "}
                      {T.pass} {score.pass} ·{" "}
                    </span>
                  )}
                  <span className="rel">
                    {s.autorelease ? (
                      <span className="auto">
                        <Icon name="auto" />
                        <span className="hide-s">auto</span>
                      </span>
                    ) : now ? (
                      <>
                        <Icon name="loop" />
                        until {T.release}
                      </>
                    ) : (
                      <>
                        <Icon name="hand" />
                        {s.bars >= 3 && T.release}
                      </>
                    )}
                  </span>
                </>
              )}
            </div>
            {now && <span className="tag">{T.now}</span>}
            {next && <span className="tag">{T.upNext}</span>}
            {now && <span className="prog" />}
          </button>
        );
      })}
    </section>
  );
}
