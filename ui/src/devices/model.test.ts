import { describe, expect, it } from "vitest";
import { builtinSummary, defaultDevice, deviceOrder, pluginSlotOf, pluginSummary } from "./model";
import type { FxStatus } from "../types";

const fx = (over: Partial<FxStatus> = {}): FxStatus =>
  ({
    bypass: false,
    preset: "custom",
    preset_label: "",
    letters: "-----",
    effects: [],
    high_pass_hz: 80,
    bands: [
      { index: 0, number: 1, kind: "peak", hz: 300, q: 1, gain_db: 0 },
      { index: 1, number: 2, kind: "peak", hz: 3000, q: 1, gain_db: 2.5 },
      { index: 2, number: 3, kind: "high_shelf", hz: 8000, q: 0.7, gain_db: 0 },
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
    delay_ms: 375,
    delay_feedback: 0.3,
    delay_mix: 0.22,
    reverb_size: 0.62,
    reverb_damping: 0.45,
    reverb_mix: 0.18,
    ...over,
  }) as FxStatus;

describe("device column", () => {
  it("lists the devices in signal order: instrument, built-in, plug-ins", () => {
    expect(deviceOrder({ instrument: true, builtin: true, plugins: [0, 3] })).toEqual([
      "instrument",
      "octaver",
      "high_pass",
      "eq",
      "comp",
      "delay",
      "reverb",
      "plugin.0",
      "plugin.3",
    ]);
    expect(deviceOrder({ instrument: false, builtin: false, plugins: [1] })).toEqual(["plugin.1"]);
    expect(pluginSlotOf("plugin.3")).toBe(3);
    expect(pluginSlotOf("eq")).toBeNull();
  });

  it("opens the EQ of a track and the first insert of a bus by default", () => {
    expect(defaultDevice(deviceOrder({ instrument: false, builtin: true, plugins: [] }))).toBe("eq");
    expect(defaultDevice(deviceOrder({ instrument: false, builtin: false, plugins: [2] }))).toBe("plugin.2");
    expect(defaultDevice([])).toBeNull();
  });

  it("sums each built-in device up in a few characters", () => {
    const f = fx();
    expect(builtinSummary(f, "high_pass")).toBe("80 Hz");
    expect(builtinSummary(f, "eq")).toBe("1 band");
    expect(builtinSummary(fx({ bands: f.bands.map((b) => ({ ...b, gain_db: 0 })) }), "eq")).toBe("flat");
    expect(builtinSummary(f, "comp")).toBe("−18 dB · 3:1");
    expect(builtinSummary(fx({ comp_ratio: 2.5 }), "comp")).toBe("−18 dB · 2.5:1");
    expect(builtinSummary(f, "delay")).toBe("1/8. · 22 %");
    expect(builtinSummary(f, "reverb")).toBe("62 % room · 18 %");
  });

  it("says what is wrong with a plugin before who made it", () => {
    const s = { vendor: "Valhalla DSP", format: "vst3" as const, bypass: false, active: true, error: null, latency: 0 };
    expect(pluginSummary(s)).toBe("Valhalla DSP");
    expect(pluginSummary({ ...s, latency: 64 })).toBe("Valhalla DSP · 64 smp");
    expect(pluginSummary({ ...s, active: false })).toBe("VST3 · starts with the engine");
    expect(pluginSummary({ ...s, error: "boom" })).toBe("failed to start");
  });
});
