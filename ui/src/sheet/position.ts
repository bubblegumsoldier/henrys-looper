//! Where the sheet stands: the runner's position, mapped onto the sheet's timeline.
//!
//! The engine parses the sheet and lays it over the sections (`score/sheet.rs`): every section has
//! a span `[start, end)` of the sheet's bars. Here only arithmetic is left:
//!
//! ```text
//!   status (20/s): section, pass, bar, beat, offset in the beat, armed change ─┐
//!   + time since that status (extrapolated at the tempo, a frame at a time)    ├─► sheetAt ─► bar on the sheet
//!   span of the section, its bars, autorelease ────────────────────────────────┘          ─► sheetState: current
//!                                                                                            line, chord, NOW, NEXT
//! ```
//!
//! * **Count-in:** the sheet stands at the start of the first section's span, `wait` counts down.
//! * **A section that moves on by itself** runs through its span once (and holds the last line if
//!   the text is shorter than the section).
//! * **A section that loops until Release** wraps around its span with each pass - or, when its text
//!   is longer than one pass (a verse sung over a two-bar loop), every `span` bars.
//! * **The change** (Release, autorelease, the end of the count-in) is predicted from the armed
//!   change's distance, so the jump lands on the bar line, not a status tick late.
//! * **Ready, stopped, finished:** `null` - the sheet rests at the top.
//!
//! Pure functions, no DOM: `position.test.ts` proves them.

import type { CompiledSheet, CompiledSection, LooperStatus, Phase, SheetItem } from "../types";

/** One status, reduced to what the sheet needs. */
export interface RunPoint {
  phase: Phase | "none";
  /** Zero-based sounding section, or null. */
  section: number | null;
  /** One-based bar inside the current pass. */
  bar: number;
  /** One-based pass. */
  pass: number;
  /** One-based beat in the bar. */
  beat: number;
  /** How far into the beat, 0..1. */
  inBeat: number;
  beatsPerBar: number;
  bpm: number;
  /** The armed change: where it leads (null = the end) and how many beats it still is away. */
  armed: { section: number | null; beatsLeft: number; countIn: boolean } | null;
}

/** Where the sheet stands. */
export interface SheetAt {
  /** On the sheet's timeline, in bars. During the count-in: the first section's start. */
  bar: number;
  /** Bars still to wait before `bar` is reached (the count-in); 0 while the song plays. */
  wait: number;
  /** The section whose text this is, null during the count-in. */
  section: number | null;
  /** End of that section's span; text from here on belongs to the next section. */
  spanEnd: number;
  /** Start of the next section's span, null after the last section. */
  nextStart: number | null;
  /** Bars until the next section starts, when that is known (armed); null = at Release. */
  changeIn: number | null;
}

/** Nothing on a sheet line is shorter than this. */
const EPS = 1e-6;
/** Holding "the end" of a span means this far before it - safely inside the last line. */
const HOLD = 1e-4;

/** The status event, reduced to a [`RunPoint`]. */
export function runPoint(s: LooperStatus): RunPoint {
  const bpb = s.beats_per_bar || 4;
  const inBeat = s.samples_per_beat > 0 ? Math.min(1, Math.max(0, s.beat_offset / s.samples_per_beat)) : 0;
  const score = s.score;
  if (!score) {
    return { phase: "none", section: null, bar: 0, pass: 0, beat: 1, inBeat: 0, beatsPerBar: bpb, bpm: s.bpm, armed: null };
  }
  const a = score.armed;
  return {
    phase: score.phase,
    section: score.section,
    bar: score.bar,
    pass: score.pass,
    beat: score.beat,
    inBeat,
    beatsPerBar: bpb,
    bpm: s.bpm,
    armed: a ? { section: a.section, beatsLeft: beatsLeft(a.bars, a.beats, bpb, inBeat), countIn: a.count_in } : null,
  };
}

/** The armed change in beats from the moment of the status: whole beats left minus the part of the
 *  current beat already gone. */
export function beatsLeft(bars: number, beats: number, beatsPerBar: number, inBeat: number): number {
  return bars * beatsPerBar + beats - inBeat;
}

/** Bars into section `index` -> a bar on the sheet. */
export function mapSection(
  sheet: CompiledSheet,
  sections: Pick<CompiledSection, "bars" | "autorelease">[],
  index: number,
  elapsed: number,
): number {
  const span = sheet.spans[index];
  const section = sections[index];
  if (!span || !section) return sheet.total;
  const length = span.end - span.start;
  if (length <= EPS) return span.start;
  let x = Math.max(0, elapsed);
  if (!section.autorelease) {
    // Loops until Release: around its text, every pass - or every `length` bars when the text is
    // longer than a pass.
    const cycle = Math.max(length, section.bars);
    x = x % cycle;
  }
  return span.start + Math.min(x, length - HOLD);
}

/**
 * The sheet's position `dt` seconds after the status `p` was taken. `null`: not playing, the sheet
 * rests at the top.
 */
