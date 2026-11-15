//! The device chain as data: which devices a channel has, in signal order, what each is called,
//! and the one-line summary its row shows. No React here, so the tests can read it.
//!
//! A channel is a column of devices, top to bottom as the signal flows:
//!
//! ```text
//!  [Instrument]                         instrument tracks only
//!  High-pass → EQ → Compressor → Delay → Reverb      built in, fixed order (the engine's)
//!  Plug-in 1 … 8                         after the built-in chain, reorderable among themselves
//!  + Add plugin
//! ```
//!
//! Buses have no built-in chain: their column is the plugin inserts only.

import type { FxSlotName, FxStatus } from "../types";
import type { SlotView } from "../plugins/types";
import { DELAY_NOTES, FX_NAME, FX_SLOT_ORDER, asHz, fxOn } from "../mixer/fxModel";

/** Whose chain: `track.1` … `track.8`, `bus.main`, `bus.monitor` - the plugin host's names. */
export type Owner = string;

export const trackOwner = (index: number): Owner => `track.${index + 1}`;
export const busOwner = (id: string): Owner => `bus.${id}`;
export const isBus = (owner: Owner) => owner.startsWith("bus.");

/** One device of a column. */
export type DeviceId = "instrument" | FxSlotName | `plugin.${number}`;

export const pluginId = (slot: number): DeviceId => `plugin.${slot}`;
export const pluginSlotOf = (id: DeviceId | null): number | null =>
  id?.startsWith("plugin.") ? Number(id.slice(7)) : null;

/** The devices of a column in signal order. */
export function deviceOrder(opts: { instrument: boolean; builtin: boolean; plugins: number[] }): DeviceId[] {
  return [
    ...(opts.instrument ? (["instrument"] as DeviceId[]) : []),
    ...(opts.builtin ? FX_SLOT_ORDER : []),
    ...opts.plugins.map(pluginId),
  ];
}

/** The device a column opens with when nothing was opened before. */
export function defaultDevice(order: DeviceId[]): DeviceId | null {
  if (order.includes("eq")) return "eq";
  return order[0] ?? null;
}

const pct = (v: number) => `${Math.round(v * 100)} %`;
const ratio = (v: number) => {
  const r = Math.round(v * 10) / 10;
  return `${r % 1 === 0 ? r.toFixed(0) : r.toFixed(1)}:1`;
};
const dbInt = (v: number) => `${v < -0.5 ? "−" : ""}${Math.abs(Math.round(v))} dB`;

/** Bands that do something - a band at 0 dB is a wire. */
export const activeBands = (fx: Pick<FxStatus, "bands">) => fx.bands.filter((b) => b.gain_db !== 0).length;

/** The summary right of a built-in device's name. */
export function builtinSummary(fx: FxStatus, slot: FxSlotName): string {
  switch (slot) {
    case "high_pass":
      return asHz(fx.high_pass_hz);
    case "eq": {
      const n = activeBands(fx);
      return n === 0 ? "flat" : `${n} ${n === 1 ? "band" : "bands"}`;
    }
    case "comp":
      return `${dbInt(fx.comp_threshold_db)} · ${ratio(fx.comp_ratio)}`;
    case "delay":
      return `${DELAY_NOTES.find((n) => n.name === fx.delay_note)?.label ?? fx.delay_note} · ${pct(fx.delay_mix)}`;
    case "reverb":
      return `${pct(fx.reverb_size)} room · ${pct(fx.reverb_mix)}`;
  }
}

/** The state of a plugin in a few words: what is wrong, or who made it. */
export function pluginSummary(s: Pick<SlotView, "vendor" | "format" | "bypass" | "active" | "error" | "latency">): string {
  if (s.error) return "failed to start";
  const format = s.format === "vst3" ? "VST3" : "AU";
  if (!s.active) return `${format} · starts with the engine`;
  return [s.vendor || format, s.latency > 0 ? `${s.latency} smp` : ""].filter(Boolean).join(" · ");
}

/** Whether a built-in device is heard: switched on, and the chain not bypassed. */
export function builtinHeard(fx: FxStatus, slot: FxSlotName): boolean {
  return fxOn(fx, slot) && !fx.bypass;
}

export const deviceName = (id: DeviceId, plugins: SlotView[]): string => {
  if (id === "instrument") return "Instrument";
  const slot = pluginSlotOf(id);
  if (slot !== null) return plugins.find((p) => p.slot === slot)?.name ?? "Plugin";
  return FX_NAME[id as FxSlotName];
};
