//! What a section resolves to, computed on this side of the bridge.
//!
//! The backend already sends `DocSection.resolved` with every parse and render, and that stays the
//! authority. This is the same rule (`resolve_doc` in `engine/src/score/doc.rs`), mirrored so an
//! edit can be drawn in the very frame it is made - before the render round trip has come back -
//! and so the edit operations can ask "what would this cell be without its own value?" without a
//! trip to Rust. A test pins the two together on the example scores' shapes.
//!
//! Defaults are the compiler's: 4 bars, no autorelease, quantize `loop`, track `stop`.

import type { Quantize, ResolvedSection, ScoreDoc, ScoreState, ValueSource } from "../types";

export const DEFAULT_BARS = 4;
export const DEFAULT_AUTORELEASE = false;
export const DEFAULT_QUANTIZE: Quantize = "loop";
export const DEFAULT_STATE: ScoreState = "stop";

/** The sections a section inherits through, itself first. Unknown or circular targets end it. */
export function repeatChain(doc: ScoreDoc, index: number): number[] {
  const chain = [index];
  let current = index;
  for (;;) {
    const target = doc.sections[current]?.repeat;
    if (target === undefined) break;
    const next = doc.sections.findIndex((s) => s.id === target);
    if (next < 0 || chain.includes(next)) break;
    chain.push(next);
    current = next;
  }
  return chain;
}

function origin(doc: ScoreDoc, self: number, at: number): ValueSource {
  return at === self ? { kind: "own" } : { kind: "repeat", index: at, section: doc.sections[at].id };
}

function pick<T>(doc: ScoreDoc, self: number, chain: number[], get: (i: number) => T | undefined, fallback: T) {
  for (const at of chain) {
    const value = get(at);
    if (value !== undefined) return { value, from: origin(doc, self, at) };
  }
  return { value: fallback, from: { kind: "default" } as ValueSource };
}

export function resolveSection(doc: ScoreDoc, index: number): ResolvedSection {
  const chain = repeatChain(doc, index);
  const s = doc.sections;
  return {
    bars: pick(doc, index, chain, (i) => s[i].bars, DEFAULT_BARS),
    autorelease: pick(doc, index, chain, (i) => s[i].autorelease, DEFAULT_AUTORELEASE),
    quantize: pick(doc, index, chain, (i) => s[i].quantize, DEFAULT_QUANTIZE),
    tracks: doc.tracks.map((track) => {
      const hit = pick(doc, index, chain, (i) => s[i].tracks.find((t) => t.track === track.name)?.state, DEFAULT_STATE);
      return { track: track.name, state: hit.value, from: hit.from };
    }),
  };
}

/** A copy of the document with every section's `resolved` recomputed. */
export function resolveDoc(doc: ScoreDoc): ScoreDoc {
  const out: ScoreDoc = { ...doc, sections: doc.sections.map((s) => ({ ...s })) };
  out.sections.forEach((s, i) => {
    s.resolved = resolveSection(out, i);
  });
  return out;
}

/**
 * What one field or one track state of a section would be if the section did not state it itself:
 * the template's value, or the default. This is what "Reset to template" goes back to.
 */
export function inheritedState(doc: ScoreDoc, section: number, track: string): { state: ScoreState; from: ValueSource } {
  const chain = repeatChain(doc, section).slice(1);
  for (const at of chain) {
    const own = doc.sections[at].tracks.find((t) => t.track === track);
    if (own) return { state: own.state, from: { kind: "repeat", index: at, section: doc.sections[at].id } };
  }
  return { state: DEFAULT_STATE, from: { kind: "default" } };
}

/** What a clip says beyond its state (`restart`, `bars`). */
export interface ClipOptions {
  restart: boolean;
  /** `{state: play, monitor: true}`: the input is heard on top of the loop. */
  monitor?: boolean;
  bars?: number;
  /** `skip_end`: beats at the end of the take that are not recorded (the clip's own value). */
  skip_end?: number;
}

/**
 * The options of one clip: those of the section that states the track - itself, or the template
 * it inherits the state from. A section that restates a track has only its own (the compiler's
 * rule in `score/compile.rs`).
 */
export function clipOptions(doc: ScoreDoc, section: number, track: string): ClipOptions {
  for (const at of repeatChain(doc, section)) {
    const own = doc.sections[at].tracks.find((t) => t.track === track);
    if (own)
      return {
        restart: own.restart === true,
        bars: own.bars,
        ...(own.monitor ? { monitor: true } : {}),
        ...(own.skip_end !== undefined ? { skip_end: own.skip_end } : {}),
      };
  }
  return { restart: false };
}

