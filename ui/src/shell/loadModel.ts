//! The audio load meter of the title bar - "CPU" in Ableton's corner, but measured where it
//! matters: how much of each audio block's time budget the engine used (`looper_engine::load`).
//!
//! The engine already sends the worst callback of the last second, four times a second. The
//! display adds the same calm ballistics as every level meter (`design/meter.ts`): it rises at
//! once, falls slowly by time, and the number beside it changes at most every `READOUT_MS`. So a
//! single slow block stays readable for a moment instead of flashing for one event.
//!
//! Colours: warning from `LOAD_WARN` %, alarm at 100 % (the block was late) and for
//! `ALARM_HOLD_MS` after an overrun. The overrun counter is counted against a baseline the user
//! can reset by clicking; the total since the engine started stays in the engine panel.
//!
//! Pure functions, tested without a browser.

import { READOUT_MS } from "../design/meter";
import type { LoadStatus } from "../types";

/** From here on the bar turns to the warning colour. */
export const LOAD_WARN = 70;
/** At or above the budget: the block was late. */
export const LOAD_ALARM = 100;
/** How fast the bar falls back, in percentage points per second. */
export const LOAD_FALL_PER_S = 25;
/** How long the meter stays in the alarm colour after an overrun. */
export const ALARM_HOLD_MS = 4000;
/** Full width of the bar. Above the budget the bar is simply full. */
export const LOAD_SCALE = 100;

export const ADVICE = "Try a larger buffer in Setup › Audio.";

export type LoadLevel = "ok" | "warn" | "alarm";

export interface LoadMeterState {
  /** Newest peak from the engine, percent. */
  target: number;
  /** What the bar shows, percent. */
  level: number;
  /** The number beside the bar and when it was last changed. */
  shown: number;
  shownAt: number;
  /** Overrun total at the last reset (the counter shows the total minus this). */
  baseline: number;
  /** Overrun total last seen, and when it last went up. */
  overruns: number;
  overrunAt: number;
  at: number;
}

export function createLoadMeter(now = 0): LoadMeterState {
  return { target: 0, level: 0, shown: 0, shownAt: -Infinity, baseline: 0, overruns: 0, overrunAt: -Infinity, at: now };
}

/** A new status: the peak of the last second, and the overrun total. */
export function loadFeed(m: LoadMeterState, load: LoadStatus, now: number): void {
  const peak = Number.isFinite(load.peak_percent) ? Math.max(0, load.peak_percent) : 0;
  m.target = peak;
  if (peak > m.level) m.level = peak;
  // A new engine starts counting at zero again; so does the counter on screen.
  if (load.overruns < m.overruns || load.overruns < m.baseline) m.baseline = 0;
  if (load.overruns > m.overruns) m.overrunAt = now;
  m.overruns = load.overruns;
  loadTick(m, now);
}

/** Advance to `now`: the bar falls towards the newest peak by time; the number follows every READOUT_MS. */
export function loadTick(m: LoadMeterState, now: number): void {
  const prev = m.at;
  m.at = now;
  if (now > prev) {
    m.level = Math.max(m.target, m.level - (LOAD_FALL_PER_S * (now - prev)) / 1000);
  }
  if (now - m.shownAt >= READOUT_MS) {
    m.shown = Math.round(m.level);
    m.shownAt = now;
  }
}

/** Overruns since the last reset. */
export function sinceReset(m: LoadMeterState): number {
  return Math.max(0, m.overruns - m.baseline);
}

/** The click on the counter. */
export function resetOverruns(m: LoadMeterState): void {
  m.baseline = m.overruns;
  m.overrunAt = -Infinity;
}

export function loadLevel(m: LoadMeterState, now: number): LoadLevel {
  if (m.level >= LOAD_ALARM || now - m.overrunAt < ALARM_HOLD_MS) return "alarm";
  if (m.level >= LOAD_WARN) return "warn";
  return "ok";
}

/** Bar length 0..1. */
export function loadFraction(percent: number): number {
  if (!(percent > 0)) return 0;
  return Math.min(1, percent / LOAD_SCALE);
}

/** "34 %"; above 999 "999+ %" so the pill never grows. */
export function percentText(percent: number): string {
  const r = Math.round(percent);
  return r > 999 ? "999+ %" : `${r} %`;
}

/** "2.67 ms" - the time budget of one block. */
export function budgetText(load: LoadStatus): string {
  return load.budget_ms > 0 ? `${load.budget_ms.toFixed(2)} ms` : "–";
}

/** The plain advice, or null while nothing was late. */
export function loadAdvice(load: LoadStatus): string | null {
  return load.overruns > 0 ? ADVICE : null;
}

/** The tooltip of the title bar meter. */
export function loadTitle(load: LoadStatus, since: number): string {
  const lines = [
    `Audio load: peak ${percentText(load.peak_percent)}, average ${percentText(load.avg_percent)} of the ${budgetText(load)} budget per block (last second).`,
  ];
  if (load.plugin_percent > 0) lines.push(`Plugins use ${percentText(load.plugin_percent)} of it.`);
  if (load.overruns === 0) {
    lines.push("No overruns so far.");
  } else {
    const last = load.last_overrun
      ? ` Last at ${load.last_overrun} (${load.last_overrun_kind ?? "overrun"}${load.last_overrun_percent > 0 ? `, ${percentText(load.last_overrun_percent)}` : ""}).`
      : "";
    lines.push(`${load.overruns} overrun${load.overruns === 1 ? "" : "s"} since the engine started.${last}`);
    lines.push(ADVICE);
    if (since > 0) lines.push("Click to reset the counter.");
  }
  return lines.join("\n");
}
