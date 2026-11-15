//! The built-in effect chain as the user interface sees it: names, letters, presets, note values,
//! and every knob with the range the engine clamps it to.
//!
//! Every range below is read off `engine/src/engine/fx/mod.rs`. The engine clamps again anyway, so
//! a control cannot produce anything unusable - but a knob whose end is not the engine's end is a
//! knob that lies about what is reachable. The defaults are the engine's `Chain::new` values, which
//! is what a double-click on a knob goes back to.

import type { BandKindName, DelayNoteName, FxParamName, FxPresetName, FxSlotName } from "../types";

/** Everything the effect part of a channel can ask the engine to do. */
export interface FxActions {
  fxBypass: (track: number, on: boolean) => void;
  fxEnable: (track: number, effect: FxSlotName, on: boolean) => void;
  fxPreset: (track: number, preset: LoadablePreset) => void;
  fxSet: (track: number, param: FxParamName, value: number, band?: number) => void;
  fxBandKind: (track: number, band: number, kind: BandKindName) => void;
  fxDelayNote: (track: number, note: DelayNoteName) => void;
}

/** `custom` is something the engine reports, never something anybody can load. */
export type LoadablePreset = Exclude<FxPresetName, "custom">;

/**
 * The three chains a user can ask for, in the order the buttons stand in. The digit is what the
 * keyboard chord `V` of the Live view expects, so a button and the key never disagree.
 */
export const FX_PRESETS: { name: LoadablePreset; label: string; hint: string }[] = [
  {
    name: "voice",
    label: "Voice",
    hint: "80 Hz high-pass, presence at 3 kHz, 3:1 from -18 dBFS, 18 % reverb",
  },
  {
    name: "piezo_guitar",
    label: "Guitar",
    hint: "100 Hz high-pass, -4 dB on the quack band at 3 kHz, 2.5:1, 10 % reverb",
  },
  { name: "dry", label: "Dry", hint: "Everything off, chain bypassed - passed through bit for bit" },
];

/** What a preset is called on screen, including the reported `custom`. */
export function presetLabel(preset: FxPresetName): string {
  return FX_PRESETS.find((p) => p.name === preset)?.label ?? "Custom";
}

/** One letter per effect, in signal order: the `H E C D R` strip. */
export const FX_LETTER: Record<FxSlotName, string> = {
  high_pass: "H",
  eq: "E",
  comp: "C",
  delay: "D",
  reverb: "R",
};

/** English names of the five effects; the engine's own `label` is German. */
export const FX_NAME: Record<FxSlotName, string> = {
  high_pass: "High-pass",
  eq: "EQ",
  comp: "Compressor",
  delay: "Delay",
  reverb: "Reverb",
};

/** Short names for the chain buttons, where a whole word does not fit. */
export const FX_SHORT: Record<FxSlotName, string> = {
  high_pass: "High-p.",
  eq: "EQ",
  comp: "Comp",
  delay: "Delay",
  reverb: "Reverb",
};

/** In signal order, so the digit of the keyboard chord `X` is the position in the chain. */
export const FX_SLOT_ORDER: FxSlotName[] = ["high_pass", "eq", "comp", "delay", "reverb"];

/** The note values the tempo-synchronous delay knows, in the order the chord `D` numbers them. */
export const DELAY_NOTES: { name: DelayNoteName; label: string }[] = [
  { name: "quarter", label: "1/4" },
  { name: "dotted_eighth", label: "1/8." },
  { name: "eighth", label: "1/8" },
  { name: "triplet_eighth", label: "1/8T" },
];

export const BAND_KINDS: { name: BandKindName; label: string; short: string }[] = [
  { name: "low_shelf", label: "Low shelf", short: "Low sh." },
  { name: "peak", label: "Peak", short: "Peak" },
  { name: "high_shelf", label: "High shelf", short: "High sh." },
];

// --- how a number is printed ------------------------------------------------

/** The minus sign a musician reads, not a hyphen. */
const minus = (v: number, text: string) => (v < 0 ? `−${text}` : text);

