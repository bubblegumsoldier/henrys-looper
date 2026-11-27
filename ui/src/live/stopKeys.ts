//! The two stop keys of the Live stage, as a pure function so they can be tested without a DOM.

/** Which stop a key press asks for on the selected track. */
export type StopKey = "bar_end" | "now";

/**
 * `S` stops the selected track at the end of the bar (pressed again while that stop waits, it is
 * taken back - the engine side decides that), `Shift+S` stops it now, mid-bar. While a score runs,
 * `Shift+S` is the score's "Stop all" (`useScoreKeys`) and not a track's, so it answers null then;
 * plain `S` still goes to the host, which refuses it with the runner's sentence like any transport
 * key of a track. Anything else is not a stop key.
 */
export function stopKey(key: string, shift: boolean, scoreRunning: boolean): StopKey | null {
  if (key.toLowerCase() !== "s") return null;
  if (!shift) return "bar_end";
  return scoreRunning ? null : "now";
}
