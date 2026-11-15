//! The one level meter of the app, horizontal (`.hm`) or vertical (`.vm`), one bar per channel.
//!
//! It never renders after mounting. The status subscription only *feeds* the ballistics
//! (`meter.ts`); a shared `requestAnimationFrame` loop moves the bars with `transform`, so a meter
//! costs no layout and no React work, and moves smoothly however often the engine reports.
//!
//! Per channel: the lit gradient as background, a cover (`.off`) scaled over the unlit part, the
//! peak tick (`.pk`), the clip lamp (`.cl`) and the LED segment gaps on top (`::after`).

import { useEffect, useRef, type CSSProperties, type RefObject } from "react";
import { useStatusEffect } from "../status";
import type { LooperStatus } from "../types";
import {
  METER_FLOOR_DB,
  READOUT_MS,
  clearClip,
  clipLit,
  createMeter,
  dbFraction,
  dbText,
  meterFeed,
  meterTick,
  onFrame,
  type MeterState,
} from "./meter";

export interface MeterProps {
  /** One linear peak (0..1, more is over) per channel, read from the status. */
  pick: (status: LooperStatus) => number[];
  /** How many bars to draw; a channel the status does not have shows the first one. */
  channels: number;
  /** Horizontal (default) or vertical. */
  vertical?: boolean;
  /** dB to bar length 0..1. Default: -60..0 dBFS, linear in dB. */
  scale?: (db: number) => number;
  /** Wider bars (vertical only), as on a mono mixer strip. */
  wide?: boolean;
  /** An element that gets the held peak as a number ("−12"), at most four times a second. */
  readout?: RefObject<HTMLElement | null>;
  className?: string;
  style?: CSSProperties;
}

interface Drawn {
  l: string;
  p: string;
  clip: boolean;
  nopeak: boolean;
}

export function Meter({ pick, channels, vertical, scale = dbFraction, wide, readout, className, style }: MeterProps) {
  const host = useRef<HTMLElement | null>(null);
  const meters = useRef<MeterState[]>([]);
  const drawn = useRef<Drawn[]>([]);
  const pickRef = useRef(pick);
  pickRef.current = pick;
  const scaleRef = useRef(scale);
  scaleRef.current = scale;
  const readoutRef = useRef(readout);
  readoutRef.current = readout;

  useStatusEffect((status) => {
    const now = performance.now();
    const peaks = pickRef.current(status);
    for (let c = 0; c < channels; c += 1) {
      const m = (meters.current[c] ??= createMeter(now));
      meterFeed(m, peaks[c] ?? peaks[0] ?? 0, now);
    }
  });

  useEffect(() => {
    let lastText = "";
    let textAt = -Infinity;
    return onFrame((now, b) => {
      const el = host.current;
      if (!el) return;
      const rows = el.children;
      let held = METER_FLOOR_DB;
      for (let c = 0; c < rows.length; c += 1) {
        const m = meters.current[c];
        if (!m) continue;
        meterTick(m, now, b);
        held = Math.max(held, m.peak);
        const sc = scaleRef.current;
        const lf = sc(m.level);
        const pf = sc(m.peak);
        const d = (drawn.current[c] ??= { l: "", p: "", clip: false, nopeak: true });
        const row = rows[c] as HTMLElement;
        // Three decimals of a scale factor are a fraction of a pixel on any meter we draw.
        const l = (1 - lf).toFixed(3);
        if (l !== d.l) {
          (row.children[0] as HTMLElement).style.transform = vertical ? `scaleY(${l})` : `scaleX(${l})`;
          d.l = l;
        }
        const p = (pf * 100).toFixed(2);
        if (p !== d.p) {
          (row.children[1] as HTMLElement).style.transform = vertical ? `translateY(-${p}%)` : `translateX(${p}%)`;
          d.p = p;
        }
        const nopeak = pf <= 0.001;
        if (nopeak !== d.nopeak) {
          row.classList.toggle("nopeak", nopeak);
          d.nopeak = nopeak;
        }
        const clip = clipLit(m, now);
        if (clip !== d.clip) {
          row.classList.toggle("clip", clip);
          d.clip = clip;
        }
      }
      const out = readoutRef.current?.current;
      if (out && now - textAt >= READOUT_MS) {
        textAt = now;
        const t = dbText(held);
        if (t !== lastText) {
          out.textContent = t;
          lastText = t;
        }
      }
    });
  }, [vertical]);

  const onClick = () => {
    for (const m of meters.current) if (m) clearClip(m);
  };

  const cls = `${vertical ? "vm" : "hm"} meter${vertical && wide ? " w" : ""}${className ? ` ${className}` : ""}`;
  const bars = Array.from({ length: channels }, (_, i) => (
    <i key={i} className="nopeak">
      <b className="off" />
      <s className="pk" />
      <u className="cl" />
    </i>
  ));
  return vertical ? (
    <div className={cls} ref={host as RefObject<HTMLDivElement>} style={style} onClick={onClick}>
      {bars}
    </div>
  ) : (
    <span className={cls} ref={host as RefObject<HTMLSpanElement>} style={style} onClick={onClick}>
      {bars}
    </span>
  );
}