export const asHz = (v: number) => (v < 1000 ? `${Math.round(v)} Hz` : `${(v / 1000).toFixed(v < 10000 ? 2 : 1)} kHz`);
export const asDb = (v: number) => (Math.abs(v) < 0.05 ? "0.0 dB" : minus(v, `${v > 0 ? "+" : ""}${Math.abs(v).toFixed(1)} dB`));
/** A width, not a gain: a knee of 8 dB is eight dB wide, not eight dB louder. */
export const asDbWidth = (v: number) => `${v.toFixed(1)} dB`;
export const asQ = (v: number) => v.toFixed(2);
export const asMs = (v: number) => (v < 10 ? `${v.toFixed(1)} ms` : `${Math.round(v)} ms`);
export const asRatio = (v: number) => `${v.toFixed(1)}:1`;
export const asPercent = (v: number) => `${Math.round(v * 100)} %`;

// --- the knobs ----------------------------------------------------------------

/** One numeric parameter: its range, its curve, its default and how it reads. */
export interface ParamSpec {
  label: string;
  min: number;
  max: number;
  def: number;
  /** Frequencies and times want a logarithmic travel. */
  log?: boolean;
  format: (v: number) => string;
  title?: string;
}

/** Chain-wide knobs, by the name `fx_set` takes. Band knobs are in `BAND_PARAMS`. */
export const FX_PARAMS: Record<Exclude<FxParamName, "band_hz" | "band_q" | "band_gain_db">, ParamSpec & { address: string }> = {
  high_pass_hz: {
    label: "Cutoff",
    min: 20,
    max: 1000,
    def: 80,
    log: true,
    format: asHz,
    address: "high_pass.hz",
    title: "Butterworth, 12 dB per octave, exactly -3 dB at the cutoff. Removes rumble, handling noise and plosives.",
  },
  comp_threshold_db: {
    label: "Threshold",
    min: -60,
    max: 0,
    def: -18,
    format: asDb,
    address: "comp.threshold",
    title: "Above this level the signal is pulled down.",
  },
  comp_ratio: {
    label: "Ratio",
    min: 1,
    max: 20,
    def: 3,
    log: true,
    format: asRatio,
    address: "comp.ratio",
    title: "3:1 is levelling; above 6:1 a voice sounds held down.",
  },
  comp_attack_ms: {
    label: "Attack",
    min: 0.1,
    max: 200,
    def: 12,
    log: true,
    format: asMs,
    address: "comp.attack",
    title: "How long the transient passes. Too fast eats consonants and the pick attack.",
  },
  comp_release_ms: {
    label: "Release",
    min: 5,
    max: 2000,
    def: 180,
    log: true,
    format: asMs,
    address: "comp.release",
    title: "About a syllable or a strum: fast enough to recover, slow enough not to pump.",
  },
  comp_knee_db: {
    label: "Knee",
    min: 0,
    max: 24,
    def: 8,
    format: asDbWidth,
    address: "comp.knee",
    title: "Width of the soft transition around the threshold. 0 is a hard knee.",
  },
  comp_makeup_db: {
    label: "Makeup",
    min: -24,
    max: 24,
    def: 0,
    format: asDb,
    address: "comp.makeup",
    title: "Gives back what the compression took.",
  },
  delay_feedback: {
    label: "Feedback",
    min: 0,
    max: 0.95,
    def: 0.3,
    format: asPercent,
    address: "delay.feedback",
    title: "How much of each repeat goes back in - how many echoes you hear.",
  },
  delay_mix: { label: "Mix", min: 0, max: 1, def: 0, format: asPercent, address: "delay.mix" },
  reverb_size: {
    label: "Size",
    min: 0,
    max: 1,
    def: 0.6,
    format: asPercent,
    address: "reverb.size",
    title: "How big the room is. 62 % is about one and a half seconds.",
  },
  reverb_damping: {
    label: "Damping",
    min: 0,
    max: 1,
    def: 0.5,
    format: asPercent,
    address: "reverb.damping",
    title: "Takes the top end off the tail so it does not rebuild sibilants.",
  },
  reverb_mix: { label: "Mix", min: 0, max: 1, def: 0, format: asPercent, address: "reverb.mix" },
};

export const BAND_PARAMS: Record<"hz" | "gain" | "q", ParamSpec & { param: FxParamName }> = {
  hz: { label: "Freq", min: 20, max: 20000, def: 1000, log: true, format: asHz, param: "band_hz" },
  gain: { label: "Gain", min: -24, max: 24, def: 0, format: asDb, param: "band_gain_db" },
  q: {
    label: "Q",
    min: 0.1,
    max: 12,
    def: 1,
    log: true,
    format: asQ,
    param: "band_q",
    title: "How wide the band acts. Small is broad and musical, large is surgical.",
  },
};
