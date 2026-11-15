//! The mixer's level meters. Like every meter in this app they never render: the status
//! subscription writes `--l` (level) and `--p` (held peak) straight into the DOM, twenty times a
//! second, and React is not involved.
//!
//! The vertical meter uses the fader law (`scale.ts`), so the meter and the dB scale beside it
//! agree: a signal at -12 dBFS lights the meter up to the "12" mark.

import { useRef } from "react";
import { useStatusEffect } from "../status";
import type { LooperStatus } from "../types";
import { gainToPos, peakText } from "./scale";

/** How long a held peak stays before it falls, in status events (20 per second), and how fast. */
const HOLD_EVENTS = 30;
const FALL = 0.02;

interface Props {
  /** One linear peak per channel. */
  pick: (status: LooperStatus) => number[];
  channels: number;
  /** Wider bars, as on a mono strip in the mockup. */
  wide?: boolean;
}

export function VMeter({ pick, channels, wide }: Props) {
  const host = useRef<HTMLDivElement | null>(null);
  const held = useRef<{ peak: number; age: number; l: string; p: string }[]>([]);

  useStatusEffect((status) => {
    const el = host.current;
    if (!el) return;
    const peaks = pick(status);
    for (let c = 0; c < el.children.length; c += 1) {
      const level = gainToPos(peaks[c] ?? peaks[0] ?? 0);
      const h = (held.current[c] ??= { peak: 0, age: 0, l: "", p: "" });
      if (level >= h.peak) {
        h.peak = level;
        h.age = 0;
      } else if (++h.age > HOLD_EVENTS) {
        h.peak = Math.max(level, h.peak - FALL);
      }
      const row = el.children[c] as HTMLElement;
      const l = `${(level * 100).toFixed(1)}%`;
      const p = `${(h.peak * 100).toFixed(1)}%`;
      if (l !== h.l) {
        row.style.setProperty("--l", l);
        h.l = l;
      }
      if (p !== h.p) {
        row.style.setProperty("--p", p);
        row.classList.toggle("nopeak", h.peak <= 0.001);
        h.p = p;
      }
    }
  });

  return (
    <div className={`vm${wide ? " w" : ""}`} ref={host}>
      {Array.from({ length: channels }, (_, i) => (
        <i key={i} className="nopeak" />
      ))}
    </div>
  );
}

/**
 * The highest peak since the last reset, as a number - the "−9.2 dB" in a strip foot. Clicking it
 * starts over, as on every console. Turns red above 0 dBFS.
 */
export function PeakHold({ pick }: { pick: (status: LooperStatus) => number }) {
  const el = useRef<HTMLSpanElement | null>(null);
  const max = useRef(0);
  const shown = useRef("");

  useStatusEffect((status) => {
    const v = pick(status);
    if (v > max.current) max.current = v;
    const text = peakText(max.current);
    if (text !== shown.current && el.current) {
      el.current.textContent = text;
      el.current.classList.toggle("over", max.current >= 1);
      shown.current = text;
    }
  });

  return (
    <span
      className="pk"
      ref={el}
      role="button"
      title="Highest peak since the last reset. Click to reset."
      onClick={() => {
        max.current = 0;
      }}
    >
      −∞ dB
    </span>
  );
}
