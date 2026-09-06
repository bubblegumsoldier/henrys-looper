//! Level meters. These are the one part of the UI that really does move twenty times a second, so
//! they never render: the subscription writes a width and a number straight into the DOM.

import { useRef } from "react";
import { useStatusEffect } from "../status";
import type { LooperStatus } from "../types";

/** Bottom of the scale. Below this a meter reads empty. */
const FLOOR_DB = -60;

/** Linear peak to a percentage on a decibel scale, which is the only scale an ear agrees with. */
function widthOf(peak: number): number {
  if (peak <= 0) return 0;
  const db = 20 * Math.log10(peak);
  if (db <= FLOOR_DB) return 0;
  return Math.min(100, ((db - FLOOR_DB) / -FLOOR_DB) * 100);
}

/** The same number the Rust side reports as `*_dbfs`; silence has no decibel value. */
function textOf(peak: number): string {
  if (peak <= 0) return "–∞";
  const db = 20 * Math.log10(peak);
  return db >= 0 ? "0.0" : db.toFixed(1);
}

interface Props {
  label: string;
  kind: "in" | "out";
  /** The linear peak to draw, 0.0 to 1.0. Zero while the track does not exist. */
  pick: (status: LooperStatus) => number;
}

export function Meter({ label, kind, pick }: Props) {
  const mask = useRef<HTMLDivElement | null>(null);
  const value = useRef<HTMLSpanElement | null>(null);
  const bar = useRef<HTMLDivElement | null>(null);
  // Last values written, so an unchanged meter costs nothing but a comparison.
  const drawn = useRef({ width: -1, text: "", clipped: false });

  useStatusEffect((status) => {
    const peak = pick(status);

    // The colour gradient belongs to the whole bar - green up to about -18 dBFS, red at the top -
    // so the level is drawn by *uncovering* it from the left rather than by a coloured fill, which
    // would squeeze the whole gradient into whatever the current level happens to be.
    const width = Math.round(widthOf(peak));
    if (width !== drawn.current.width && mask.current) {
      mask.current.style.width = `${100 - width}%`;
      drawn.current.width = width;
    }
    const text = textOf(peak);
    if (text !== drawn.current.text && value.current) {
      value.current.textContent = text;
      drawn.current.text = text;
    }
    const clipped = peak >= 0.999;
    if (clipped !== drawn.current.clipped && bar.current) {
      bar.current.classList.toggle("meter-clip", clipped);
      drawn.current.clipped = clipped;
    }
  });

  return (
    <div className={`meter meter-${kind}`}>
      <span className="meter-label">{label}</span>
      <div className="meter-bar" ref={bar}>
        <div className="meter-mask" ref={mask} style={{ width: "100%" }} />
      </div>
      <span className="meter-value mono" ref={value}>
        –∞
      </span>
    </div>
  );
}
