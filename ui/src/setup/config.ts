//! The start configuration: what the engine is opened with, stored in localStorage so it survives a
//! restart, and the helpers that turn a device survey into choices.
//!
//! On Windows the defaults are the values phase 0 measured there (ASIO, Focusrite, 48 kHz, 128
//! frames, 827 frames latency). On the Mac they are CoreAudio, 48 kHz, 128 frames and a latency of
//! 596 frames that is only an estimate from what CoreAudio reports - nobody has measured it with a
//! cable, and the Setup page says so.

import { useSyncExternalStore } from "react";
import type { ConfigInfo, DeviceInfo, StartBusOut, StartConfig, StartTrack } from "../types";
import { IS_MAC } from "../platform";

export const STORE_KEY = "looper.setup";

export const SAMPLE_RATES = [44100, 48000, 88200, 96000];

/**
 * Default compensation per platform, mirrored from `looper_engine::audio::DEFAULT_LATENCY_FRAMES`.
 * Windows: measured with a loopback cable. Mac: estimated from CoreAudio's own figures for a
 * Scarlett 2i2 at 48 kHz / 128 frames - not measured, and likely a little short.
 */
export const DEFAULT_LATENCY = IS_MAC ? 596 : 827;
export const DEFAULT_LATENCY_MEASURED = !IS_MAC;

/** The four ways a source can be routed, in the order they are actually used. */
export const BUS_CHOICES: { value: string; label: string }[] = [
  { value: "main+monitor", label: "Main + Monitor" },
  { value: "main", label: "Main only" },
  { value: "monitor", label: "Monitor only" },
  { value: "none", label: "None" },
];

export const DEFAULT_SETUP: StartConfig = {
  host: IS_MAC ? "coreaudio" : "asio",
  device: null,
  sample_rate: 48000,
  buffer_frames: 128,
  input_channels: null,
  output_channels: null,
  force_buffer: false,
  tracks: [
    {
      name: "stimme",
      input_channel: 1,
      input_channel_right: null,
      pan: 0,
      latency_frames: null,
      latency_trim: 0,
      bus: "main+monitor",
      monitor_bus: "monitor",
    },
    {
      name: "gitarre",
      input_channel: 2,
      input_channel_right: null,
      pan: 0,
      latency_frames: null,
      latency_trim: 0,
      bus: "main+monitor",
      monitor_bus: "monitor",
    },
  ],
  bpm: 100,
  beats_per_bar: 4,
  beat_unit: 4,
  bars: 8,
  quantize: "loop",
  overdub_mode: "latch",
  count_in_bars: 1,
  latency_frames: DEFAULT_LATENCY,
  monitor_gain: 1,
  click_gain: 1,
  click: true,
  monitor: false,
  // Null on all four: the engine picks the split that fits the device (1-2 and 3-4 from four
  // outputs on, both on 1-2 below that) and says so out loud when it has to fold them together.
  bus_out_main: null,
  bus_out_monitor: null,
  bus_gain_main: null,
  bus_gain_monitor: null,
};

export function loadStored(): StartConfig {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (!raw) return DEFAULT_SETUP;
    const parsed = JSON.parse(raw) as Partial<StartConfig>;
    // Merge, so a field added later still has its default - including per track, which is what
    // keeps a setup stored before the stereo rebuild loadable.
    const tracks: StartTrack[] = parsed.tracks?.length
      ? parsed.tracks.map((entry) => {
          // A setup stored before the stereo rebuild has neither of the two channel fields, one
          // stored before the per-track latency has neither latency field.
          const stored = entry as Partial<StartTrack>;
          return {
            name: stored.name ?? "track",
            input_channel: stored.input_channel ?? 1,
            input_channel_right: stored.input_channel_right ?? null,
            pan: stored.pan ?? 0,
            latency_frames: stored.latency_frames ?? null,
            latency_trim: stored.latency_trim ?? 0,
            // A setup stored before the buses existed has neither field, and the defaults are the
            // ones the engine would have used anyway.
            bus: stored.bus ?? "main+monitor",
            monitor_bus: stored.monitor_bus ?? "monitor",
            instrument: stored.instrument ?? null,
          };
        })
      : DEFAULT_SETUP.tracks;
    // `latency_samples` was the old name of the global default; it always counted frames, the name
    // just did not say so. A setup stored under it keeps its number instead of silently falling
    // back to 827.
    // Destructure the legacy name out of the rest: spreading `parsed` unchanged would carry it
    // along, and the Rust side accepts it as an alias of `latency_frames` - so a setup stored
    // under the old name would arrive carrying both spellings and be rejected as a duplicate field.
    const { latency_samples: legacy, ...rest } = parsed as Partial<StartConfig> & {
      latency_samples?: number;
    };
    const latency_frames = parsed.latency_frames ?? legacy ?? DEFAULT_SETUP.latency_frames;
    return { ...DEFAULT_SETUP, ...rest, latency_frames, tracks };
  } catch {
    return DEFAULT_SETUP;
  }
}

