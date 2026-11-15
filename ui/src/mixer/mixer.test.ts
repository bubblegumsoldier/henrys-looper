import { describe, expect, it } from "vitest";
import { dbToPos, faderText, gainToPos, panText, posToDb, posToGain, MAX_TRACK_GAIN } from "./scale";
import { responseDb } from "./EqGraph";
import type { FxStatus } from "../types";

describe("fader law", () => {
  it("puts the mockup's marks where the mockup has them", () => {
    expect(dbToPos(0)).toBeCloseTo(0.82);
    expect(dbToPos(-6)).toBeCloseTo(0.66);
    expect(dbToPos(-12)).toBeCloseTo(0.5);
    expect(dbToPos(-24)).toBeCloseTo(0.3);
    expect(dbToPos(-48)).toBeCloseTo(0.1);
    expect(dbToPos(-Infinity)).toBe(0);
    expect(dbToPos(12)).toBeCloseTo(1);
  });

  it("is its own inverse", () => {
    for (const p of [0.01, 0.05, 0.1, 0.25, 0.5, 0.7, 0.82, 0.95, 1]) {
      expect(dbToPos(posToDb(p))).toBeCloseTo(p, 6);
    }
    expect(posToGain(gainToPos(1))).toBeCloseTo(1, 6);
    expect(posToGain(1)).toBeCloseTo(MAX_TRACK_GAIN, 4);
    expect(posToGain(0)).toBe(0);
  });

  it("prints values the way a console does", () => {
    expect(faderText(1)).toBe("0.0");
    expect(faderText(0.5)).toBe("−6.0");
    expect(faderText(0)).toBe("−∞");
    expect(panText(0)).toBe("C");
    expect(panText(-0.2)).toBe("L 20");
    expect(panText(1)).toBe("R 100");
  });
});

function chain(patch: Partial<FxStatus>): FxStatus {
  return {
    bypass: false,
    preset: "custom",
    preset_label: "",
    letters: "",
    effects: [
      { name: "high_pass", label: "", on: false },
      { name: "eq", label: "", on: true },
      { name: "comp", label: "", on: false },
      { name: "delay", label: "", on: false },
      { name: "reverb", label: "", on: false },
    ],
    high_pass_hz: 80,
    bands: [
      { index: 0, number: 1, kind: "peak", hz: 1000, q: 1, gain_db: 6 },
      { index: 1, number: 2, kind: "peak", hz: 1000, q: 1, gain_db: 0 },
      { index: 2, number: 3, kind: "peak", hz: 1000, q: 1, gain_db: 0 },
    ],
    comp_threshold_db: -18,
    comp_ratio: 3,
    comp_attack_ms: 12,
    comp_release_ms: 180,
    comp_knee_db: 8,
    comp_makeup_db: 0,
    comp_reduction_db: 0,
    delay_note: "dotted_eighth",
    delay_note_label: "1/8.",
    delay_samples: 0,
    delay_ms: 0,
    delay_feedback: 0.3,
    delay_mix: 0,
    reverb_size: 0.6,
    reverb_damping: 0.5,
    reverb_mix: 0,
    ...patch,
  };
}

const at = (curve: { hz: number; db: number }[], hz: number) =>
  curve.reduce((a, b) => (Math.abs(Math.log(b.hz / hz)) < Math.abs(Math.log(a.hz / hz)) ? b : a)).db;

describe("EQ curve", () => {
  it("lifts a peak band by its gain at its frequency and leaves the ends flat", () => {
    const c = responseDb(chain({}), 48000, 400);
    expect(at(c, 1000)).toBeCloseTo(6, 0);
    expect(Math.abs(at(c, 30))).toBeLessThan(0.3);
  });

  it("draws the high-pass -3 dB at its cutoff", () => {
    const fx = chain({});
    fx.effects[0].on = true;
    fx.effects[1].on = false;
    expect(at(responseDb(fx, 48000, 600), 80)).toBeCloseTo(-3, 0);
  });

  it("is flat when the chain is bypassed", () => {
    const c = responseDb(chain({ bypass: true }), 48000);
    expect(c.every((p) => p.db === 0)).toBe(true);
  });
});
