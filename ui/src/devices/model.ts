//! The device chain as data: which devices a channel has, in signal order, what each is called,
//! and the one-line summary its row shows. No React here, so the tests can read it.
//!
//! A channel is a column of devices, top to bottom as the signal flows:
//!
//! ```text
//!  [Instrument]                         instrument tracks only
//!  Octaver → High-pass → EQ → Compressor → Delay → Reverb   built in, fixed order (the engine's)
//!  Plug-in 1 … 8                         anywhere between them, or after them (the default)
//!  + Add plugin
//! ```
//!
//! The built-in devices keep their order; a plug-in is dragged to any gap between them. Its place
//! travels as `before` (the built-in device it sits in front of), in the rack file and in the
//! score's `inserts:` alike.
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

/** A plug-in of a column: its slot, and the built-in device it sits in front of (none = after them). */
export interface PluginPlace {
  slot: number;
  before?: FxSlotName | null;
}

/** 0..5: in front of that built-in device; 6: after the built-in chain. */
export const placeRank = (before: FxSlotName | null | undefined): number =>
  before ? FX_SLOT_ORDER.indexOf(before) : FX_SLOT_ORDER.length;

/**
 * The devices of a column in signal order: the instrument, then the built-in devices with every
 * plug-in in front of the device it is placed before (in slot order), then the plug-ins after the
 * chain. A bare slot number is a plug-in after the chain; a bus has only plug-ins.
 */
export function deviceOrder(opts: { instrument: boolean; builtin: boolean; plugins: (number | PluginPlace)[] }): DeviceId[] {
  const places = opts.plugins
    .map((p) => (typeof p === "number" ? { slot: p, before: null } : p))
    .map((p) => ({ slot: p.slot, rank: opts.builtin ? placeRank(p.before) : FX_SLOT_ORDER.length }))
    .sort((a, b) => a.rank - b.rank || a.slot - b.slot);
  const out: DeviceId[] = opts.instrument ? ["instrument"] : [];
  if (opts.builtin) {
    FX_SLOT_ORDER.forEach((name, rank) => {
      for (const p of places) if (p.rank === rank) out.push(pluginId(p.slot));
      out.push(name);
    });
  }
  for (const p of places) if (p.rank === FX_SLOT_ORDER.length) out.push(pluginId(p.slot));
  return out;
}

/** Where slot `index` ends up when the plug-in in `from` moves to `to` - the engine's rotation. */
export function movedIndex(index: number, from: number, to: number): number {
  if (index === from) return to;
  if (from < to && index > from && index <= to) return index - 1;
  if (to < from && index >= to && index < from) return index + 1;
  return index;
}

/**
 * Dropping plug-in `from` into the column `order` so that it lands in front of row `at` (an index
 * into `order`; `order.length` = the very end). Answers the slot and the place the host is asked
 * for (`pluginApi.place`), or null when nothing would change.
 *
 * The place is the first built-in device below the drop point (none: after the chain). The slot is
 * chosen so that slot order stays signal order; the plug-in keeps its own slot whenever that is
 * enough - moving it across built-in devices only keeps every MIDI address of its slot.
 */
export function placeDrop(
  order: DeviceId[],
  from: number,
  at: number,
  maxSlots: number,
): { to: number; before: FxSlotName | null } | null {
  const own = order.indexOf(pluginId(from));
  if (own < 0) return null;
  const rest = order.filter((id) => id !== pluginId(from));
  const point = Math.max(0, Math.min(rest.length, own < at ? at - 1 : at));
  const below = rest.slice(point).find((id) => (FX_SLOT_ORDER as string[]).includes(id)) as FxSlotName | undefined;
  const before = below ?? null;
  // How many plug-ins sit in front of the drop point: the moved one has to be next in slot order.
  const ahead = rest.slice(0, point).filter((id) => pluginSlotOf(id) !== null).length;
  const others = rest.map(pluginSlotOf).filter((s): s is number => s !== null);
  const fits = (to: number) => others.filter((s) => movedIndex(s, from, to) < to).length === ahead;
  const candidates = [from, ...Array.from({ length: maxSlots }, (_, i) => i)];
  const to = candidates.find(fits);
  if (to === undefined) return null;
  const was = order.slice(own + 1).find((id) => (FX_SLOT_ORDER as string[]).includes(id)) ?? null;
  if (to === from && was === before) return null;
  return { to, before };
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
    case "octaver":
      return `sub ${pct(fx.octaver_sub)} · dry ${pct(fx.octaver_dry)}`;
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
