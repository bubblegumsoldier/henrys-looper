// The stage without an engine: the app opens on it, so it has to be the whole stage - track tiles,
// transport and clock - and not an empty page with a "start the engine" card.

import { describe, expect, it, vi } from "vitest";

// No DOM here (the tests run in node): the modules only need a `window` to hang dev helpers on.
vi.hoisted(() => {
  (globalThis as unknown as { window: unknown }).window ??= globalThis;
});
// Rendered to a string, the stores' snapshot is also their server snapshot.
vi.mock("react", async (original) => {
  const react = await original<typeof import("react")>();
  const useSyncExternalStore: typeof react.useSyncExternalStore = (subscribe, get, server) =>
    react.useSyncExternalStore(subscribe, get, server ?? get);
  return { ...react, default: { ...react, useSyncExternalStore }, useSyncExternalStore };
});
import { renderToStaticMarkup } from "react-dom/server";
import { LiveView } from "./LiveView";
import { pushStatus } from "../status";
import { engineWords } from "../shell/engine";
import { shouldReload } from "../shell/reloadScore";
import { IDLE_STATUS, type EngineStateView, type FxStatus, type LooperStatus, type LooperTrackStatus } from "../types";

const FX: FxStatus = {
  bypass: false,
  preset: "custom",
  preset_label: "",
  letters: "------",
  effects: [],
  latency_frames: 0,
  latency_ms: 0,
  octaver_dry: 1,
  octaver_sub: 1,
  octaver_tone_hz: 2000,
  octaver_drive: 0,
  high_pass_hz: 80,
  bands: [],
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
};

function resting(index: number, name: string): LooperTrackStatus {
  return {
    index,
    name,
    input_channel: index + 1,
    input_channels: [index + 1],
    channels: 1,
    channels_label: "mono",
    pan: 0,
    bus_main: true,
    bus_monitor: true,
    monitor_bus_main: false,
    monitor_bus_monitor: true,
    latency_frames: 0,
    latency_ms: 0,
    latency_measured: null,
    latency_trim: 0,
    latency_inherited: true,
    state: "empty",
    state_label: "empty",
    monitor: false,
    playing: false,
    loop_samples: 0,
    loop_seconds: 0,
    filled_samples: 0,
    take_bar: 0,
    loop_bars: 0,
    input_peak: 0,
    input_dbfs: null,
    input_peaks: [0],
    output_peak: 0,
    output_dbfs: null,
    output_peaks: [0, 0],
    layers: [],
    pending_kind: null,
    pending_label: null,
    pending_bars: 0,
    pending_beats: 0,
    latched: false,
    fx: FX,
    gain: 1,
    gain_db: 0,
    muted: false,
    soloed: false,
    strip_peaks: [0, 0],
    strip_peak: 0,
  };
}

/** What the Rust side sends while no engine runs (`host::ready_event`). */
const READY: LooperStatus = {
  ...IDLE_STATUS,
  ready: true,
  bpm: 96,
  beats_per_bar: 4,
  beat_unit: 4,
  bars: 4,
  sample_rate: 48000,
  click: true,
  tracks: [resting(0, "guitar"), resting(1, "voice"), resting(2, "keys")],
};

describe("the stage without an engine", () => {
  it("shows every track of the setup as a tile, at rest", () => {
    pushStatus(READY);
    const html = renderToStaticMarkup(<LiveView onOpenSetup={() => {}} onOpenArrangement={() => {}} />);
    for (const name of ["guitar", "voice", "keys"]) expect(html).toContain(name);
    expect(html).not.toContain("Engine stopped");
    expect(html).not.toContain("Start engine");
    // The transport and the clock are there too.
    expect(html).toContain("96");
  });

  it("is the stage even before the Rust side has said anything", () => {
    pushStatus(IDLE_STATUS);
    const html = renderToStaticMarkup(<LiveView onOpenSetup={() => {}} onOpenArrangement={() => {}} />);
    expect(html).not.toContain("Start engine");
  });
});

describe("loading the last score again", () => {
  const on = { autostart: true, reloadScore: true };
  const note = { path: "/scores/set.yaml", name: "set.yaml" };
  it("only when it was loaded, both settings are on and the same file is open", () => {
    expect(shouldReload(on, note, "/scores/set.yaml")).toBe("reload");
    expect(shouldReload(on, null, "/scores/set.yaml")).toBe("skip");
    expect(shouldReload({ ...on, reloadScore: false }, note, "/scores/set.yaml")).toBe("skip");
    expect(shouldReload({ ...on, autostart: false }, note, "/scores/set.yaml")).toBe("skip");
    expect(shouldReload(on, note, "/scores/other.yaml")).toBe("other-file");
  });
});

describe("the status pill", () => {
  const view = (patch: Partial<EngineStateView>): EngineStateView => ({
    phase: "off",
    message: null,
    missing: null,
    alternative: null,
    permission: false,
    restart_needed: false,
    autostart: true,
    ...patch,
  });

  it("says where the engine stands, calmly", () => {
    expect(engineWords(view({ phase: "running" }))).toBeNull();
    expect(engineWords(view({ phase: "starting" }))).toBe("Engine starting…");
    expect(engineWords(view({ phase: "off" }))).toBe("Engine off – starts when you play");
    expect(engineWords(view({ phase: "stopped" }))).toBe("Engine off – starts when you play");
    expect(engineWords(view({ phase: "waiting", missing: "Scarlett 2i2 USB" }))).toBe("Waiting for Scarlett 2i2 USB");
    expect(engineWords(view({ phase: "failed", message: "Engine could not start: x" }))).toBe("Engine could not start");
    // Before the first answer: off, not "stopped".
    expect(engineWords(null)).toBe("Engine off – starts when you play");
  });
});
