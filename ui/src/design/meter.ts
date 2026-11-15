//! Meter ballistics: the one place that decides how every level display in the app moves.
//!
//! The engine sends a peak (the loudest sample since the previous event) twenty times a second.
//! Drawing those raw values makes a bar that jumps up and down at 20 Hz - exactly the nervous
//! flicker a meter must not have. So every meter runs the same PPM-like ballistics:
//!
//! * **Rise at once.** A new peak lifts the bar in the very next frame; no transient is swallowed.
//! * **Fall slowly**, at `METER_FALL_DB_PER_S`, measured in time and not in events or frames - the
//!   bar falls at the same speed whether the screen runs at 60 or 120 Hz.
//! * **Peak hold:** a thin tick stays at the highest level for `PEAK_HOLD_MS`, then sinks at
//!   `PEAK_FALL_DB_PER_S`.
//! * **Clip:** a peak at or above 0 dBFS lights a red lamp that stays for `CLIP_HOLD_MS` after the
//!   last over (or until clicked), instead of flashing for one event.
//! * Everything runs in dB; how dB map to bar length is the view's business (`dbFraction` for the
//!   plain -60..0 dBFS scale, the fader law for the mixer).
//!
//! With `prefers-reduced-motion` the bar falls even slower and the peak tick does not glide: it
//! holds, then steps down to the bar.
//!
//! The functions below are pure (state in, time in), so they are tested without a browser. The
//! drawing loop at the bottom is one shared `requestAnimationFrame` for all meters on screen.

/** Bottom of every meter: anything quieter reads as silence. */
export const METER_FLOOR_DB = -60;
/** How fast the bar falls back, in dB per second (PPM-like; 13.3 dB/s is the IEC type I value). */
export const METER_FALL_DB_PER_S = 15;
/** The same with `prefers-reduced-motion`. */
export const METER_FALL_DB_PER_S_REDUCED = 8;
/** How long the peak tick stays put before it sinks. */
export const PEAK_HOLD_MS = 1200;
/** How fast the peak tick sinks after the hold. */
export const PEAK_FALL_DB_PER_S = 10;
/** How long the clip lamp stays lit after the last over. */
export const CLIP_HOLD_MS = 2000;
/** From here on a peak counts as an over: 0 dBFS, with a hair of tolerance for float rounding. */
export const CLIP_DB = -0.01;
/** A value older than this no longer holds the bar up - the engine stopped sending. */
export const STALE_MS = 300;
/** How often a number beside a meter may change (four times a second). */
export const READOUT_MS = 250;

export interface Ballistics {
  /** Fall rate of the bar, dB/s. */
  fall: number;
  /** Fall rate of the peak tick after the hold, dB/s; `Infinity` steps it straight to the bar. */
  peakFall: number;
  holdMs: number;
}

export const NORMAL: Ballistics = { fall: METER_FALL_DB_PER_S, peakFall: PEAK_FALL_DB_PER_S, holdMs: PEAK_HOLD_MS };
export const REDUCED: Ballistics = { fall: METER_FALL_DB_PER_S_REDUCED, peakFall: Infinity, holdMs: PEAK_HOLD_MS };

export interface MeterState {
  /** Newest value from the engine, dB (clamped to the floor), and when it came. */
  target: number;
  targetAt: number;
  /** What the bar shows, dB. */
  level: number;
  /** Where the peak tick sits, dB, and when it was last pushed up. */
  peak: number;
  peakAt: number;
  /** Time of the last over; -Infinity when the lamp is out. */
  clipAt: number;
  /** Time of the last `meterTick`. */
  at: number;
}

export function createMeter(now = 0): MeterState {
  return {
    target: METER_FLOOR_DB,
    targetAt: -Infinity,
    level: METER_FLOOR_DB,
    peak: METER_FLOOR_DB,
    peakAt: -Infinity,
    clipAt: -Infinity,
    at: now,
  };
}

/** Linear amplitude to dBFS, clamped to the meter floor. */
export function toDb(linear: number): number {
  if (!(linear > 0)) return METER_FLOOR_DB;
  return Math.max(METER_FLOOR_DB, 20 * Math.log10(linear));
}

/** dB to bar length 0..1 on the plain scale: `METER_FLOOR_DB` at the bottom, 0 dBFS at the top. */
export function dbFraction(db: number): number {
  if (!(db > METER_FLOOR_DB)) return 0;
  return Math.min(1, (db - METER_FLOOR_DB) / -METER_FLOOR_DB);
}

/** A new value from the engine (linear peak). Rises at once; the fall happens in `meterTick`. */
export function meterFeed(m: MeterState, linear: number, now: number): void {
  meterFeedDb(m, toDb(linear), now);
  if (linear > 0 && 20 * Math.log10(linear) >= CLIP_DB) m.clipAt = now;
}

/** The same for a value already in dB (at least `METER_FLOOR_DB`); no clip detection. */
export function meterFeedDb(m: MeterState, db: number, now: number): void {
  m.target = db;
  m.targetAt = now;
  if (db > m.level) m.level = db;
  if (db >= m.peak) {
    m.peak = db;
    m.peakAt = now;
  }
}

/** Advance the meter to `now`: the bar and the peak tick fall, by time. */
export function meterTick(m: MeterState, now: number, b: Ballistics = NORMAL): void {
  const prev = m.at;
  m.at = now;
  if (now <= prev) return;
  const dt = (now - prev) / 1000;
  const floor = now - m.targetAt > STALE_MS ? METER_FLOOR_DB : m.target;
  m.level = Math.max(floor, m.level - b.fall * dt);

  const holdEnd = m.peakAt + b.holdMs;
  if (now > holdEnd) {
    if (b.peakFall === Infinity) {
      m.peak = m.level;
    } else {
      const falling = (now - Math.max(prev, holdEnd)) / 1000;
      m.peak -= b.peakFall * falling;
    }
  }
  if (m.peak < m.level) m.peak = m.level;
}

/** Whether the clip lamp is lit. */
export function clipLit(m: MeterState, now: number): boolean {
  return now - m.clipAt < CLIP_HOLD_MS;
}

/** Put the clip lamp out (a click on the meter). */
export function clearClip(m: MeterState): void {
  m.clipAt = -Infinity;
}

/** A dB value as a readout beside a meter: "−12", "−∞" at the floor. */
export function dbText(db: number): string {
  if (!(db > METER_FLOOR_DB)) return "−∞";
  const r = Math.round(db);
  return r < 0 ? `−${-r}` : `${r}`;
}

// ---------------------------------------------------------------------------------------------
// One drawing loop for every meter on screen
// ---------------------------------------------------------------------------------------------

type Frame = (now: number, b: Ballistics) => void;

const frames = new Set<Frame>();
let raf = 0;
let reduced = false;

if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
  const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
  reduced = mq.matches;
  mq.addEventListener?.("change", (e) => {
    reduced = e.matches;
  });
}

function loop() {
  const now = performance.now();
  const b = reduced ? REDUCED : NORMAL;
  for (const f of frames) f(now, b);
  raf = frames.size > 0 ? requestAnimationFrame(loop) : 0;
}

/** Run `frame` once per display frame while it is registered. Returns the unregister function. */
export function onFrame(frame: Frame): () => void {
  frames.add(frame);
  if (!raf && typeof requestAnimationFrame === "function") raf = requestAnimationFrame(loop);
  return () => {
    frames.delete(frame);
  };
}