export function store(config: StartConfig): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(config));
  } catch {
    /* a full or blocked storage is not worth an error */
  }
}

/** Tightest buffer range every configuration of this device agrees on. */
export function bufferRange(device: DeviceInfo | null): { min: number; max: number } | null {
  if (!device) return null;
  const configs = [...device.input_configs, ...device.output_configs].filter(
    (c) => c.min_buffer_frames !== null && c.max_buffer_frames !== null,
  );
  if (configs.length === 0) return null;
  const min = Math.max(...configs.map((c) => c.min_buffer_frames as number));
  const max = Math.min(...configs.map((c) => c.max_buffer_frames as number));
  return max >= min ? { min, max } : null;
}

export function covers(configs: ConfigInfo[], rate: number): boolean {
  return configs.some((c) => c.min_sample_rate <= rate && rate <= c.max_sample_rate);
}

export function maxChannels(device: DeviceInfo | null): number {
  if (!device || device.input_configs.length === 0) return 8;
  return Math.max(...device.input_configs.map((c) => c.channels));
}

/** Output channels this device offers - what a bus may be routed to. */
export function maxOutChannels(device: DeviceInfo | null): number {
  if (!device || device.output_configs.length === 0) return 2;
  return Math.max(...device.output_configs.map((c) => c.channels));
}

/** Where a bus leaves, as a single string a select can carry: `1-2`, `3-4`, `1`, `2`. */
export function busOutValue(out: StartBusOut | null, fallback: StartBusOut): string {
  const o = out ?? fallback;
  return o.width >= 2 ? `${o.channel}-${o.channel + 1}` : `${o.channel}`;
}

export function parseBusOut(value: string): StartBusOut {
  const [first, second] = value.split("-");
  return { channel: Number(first), width: second === undefined ? 1 : 2 };
}

/** Every routing a device with `outs` output channels can offer, pairs first. */
export function busOutChoices(outs: number): { value: string; label: string }[] {
  const list: { value: string; label: string }[] = [];
  for (let c = 1; c + 1 <= outs; c += 2) list.push({ value: `${c}-${c + 1}`, label: `Output ${c}–${c + 1}` });
  for (let c = 1; c <= outs; c += 1) list.push({ value: `${c}`, label: `Output ${c} (mono)` });
  return list;
}

/** A frame count as milliseconds, which is the unit an ear has for it. */
export function ms(frames: number, sampleRate: number): string {
  return `${((frames / (sampleRate || 48000)) * 1000).toFixed(2)} ms`;
}


// ---------------------------------------------------------------------------------------------
// One shared copy, so the Setup page and the "Start engine" buttons elsewhere start the same thing.
// ---------------------------------------------------------------------------------------------

let current: StartConfig = loadStored();
const listeners = new Set<() => void>();

export function currentConfig(): StartConfig {
  return current;
}

export function setConfig(update: StartConfig | ((c: StartConfig) => StartConfig)): void {
  current = typeof update === "function" ? update(current) : update;
  store(current);
  for (const l of listeners) l();
}

export function useConfig(): StartConfig {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}
