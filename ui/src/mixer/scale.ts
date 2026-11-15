//! The fader law: where a level sits on a channel fader, in both directions.
//!
//! One curve serves the fader cap, the dB scale beside it and the level meter next to it, so a
//! signal at -12 dBFS lights the meter exactly where the scale says "12" and where a fader set to
//! -12 dB stands. The breakpoints are the ones of the mockup (design/arrangement-mixer.html):
//! 0 dB at 82 % of the travel, then 6, 12, 24 and 48 dB down at 66, 50, 30 and 10 %, -inf at the
//! bottom. Above 0 dB the travel reaches +12 dB, the engine's ceiling (`MAX_TRACK_GAIN`).

/** +12 dB, linear - the channel fader's ceiling in the engine. */
export const MAX_TRACK_GAIN = Math.pow(10, 12 / 20);

/** The bus volume's ceiling in the engine (`MAX_BUS_GAIN`), linear; about +12 dB as well. */
export const MAX_BUS_GAIN = 4;

/** (position 0..1, dB) pairs, bottom to top; below the first one the curve runs linearly to zero. */
const POINTS: [number, number][] = [
  [0.1, -48],
  [0.3, -24],
  [0.5, -12],
  [0.66, -6],
  [0.82, 0],
  [0.91, 6],
  [1, 12],
];

export const dbToGain = (db: number) => Math.pow(10, db / 20);
export const gainToDb = (gain: number) => (gain <= 0 ? -Infinity : 20 * Math.log10(gain));

/** Fader position 0..1 for a decibel value. */
export function dbToPos(db: number): number {
  if (!Number.isFinite(db) || db <= -200) return 0;
  const [p0, d0] = POINTS[0];
  if (db <= d0) return p0 * dbToGain(db - d0); // linear in gain below -48 dB
  for (let i = 1; i < POINTS.length; i += 1) {
    const [pa, da] = POINTS[i - 1];
    const [pb, db1] = POINTS[i];
    if (db <= db1) return pa + ((db - da) / (db1 - da)) * (pb - pa);
  }
  return 1;
}

/** Decibels for a fader position 0..1; -Infinity at the bottom. */
export function posToDb(pos: number): number {
  const p = Math.min(1, Math.max(0, pos));
  if (p <= 0) return -Infinity;
  const [p0, d0] = POINTS[0];
  if (p <= p0) return d0 + gainToDb(p / p0);
  for (let i = 1; i < POINTS.length; i += 1) {
    const [pa, da] = POINTS[i - 1];
    const [pb, db1] = POINTS[i];
    if (p <= pb) return da + ((p - pa) / (pb - pa)) * (db1 - da);
  }
  return POINTS[POINTS.length - 1][1];
}

export const gainToPos = (gain: number) => dbToPos(gainToDb(gain));
export const posToGain = (pos: number) => {
  const db = posToDb(pos);
  return Number.isFinite(db) ? dbToGain(db) : 0;
};

/** The labels beside the fader: the number is how far *below* 0 dB, as on a console. */
export const SCALE_MARKS: { label: string; pos: number }[] = [
  { label: "+12", pos: dbToPos(12) },
  { label: "+6", pos: dbToPos(6) },
  { label: "0", pos: dbToPos(0) },
  { label: "6", pos: dbToPos(-6) },
  { label: "12", pos: dbToPos(-12) },
  { label: "24", pos: dbToPos(-24) },
  { label: "48", pos: dbToPos(-48) },
  { label: "∞", pos: 0 },
];

/** A fader value as the strip foot prints it: "0.0", "−3.5", "+2.0", "−∞". */
export function faderText(gain: number): string {
  const db = gainToDb(gain);
  if (!Number.isFinite(db) || db < -99) return "−∞";
  const v = Math.abs(db) < 0.05 ? 0 : db;
  return `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)}`;
}

/** A peak as the strip foot prints it: "−9.2 dB", "−∞ dB". */
export function peakText(peak: number): string {
  const db = gainToDb(peak);
  if (!Number.isFinite(db) || db < -99) return "−∞ dB";
  const v = Math.abs(db) < 0.05 ? 0 : db;
  return `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)} dB`;
}

/** A pan as the strip prints it: "C", "L 20", "R 100". */
export function panText(pan: number): string {
  const percent = Math.round(Math.abs(pan) * 100);
  if (percent === 0) return "C";
  return `${pan < 0 ? "L" : "R"} ${percent}`;
}