/** Which states a clip option goes with - the compiler refuses it with any other. */
export const RESTART_STATES: ScoreState[] = ["play", "overdub", "stack"];
export const TAKE_BARS_STATES: ScoreState[] = ["record"];
/** `monitor: true` goes with play only: the takes and hear_through hear the input anyway. */
export const MONITOR_STATES: ScoreState[] = ["play"];
/** `skip_end` goes with the takes that end: record and overdub. */
export const SKIP_END_STATES: ScoreState[] = ["record", "overdub"];

export type SectionField = "bars" | "autorelease" | "quantize";

export function inheritedField<K extends SectionField>(
  doc: ScoreDoc,
  section: number,
  field: K,
): { value: NonNullable<ScoreDoc["sections"][number][K]>; from: ValueSource } {
  const chain = repeatChain(doc, section).slice(1);
  for (const at of chain) {
    const value = doc.sections[at][field];
    if (value !== undefined) {
      return {
        value: value as NonNullable<ScoreDoc["sections"][number][K]>,
        from: { kind: "repeat", index: at, section: doc.sections[at].id },
      };
    }
  }
  const fallback = { bars: DEFAULT_BARS, autorelease: DEFAULT_AUTORELEASE, quantize: DEFAULT_QUANTIZE }[field];
  return { value: fallback as unknown as NonNullable<ScoreDoc["sections"][number][K]>, from: { kind: "default" } };
}

// ---------------------------------------------------------------------------------------------
// Layout and derived facts
// ---------------------------------------------------------------------------------------------

/** Where each section starts and how long it is, in bars, from the resolved values. */
export interface SectionSpan {
  start: number;
  bars: number;
}

export function sectionSpans(doc: ScoreDoc): SectionSpan[] {
  let at = 0;
  return doc.sections.map((s, i) => {
    const bars = Math.max(1, (s.resolved ?? resolveSection(doc, i)).bars.value);
    const span = { start: at, bars };
    at += bars;
    return span;
  });
}

export function totalBars(doc: ScoreDoc): number {
  return sectionSpans(doc).reduce((sum, s) => sum + s.bars, 0);
}

/** `"3/4"` -> 3 beats per bar; the old `beats_per_bar` alias when that is what the file uses. */
export function beatsPerBar(doc: ScoreDoc): number {
  const sig = doc.time_signature?.match(/^\s*(\d+)\s*\/\s*(\d+)\s*$/);
  if (sig) return Math.max(1, Number(sig[1]));
  return doc.beats_per_bar ?? 4;
}

/** Seconds one bar takes. The tempo counts the denominator's note value, as the engine does. */
export function secondsPerBar(doc: ScoreDoc): number {
  return doc.bpm > 0 ? (beatsPerBar(doc) * 60) / doc.bpm : 0;
}

/**
 * Which layer each cell produces, walking the score front to back the way an uninterrupted run
 * would: `record` starts a fresh loop (layer 1), `overdub` adds one, `stack` adds one per pass
 * (at least one - how many depends on when Release is pressed), `play` plays however many
 * there are. `layers` is the count once the section has run; `recordsLoop` tells a take that only
 * defines the loop (nothing is stacked on it later) from the first of several layers.
 */
export interface CellLayer {
  layers: number;
  /** For `record` and `overdub`: the number of the layer this cell writes. */
  writes: number | null;
  /** True for a `record` that is followed by an `overdub` on the same loop. */
  stacked: boolean;
  /** A `stack` ran before (or here): `layers` is the least there will be, possibly more. */
  open?: boolean;
}

export function deriveLayers(doc: ScoreDoc): CellLayer[][] {
  const out: CellLayer[][] = doc.sections.map(() => []);
  doc.tracks.forEach((track, t) => {
    let layers = 0;
    let open = false;
    let lastRecord: CellLayer | null = null;
    doc.sections.forEach((section, s) => {
      const resolved = section.resolved ?? resolveSection(doc, s);
      const state = resolved.tracks.find((r) => r.track === track.name)?.state ?? DEFAULT_STATE;
      let cell: CellLayer;
      if (state === "record") {
        layers = 1;
        open = false;
        cell = { layers, writes: 1, stacked: false };
        lastRecord = cell;
      } else if (state === "overdub" || state === "stack") {
        layers = layers === 0 ? 1 : layers + 1;
        if (state === "stack") open = true;
        cell = { layers, writes: layers, stacked: false, open };
        if (lastRecord) lastRecord.stacked = true;
      } else {
        cell = { layers, writes: null, stacked: false, open };
      }
      out[s][t] = cell;
    });
  });
  return out;
}
