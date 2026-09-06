//! Bar and beat, readable from across a stage, plus the pulse.
//!
//! Bar and beat change at most a few times a second, so they are a normal slice and render. The
//! pulse inside the beat moves with every event and is written into the DOM by hand.

import { useRef } from "react";
import { useStatusEffect, useStatusSlice } from "../status";
import type { LooperStatus } from "../types";

interface Beat {
  bar: number;
  beat: number;
  beatsPerBar: number;
  bars: number;
}

function pick(s: LooperStatus): Beat {
  return { bar: s.bar, beat: s.beat, beatsPerBar: s.beats_per_bar, bars: s.bars };
}

function same(a: Beat, b: Beat): boolean {
  return a.bar === b.bar && a.beat === b.beat && a.beatsPerBar === b.beatsPerBar && a.bars === b.bars;
}

export function Position() {
  const { bar, beat, beatsPerBar, bars } = useStatusSlice(pick, same);
  const pulse = useRef<HTMLDivElement | null>(null);
  const drawn = useRef(-1);

  // Position inside the current beat, 0..100. The one thing that has to move smoothly.
  useStatusEffect((s) => {
    const share = s.samples_per_beat > 0 ? s.beat_offset / s.samples_per_beat : 0;
    const width = Math.round(Math.min(1, Math.max(0, share)) * 100);
    if (width !== drawn.current && pulse.current) {
      pulse.current.style.width = `${width}%`;
      drawn.current = width;
    }
  });

  // Bar inside the loop, which is what a looper is actually counting.
  const barInLoop = bars > 0 ? ((bar - 1) % bars) + 1 : bar;
  const dots = beatsPerBar > 0 ? beatsPerBar : 4;

  return (
    <section className="live-position">
      <div className="live-bar">
        <span className="live-bar-label">Takt</span>
        <span className="live-bar-value">
          {barInLoop}
          {bars > 0 && <span className="live-bar-total">/{bars}</span>}
        </span>
      </div>

      {/* key forces the flash animation to restart on every beat */}
      <div className="live-beat" key={`${bar}-${beat}`}>
        {beat}
      </div>

      <div className="live-pulse-col">
        <div className="live-dots">
          {Array.from({ length: dots }, (_, i) => (
            <span key={i} className={`live-dot${i + 1 === beat ? " live-dot-on" : ""}${i === 0 ? " live-dot-one" : ""}`} />
          ))}
        </div>
        <div className="live-pulse">
          <div className="live-pulse-fill" ref={pulse} style={{ width: 0 }} />
        </div>
        <div className="live-abs mono">Takt {bar} absolut</div>
      </div>
    </section>
  );
}
