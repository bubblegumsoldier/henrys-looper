//! The segmented horizontal meter of the design (`.hm` in app.css): one LED chain per channel, a
//! peak-hold tick on each. Like `Meter`, it never renders - the status subscription writes `--l`
//! (level) and `--p` (held peak) straight into the DOM.

import { useRef, type CSSProperties } from "react";
import { useStatusEffect } from "../status";
import type { LooperStatus } from "../types";

const FLOOR_DB = -60;
/** How long a peak stays put before it starts to fall, and how fast it falls (percent per event). */
const HOLD_EVENTS = 24;
const FALL = 2.5;

export function levelPercent(peak: number): number {
  if (peak <= 0) return 0;
  const db = 20 * Math.log10(peak);
  if (db <= FLOOR_DB) return 0;
  return Math.min(100, ((db - FLOOR_DB) / -FLOOR_DB) * 100);
}

interface Props {
  /** One linear peak (0..1) per channel, read from the status. */
  pick: (status: LooperStatus) => number[];
  /** How many channels to draw. */
  channels: number;
  className?: string;
  style?: CSSProperties;
}

export function HMeter({ pick, channels, className, style }: Props) {
  const host = useRef<HTMLSpanElement | null>(null);
  const held = useRef<{ peak: number; age: number; l: number; p: number }[]>([]);

  useStatusEffect((status) => {
    const el = host.current;
    if (!el) return;
    const peaks = pick(status);
    const rows = el.children;
    for (let c = 0; c < rows.length; c += 1) {
      const level = Math.round(levelPercent(peaks[c] ?? peaks[0] ?? 0));
      const h = (held.current[c] ??= { peak: 0, age: 0, l: -1, p: -1 });
      if (level >= h.peak) {
        h.peak = level;
        h.age = 0;
      } else if (++h.age > HOLD_EVENTS) {
        h.peak = Math.max(level, h.peak - FALL);
      }
      const p = Math.round(h.peak);
      const row = rows[c] as HTMLElement;
      if (level !== h.l) {
        row.style.setProperty("--l", `${level}%`);
        h.l = level;
      }
      if (p !== h.p) {
        row.style.setProperty("--p", `${p}%`);
        row.classList.toggle("nopeak", p === 0);
        h.p = p;
      }
    }
  });

  return (
    <span className={`hm${className ? ` ${className}` : ""}`} ref={host} style={style}>
      {Array.from({ length: channels }, (_, i) => (
        <i key={i} className="nopeak" />
      ))}
    </span>
  );
}
