//! The words and icons of the arrangement editor.
//!
//! Every state has a fixed colour (`--st-*`), a fixed icon and a fixed word, and colour alone never
//! carries the meaning (design/tokens.css). The words are the usual DAW terms. When a central
//! glossary lands in `ui/src`, this file should import from it instead of defining its own.

import type { ScoreState } from "../types";

export interface StateLook {
  /** CSS modifier of `.clip`, `.st`, the picker. */
  cls: "rec" | "dub" | "play" | "stop" | "hear";
  /** Icon class from icons.css. */
  icon: string;
  word: string;
  /** Keyboard shortcut in the grid. */
  key: string;
}

export const STATE_LOOK: Record<ScoreState, StateLook> = {
  record: { cls: "rec", icon: "ic-rec", word: "Record", key: "R" },
  overdub: { cls: "dub", icon: "ic-dub", word: "Overdub", key: "O" },
  play: { cls: "play", icon: "ic-play", word: "Play", key: "P" },
  stop: { cls: "stop", icon: "ic-stop", word: "Stop", key: "S" },
  hear_through: { cls: "hear", icon: "ic-hear", word: "Monitor", key: "M" },
};

/** Picker order, as in the mockup: record, overdub, play, stop, monitor. */
export const STATES: ScoreState[] = ["record", "overdub", "play", "stop", "hear_through"];

export const STATE_BY_KEY: Record<string, ScoreState> = Object.fromEntries(
  STATES.map((s) => [STATE_LOOK[s].key.toLowerCase(), s]),
) as Record<string, ScoreState>;

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** `10.2 s`, `1:04 min`. */
export function duration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "–";
  if (seconds < 60) return `${seconds.toFixed(1)} s`;
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds - m * 60);
  return `${m}:${String(s).padStart(2, "0")} min`;
}

/** Bus words of the score format, as two switches. */
export function busHas(value: string | undefined, fallback: string, bus: "main" | "monitor"): boolean {
  const v = value ?? fallback;
  return v === "main+monitor" || v === bus;
}

export function busWord(main: boolean, monitor: boolean): string {
  return main && monitor ? "main+monitor" : main ? "main" : monitor ? "monitor" : "none";
}
