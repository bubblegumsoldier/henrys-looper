import { describe, expect, it } from "vitest";
import { builtinSummary, defaultDevice, deviceOrder, dropIndex, movedIndex, placeDrop, pluginSlotOf, pluginSummary, stepPlace } from "./model";
import type { FxSlotName, FxStatus } from "../types";

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

describe("plug-ins anywhere in the chain", () => {
  // An IR loader in front of everything (slot 0), a de-esser before the compressor (slot 1), a
  // delay after the chain (slot 2).
  const plugins = [
    { slot: 2, before: null },
    { slot: 0, before: "octaver" as const },
    { slot: 1, before: "comp" as const },
  ];
  const order = deviceOrder({ instrument: false, builtin: true, plugins });

  it("puts every plug-in in front of the built-in device it is placed before", () => {
    expect(order).toEqual(["plugin.0", "octaver", "high_pass", "eq", "plugin.1", "comp", "delay", "reverb", "plugin.2"]);
    // A bus has no built-in devices: its plug-ins in slot order, whatever they say.
    expect(deviceOrder({ instrument: false, builtin: false, plugins })).toEqual(["plugin.0", "plugin.1", "plugin.2"]);
    expect(deviceOrder({ instrument: true, builtin: true, plugins: [{ slot: 4, before: "eq" }] }).slice(0, 4)).toEqual([
      "instrument",
      "octaver",
      "high_pass",
      "plugin.4",
    ]);
  });

  it("drops a plug-in in front of a built-in device without changing its slot", () => {
    // The delay (slot 2) dragged above the reverb: only its place changes - its MIDI addresses
    // (`insert.3.…`) stay, and every built-in address is by name anyway.
    expect(placeDrop(order, 2, order.indexOf("reverb"), 8)).toEqual({ to: 2, before: "reverb" });
    // The IR loader dragged from the front to just above the EQ.
    expect(placeDrop(order, 0, order.indexOf("eq"), 8)).toEqual({ to: 0, before: "eq" });
    // The de-esser to the very end, after the delay: it has to follow it in slot order too.
    expect(placeDrop(order, 1, order.length, 8)).toEqual({ to: 2, before: null });
    // The delay to the very front, ahead of the IR loader: slot 0, the others shift.
    expect(placeDrop(order, 2, 0, 8)).toEqual({ to: 0, before: "octaver" });
  });

  it("does nothing when a plug-in is dropped where it already is", () => {
    const at = order.indexOf("plugin.1");
    expect(placeDrop(order, 1, at, 8)).toBeNull();
    expect(placeDrop(order, 1, at + 1, 8)).toBeNull();
    expect(placeDrop(order, 9, 0, 8)).toBeNull();
  });

  it("keeps slot order and signal order the same, gaps in the slots included", () => {
    // Slots 0 and 3 taken (1, 2 empty): the plug-in in slot 3 dragged in front of the one in slot 0.
    const gappy = deviceOrder({ instrument: false, builtin: true, plugins: [{ slot: 0, before: "eq" }, { slot: 3, before: null }] });
    const move = placeDrop(gappy, 3, gappy.indexOf("high_pass"), 8);
    expect(move).toEqual({ to: 0, before: "high_pass" });
    expect(movedIndex(0, 3, 0)).toBe(1);
  });
});

describe("moving a plug-in without HTML5 drag and drop", () => {
  it("hit-tests a drag by the middles of the row headers", () => {
    // Three rows with headers centred at 10, 40 (an unfolded row from 25 to 120) and 130.
    const mids = [10, 40, 130];
    expect(dropIndex(5, mids)).toBe(0);
    expect(dropIndex(30, mids)).toBe(1);
    expect(dropIndex(90, mids)).toBe(2); // on the unfolded row's controls: behind it
    expect(dropIndex(200, mids)).toBe(3); // below everything: the end
  });

  it("steps a plug-in through all seven places and past others at the same place", () => {
    // The owner's case: everything built in, IRDX Studio after the reverb.
    let place: { slot: number; before: FxSlotName | null } = { slot: 0, before: null };
    const seen: (FxSlotName | null)[] = [];
    for (;;) {
      const order = deviceOrder({ instrument: false, builtin: true, plugins: [place] });
      const step = stepPlace(order, place.slot, -1, 8);
      if (!step) break;
      seen.push(step.before);
      place = { slot: step.to, before: step.before };
    }
    expect(seen).toEqual(["reverb", "delay", "comp", "eq", "high_pass", "octaver"]);
    const front = deviceOrder({ instrument: false, builtin: true, plugins: [{ slot: 0, before: "octaver" }] });
    expect(stepPlace(front, 0, 1, 8)).toEqual({ to: 0, before: "high_pass" });
    const end = deviceOrder({ instrument: false, builtin: true, plugins: [0] });
    expect(stepPlace(end, 0, 1, 8)).toBeNull();
    // Two plug-ins after the chain: one step up passes the other one, the place stays.
    const two = deviceOrder({ instrument: false, builtin: true, plugins: [0, 1] });
    expect(stepPlace(two, 1, -1, 8)).toEqual({ to: 0, before: null });
    // Never above an instrument.
    const inst = deviceOrder({ instrument: true, builtin: true, plugins: [{ slot: 0, before: "octaver" }] });
    expect(stepPlace(inst, 0, -1, 8)).toBeNull();
  });
});
