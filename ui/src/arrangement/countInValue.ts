//! The count-in as a value: default, range, the live button's cycle. Pure, so it can be tested
//! without a window; the wiring to the runner and the document is `countIn.ts`.

import type { ScoreDoc } from "../types";

/** What a score without `count_in:` counts in - `DEFAULT_COUNT_IN_BARS` of the engine. */
export const DEFAULT_COUNT_IN = 2;
export const MAX_COUNT_IN = 8;

export function clampCountIn(bars: number): number {
  return Math.max(1, Math.min(MAX_COUNT_IN, Math.round(bars) || 1));
}

/** The count-in a document asks for. */
export function docCountIn(doc: ScoreDoc | null | undefined): number {
  return doc?.count_in ?? DEFAULT_COUNT_IN;
}

/** The next value of the live button: 1, 2, 3, 4, then 1 again. Anything above four (set in the
 *  file) goes back to one as well. */
export function nextCountIn(current: number): number {
  return current >= 4 ? 1 : current + 1;
}

/** A score text without its `count_in:` line - that line is kept in step with the runner directly
 *  (above), so it does not make the loaded score stale. */
export function withoutCountIn(text: string): string {
  return text.replace(/^count_in\s*:.*(\r?\n|$)/m, "");
}
