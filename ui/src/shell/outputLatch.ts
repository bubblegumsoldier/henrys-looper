//! The clip latch of the title bar: which device outputs went over 0 dBFS since the musician last
//! looked, how far, and which volume to turn down. Pure state in, state out, so it is tested
//! without a browser (`outputLatch.test.ts`); `OutputClip.tsx` draws it.
//!
//! It latches like the clip LED of a hardware desk - it stays red until it is clicked - because an
//! over that only flashes while nobody looks is exactly the one that goes unnoticed on a stage.
//! What it counts is the sum on the socket before the safety limiter (`outputs` in the status):
//! the limiter keeps the over from being heard as hard clipping, the latch says it happened.

import { CLIP_DB } from "../design/meter";
import type { OutputMeter } from "../types";

export interface OutputLatch {
  /** Device channels (one-based) that went over since the last reset, ascending. */
  channels: number[];
  /** Loudest over in dBFS. */
  worstDb: number;
  /** Deepest gain reduction of the safety limiter in dB, since the last reset (0 = none). */
  reductionDb: number;
  /** Which buses leave on the channels that went over, as the status names them. */
  buses: string[];
}

export const NO_CLIP: OutputLatch = { channels: [], worstDb: -Infinity, reductionDb: 0, buses: [] };

/** Fold one status event into the latch. Returns `prev` itself when nothing changed. */
export function latchOutputs(prev: OutputLatch, outputs: readonly OutputMeter[]): OutputLatch {
  let next = prev;
  for (const o of outputs) {
    const reduction = Math.min(0, o.reduction_db);
    const over = o.dbfs !== null && o.dbfs >= CLIP_DB;
    if (!over && reduction >= next.reductionDb) continue;
    if (next === prev) next = { ...prev, channels: [...prev.channels], buses: [...prev.buses] };
    if (reduction < next.reductionDb) next.reductionDb = reduction;
    if (!over) continue;
    if (!next.channels.includes(o.channel)) {
      next.channels.push(o.channel);
      next.channels.sort((a, b) => a - b);
    }
    if (o.buses && !next.buses.includes(o.buses)) next.buses.push(o.buses);
    if ((o.dbfs ?? -Infinity) > next.worstDb) next.worstDb = o.dbfs ?? next.worstDb;
  }
  return next;
}

/** Whether the latch is lit. */
export function clipped(l: OutputLatch): boolean {
  return l.channels.length > 0;
}

/** What to turn down, from the buses on the channels that went over. */
function remedy(buses: readonly string[]): string {
  const shared = buses.some((b) => b.includes("/"));
  const main = shared || buses.includes("Main");
  const monitor = buses.includes("Monitor");
  if (shared) return "lower Main or the click (Main and Monitor share this output: one mix, Main sets its level)";
  if (main && monitor) return "lower Main/Monitor";
  if (monitor) return "lower Monitor or the click";
  if (main) return "lower Main";
  return "lower Main/Monitor";
}

/** The tooltip: "Output 1/2 clipping (+1.8 dBFS) – lower Main …". */
export function outputClipTitle(l: OutputLatch): string {
  if (!clipped(l)) return "";
  const which = `Output${l.channels.length > 1 ? "s" : ""} ${l.channels.join("/")}`;
  const db = Number.isFinite(l.worstDb) ? ` (${l.worstDb >= 0 ? "+" : "−"}${Math.abs(l.worstDb).toFixed(1)} dBFS)` : "";
  const lim =
    l.reductionDb < -0.05
      ? ` The safety limiter took off up to ${Math.abs(l.reductionDb).toFixed(1)} dB, so it was not heard as hard clipping.`
      : "";
  return `${which} clipping${db} – ${remedy(l.buses)}.${lim} Click to reset.`;
}
