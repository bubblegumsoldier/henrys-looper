import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Tap tempo: the user taps a beat, we estimate BPM from the gaps.
 *
 * Two decisions worth knowing about:
 *
 * - We take the MEDIAN gap, not the mean. A single late tap (a stumble, a hiccup in the
 *   render loop) shifts a mean noticeably but barely moves a median. Musicians do stumble.
 * - Only the last `MAX_TAPS` gaps count, so the estimate follows along when someone speeds
 *   up mid-series instead of averaging over a tempo they have already left behind.
 */

/** Gap longer than this starts a fresh series rather than counting as a very slow beat. */
const RESET_MS = 2500;
/** Sliding window of taps kept for the estimate. */
const MAX_TAPS = 9;
/** Outside this range we assume a mis-tap rather than a real tempo. */
const MIN_BPM = 20;
const MAX_BPM = 300;

function estimate(taps: number[]): number | null {
  if (taps.length < 2) return null;
  const gaps: number[] = [];
  for (let i = 1; i < taps.length; i += 1) gaps.push(taps[i] - taps[i - 1]);
  gaps.sort((a, b) => a - b);
  const mid = Math.floor(gaps.length / 2);
  const median = gaps.length % 2 === 1 ? gaps[mid] : (gaps[mid - 1] + gaps[mid]) / 2;
  if (median <= 0) return null;
  const bpm = 60000 / median;
  if (bpm < MIN_BPM || bpm > MAX_BPM) return null;
  return Math.round(bpm * 10) / 10;
}

export function TapTempo({ onTempo }: { onTempo: (bpm: number) => void }) {
  const taps = useRef<number[]>([]);
  const [count, setCount] = useState(0);
  const [bpm, setBpm] = useState<number | null>(null);
  const [stale, setStale] = useState(false);
  const timer = useRef<number | null>(null);

  const clearTimer = () => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  };

  useEffect(() => clearTimer, []);

  const tap = useCallback(() => {
    const now = performance.now();
    const prev = taps.current;
    const expired = prev.length > 0 && now - prev[prev.length - 1] > RESET_MS;
    const next = [...(expired ? [] : prev), now].slice(-MAX_TAPS);
    taps.current = next;

    const est = estimate(next);
    setCount(next.length);
    setStale(false);
    if (est !== null) {
      setBpm(est);
      onTempo(est);
    } else if (expired || next.length < 2) {
      setBpm(null);
    }

    // Mark the series as finished once the user stops, so the next tap visibly starts over.
    clearTimer();
    timer.current = window.setTimeout(() => setStale(true), RESET_MS);
  }, [onTempo]);

  const reset = () => {
    taps.current = [];
    clearTimer();
    setCount(0);
    setBpm(null);
    setStale(false);
  };

  const hint = (() => {
    if (count === 0) return "Vier Mal im Takt klicken, dann steht das Tempo.";
    if (count === 1) return "Weiter klicken …";
    if (stale) return `${count} Klicks · Pause erkannt, der nächste Klick beginnt neu.`;
    return `${count} Klicks${bpm !== null ? ` · ${bpm.toFixed(1)} BPM übernommen` : ""}`;
  })();

  return (
    <div className="tap">
      <button
        type="button"
        className={`btn tap-btn${count > 0 && !stale ? " tap-active" : ""}`}
        onClick={tap}
      >
        Tempo tippen
        <span className="tap-value">{bpm !== null ? bpm.toFixed(1) : "–"}</span>
      </button>
      <button type="button" className="btn btn-mini" onClick={reset} disabled={count === 0}>
        Zurücksetzen
      </button>
      <div className="field-hint tap-hint">{hint}</div>
    </div>
  );
}
