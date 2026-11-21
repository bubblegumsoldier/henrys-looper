//! The audio load meter in the title bar: a short bar and the peak load of the last second, in the
//! warning colour from 70 %, in the alarm colour at the budget and after an overrun, and a red
//! counter once an overrun happened. A click resets the counter and opens the engine panel with the
//! details. Logic and ballistics: `loadModel.ts`.
//!
//! Like the level meters it never renders for a new value: the status effect feeds the state, one
//! shared animation frame draws it straight into the DOM, and React only renders when the engine
//! starts or stops or the counter changes.

import { useEffect, useRef, useState } from "react";
import { onFrame } from "../design/meter";
import { useStatusEffect, useStatusSlice } from "../status";
import { IDLE_LOAD, type LoadStatus } from "../types";
import {
  createLoadMeter,
  loadFeed,
  loadFraction,
  loadLevel,
  loadTick,
  loadTitle,
  percentText,
  resetOverruns,
  sinceReset,
} from "./loadModel";

export function LoadMeter({ onToggle }: { onToggle: () => void }) {
  const running = useStatusSlice((s) => s.running);
  const meter = useRef(createLoadMeter(performance.now()));
  const latest = useRef<LoadStatus>(IDLE_LOAD);
  const [since, setSince] = useState(0);
  const sinceRef = useRef(0);
  const btn = useRef<HTMLButtonElement | null>(null);
  const fill = useRef<HTMLElement | null>(null);
  const readout = useRef<HTMLElement | null>(null);
  const drawn = useRef({ scale: -1, level: "", text: "", shownAt: -1 });

  useStatusEffect((s) => {
    const load = s.load ?? IDLE_LOAD;
    latest.current = load;
    loadFeed(meter.current, load, performance.now());
    const n = sinceReset(meter.current);
    if (n !== sinceRef.current) {
      sinceRef.current = n;
      setSince(n);
    }
  });

  useEffect(() => {
    if (!running) return;
    drawn.current = { scale: -1, level: "", text: "", shownAt: -1 };
    return onFrame((now) => {
      const m = meter.current;
      loadTick(m, now);
      const d = drawn.current;
      const scale = Math.round(loadFraction(m.level) * 200) / 200;
      if (scale !== d.scale && fill.current) {
        fill.current.style.transform = `scaleX(${scale})`;
        d.scale = scale;
      }
      const level = loadLevel(m, now);
      if (level !== d.level && btn.current) {
        btn.current.dataset.level = level;
        d.level = level;
      }
      if (m.shownAt !== d.shownAt) {
        d.shownAt = m.shownAt;
        const text = percentText(m.shown);
        if (text !== d.text && readout.current) {
          readout.current.textContent = text;
          d.text = text;
        }
        // The tooltip follows the number, four times a second at most.
        if (btn.current) btn.current.title = loadTitle(latest.current, sinceReset(m));
      }
    });
  }, [running]);

  if (!running) return null;

  const click = () => {
    if (sinceReset(meter.current) > 0) {
      resetOverruns(meter.current);
      sinceRef.current = 0;
      setSince(0);
    }
    onToggle();
  };

  return (
    <button
      ref={btn}
      className="pill load-meter"
      data-level="ok"
      onClick={click}
      aria-label="Audio load"
      title={loadTitle(latest.current, since)}
    >
      <span className="load-word">Load</span>
      <span className="load-bar" aria-hidden>
        <i ref={fill} />
      </span>
      <span className="load-num" ref={readout}>
        {percentText(meter.current.shown)}
      </span>
      {since > 0 && (
        <b className="load-over" title="Overruns since the last reset - click to reset">
          {since}
        </b>
      )}
    </button>
  );
}