export function sheetAt(
  sheet: CompiledSheet,
  sections: Pick<CompiledSection, "bars" | "autorelease">[],
  p: RunPoint,
  dt = 0,
): SheetAt | null {
  if (p.phase !== "count_in" && p.phase !== "running") return null;
  const bpb = Math.max(1, p.beatsPerBar);
  const ahead = (Math.max(0, dt) * p.bpm) / 60;
  const at = (index: number, elapsed: number, changeIn: number | null): SheetAt => {
    const span = sheet.spans[index];
    const next = sheet.spans[index + 1];
    return {
      bar: mapSection(sheet, sections, index, elapsed),
      wait: 0,
      section: index,
      spanEnd: span ? span.end : sheet.total,
      nextStart: next ? next.start : null,
      changeIn,
    };
  };

  const armed = p.armed;
  if (armed && ahead >= armed.beatsLeft) {
    // The change has happened since the status (or the runner has not caught up yet).
    if (armed.section === null) return null;
    return at(armed.section, (ahead - armed.beatsLeft) / bpb, null);
  }
  if (p.phase === "count_in" || p.section === null) {
    const first = sheet.spans[0];
    if (!first) return null;
    const wait = armed ? (armed.beatsLeft - ahead) / bpb : 0;
    return {
      bar: first.start,
      wait: Math.max(0, wait),
      section: null,
      spanEnd: first.start,
      nextStart: first.start,
      changeIn: Math.max(0, wait),
    };
  }
  const section = sections[p.section];
  if (!section) return null;
  const elapsed = (p.pass - 1) * section.bars + (p.bar - 1) + (p.beat - 1 + p.inBeat + ahead) / bpb;
  const changeIn = armed && armed.section !== null ? (armed.beatsLeft - ahead) / bpb : null;
  return at(p.section, elapsed, changeIn);
}

// ---------------------------------------------------------------------------------------------
// What to show at a position
// ---------------------------------------------------------------------------------------------

export interface SheetNext {
  text: string;
  rec: boolean;
  /** Bars until it is in force; null = after Release (the section loops, nothing armed). */
  inBars: number | null;
}

export interface SheetState {
  /** Index into `items` of the line or timed direction that runs now; -1 for none. */
  current: number;
  /** 0..1 through the current item. */
  progress: number;
  /** The chord of the current line that sounds now; -1 for none. */
  chord: number;
  /** Items before this index are past (dimmed) - except the directions in force. */
  pastBefore: number;
  /** Indices of the directions in force (NOW). */
  active: number[];
  now: { text: string; rec: boolean } | null;
  next: SheetNext | null;
}

type Timed = Exclude<SheetItem, { kind: "part" }>;
const isTimed = (item: SheetItem): item is Timed => item.kind !== "part" && item.bars > EPS;
const startOf = (item: SheetItem): number => (item.kind === "part" ? item.at : item.start);

/** The current line, the directions in force and the next one, at `at` (`null` = at rest). */
export function sheetState(sheet: CompiledSheet, at: SheetAt | null): SheetState {
  const items = sheet.items;
  const none: SheetState = { current: -1, progress: 0, chord: -1, pastBefore: 0, active: [], now: null, next: null };
  if (at === null || at.wait > 0) {
    // At rest or counting in: nothing runs yet, the first direction from the start is next.
    const from = at ? at.bar : 0;
    const first = items.findIndex((i) => i.kind === "direction" && i.start >= from - EPS);
    if (first < 0) return none;
    const d = items[first] as Extract<SheetItem, { kind: "direction" }>;
    return { ...none, next: { text: d.text, rec: d.rec, inBars: at ? at.wait + (d.start - at.bar) : null } };
  }

  const bar = at.bar;
  let current = -1;
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (!isTimed(item)) continue;
    const start = startOf(item);
    if (bar >= start - EPS && bar < start + item.bars - EPS) {
      current = i;
      break;
    }
  }
  // Where we are in the list: the current item, else the last thing that started.
  let index = current;
  if (index < 0) {
    for (let i = 0; i < items.length; i++) if (items[i].kind !== "part" && startOf(items[i]) <= bar + EPS) index = i;
  }

  let progress = 0;
  let chord = -1;
  if (current >= 0) {
    const item = items[current] as Exclude<SheetItem, { kind: "part" }>;
    progress = Math.min(1, Math.max(0, (bar - item.start) / item.bars));
    if (item.kind === "line" && item.chords.length > 0) {
      chord = Math.min(item.chords.length - 1, Math.floor(progress * item.chords.length));
    }
  }

  // The directions in force: the last one at or before here, with the untimed ones written right
  // above it (several lines of one instruction).
  let last = -1;
  for (let i = index; i >= 0; i--) {
    if (items[i].kind === "direction") {
      last = i;
      break;
    }
  }
  let first = last;
  while (first > 0) {
    const above = items[first - 1];
    if (above.kind !== "direction" || above.bars > EPS) break;
    first--;
  }
  const active: number[] = [];
  if (last >= 0) for (let i = first; i <= last; i++) active.push(i);
  const dirs = active.map((i) => items[i] as Extract<SheetItem, { kind: "direction" }>);
  const now = dirs.length > 0 ? { text: dirs.map((d) => d.text).join("  +  "), rec: dirs.some((d) => d.rec) } : null;

  let next: SheetNext | null = null;
  for (let i = index + 1; i < items.length; i++) {
    const item = items[i];
    if (item.kind !== "direction") continue;
    let inBars: number | null = item.start - bar;
    if (item.start >= at.spanEnd - EPS && at.nextStart !== null) {
      // It belongs to the next section: that comes at the change - known when armed.
      inBars = at.changeIn === null ? null : at.changeIn + Math.max(0, item.start - at.nextStart);
    }
    next = { text: item.text, rec: item.rec, inBars };
    break;
  }

  return { current, progress, chord, pastBefore: current >= 0 ? current : index + 1, active, now, next };
}

/** "in 3 bars", "in 1 bar", "after Release" - whole bars, rounded up, at least one. */
export function inBarsText(inBars: number | null): string {
  if (inBars === null) return "after Release";
  const n = Math.max(1, Math.ceil(inBars - 1e-3));
  return `in ${n} bar${n === 1 ? "" : "s"}`;
}
