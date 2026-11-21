//! Every plugin-host command (`app/src-tauri/src/plugins.rs`). Rejections carry an English sentence.
//!
//! `target` is `track.1` … `track.8`, `bus.main` or `bus.monitor`; `slot` is 0-based.

import { invoke } from "@tauri-apps/api/core";
import type { FxSlotName } from "../types";
import type { CatalogView, ParamValue, ParamView, RackView } from "./types";

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
  }
}

export const pluginApi = {
  /** The last scan's result, from the cache. Fast. */
  catalog: () => call<CatalogView>("plugin_catalog"),
  /** Probe new or changed plugins (each in a child process); `full` probes everything again. */
  scan: (full = false) => call<CatalogView>("plugin_scan", { full }),
  rack: () => call<RackView>("plugin_rack"),
  /** Into `slot`, or the first free one when `slot` is omitted. An occupied slot is replaced. */
  load: (target: string, pluginId: string, slot?: number) =>
    call<RackView>("plugin_load", { target, slot: slot ?? null, plugin_id: pluginId }),
  remove: (target: string, slot: number) => call<RackView>("plugin_remove", { target, slot }),
  /** Move within one chain; the slots in between shift by one. */
  move: (target: string, from: number, to: number) =>
    call<RackView>("plugin_move", { target, from, to }),
  /**
   * Move to slot `to` and to a place in the track's built-in chain: in front of `before`, or after
   * the chain (`null`). Refused when it would put the plugins out of signal order - see
   * `devices/model.ts` `placeDrop`, which computes both.
   */
  place: (target: string, from: number, to: number, before: FxSlotName | null) =>
    call<RackView>("plugin_place", { target, from, to, before }),
  /** A copy - same plugin, full state, bypass and place - onto another track or bus. */
  copy: (target: string, slot: number, to: string) => call<RackView>("plugin_copy", { target, slot, to }),
  /**
   * The whole device chain of track `from` onto track `to` (0-based): built-in effects with every
   * setting, then copies of its plugins; the target's plugins are replaced, fader and mute stay.
   */
  copyChain: (from: number, to: number) => call<RackView>("chain_copy", { from, to }),
  bypass: (target: string, slot: number, on: boolean) =>
    call<RackView>("plugin_bypass", { target, slot, on }),
  params: (target: string, slot: number) => call<ParamView[]>("plugin_params", { target, slot }),
  /** Normalised 0..1. Answers with what the plugin actually took. */
  setParam: (target: string, slot: number, id: number, value: number) =>
    call<ParamValue>("plugin_set_param", { target, slot, id, value }),
  /**
   * The plugin's own window. Fails with a sentence when it has none - show `params` instead. Opening
   * it starts the engine first when it is not running: a plugin one plays wants to be heard.
   */
  editor: async (target: string, slot: number, open: boolean) => {
    if (open) await invoke("engine_ensure").catch(() => undefined);
    return call<RackView>("plugin_editor", { target, slot, open });
  },
  /** The complete plugin state as base64 (a preset to keep). */
  stateGet: (target: string, slot: number) => call<string>("plugin_state_get", { target, slot }),
  stateSet: (target: string, slot: number, data: string) =>
    call<RackView>("plugin_state_set", { target, slot, data }),
};
