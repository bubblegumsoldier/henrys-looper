//! Wire types of the plugin host (`app/src-tauri/src/plugins.rs`, `engine/src/plugin/`).
//!
//! A **slot owner** (`target`) is spelled `track.1` … `track.8` (1-based) or `bus.main` /
//! `bus.monitor`. A **slot** is 0-based on the wire and shown 1-based. Parameter values are always
//! **normalised 0..1**; `text` is the plugin's own display of the value.

export type PluginFormat = "vst3" | "au";
export type PluginKind = "effect" | "instrument" | "other";
/** How the scan's child-process probe went. Only `ok` can be loaded into a slot. */
export type ScanStatus = "ok" | "failed" | "crashed" | "timeout" | "instrument";

/** One plugin of the catalogue. */
export interface ScannedPlugin {
  /** `vst3:<class id hex>` or `au:<type>:<subtype>:<manufacturer>`. Stable across scans. */
  id: string;
  format: PluginFormat;
  name: string;
  vendor: string;
  version: string;
  /** VST3 sub-categories (`Fx|Reverb`) or the AU type. */
  category: string;
  kind: PluginKind;
  /** Bundle path (VST3 only). */
  path: string | null;
  status: ScanStatus;
  /** Why, when `status` is not `ok`. */
  message: string | null;
  /** Latency reported during the probe at 48 kHz, frames. */
  latency: number;
  params: number;
}

export interface CatalogView {
  /** False on a build without a plugin host (Windows for now). */
  supported: boolean;
  plugins: ScannedPlugin[];
  cache_path: string;
}

/** One occupied insert slot. */
export interface SlotView {
  target: string;
  slot: number;
  plugin_id: string;
  name: string;
  vendor: string;
  format: PluginFormat;
  bypass: boolean;
  /** Frames; made up for on the track's loop playback (not on the live input). */
  latency: number;
  has_editor: boolean;
  editor_open: boolean;
  /** Installed in the running engine. False while the engine is stopped. */
  active: boolean;
  /** Blocks processed so far; grows while audio passes. */
  blocks: number;
  /** Output peak since the last report, linear. */
  peak: number;
  /** Longest process call since the last report, µs. */
  cpu_us: number;
  /** Why the plugin did not start, if it did not. */
  error: string | null;
  /** MIDI address of the slot's bypass, e.g. `track.1.insert.2.bypass`. */
  bypass_address: string;
}

export interface RackView {
  supported: boolean;
  engine_running: boolean;
  sample_rate: number | null;
  /** Slots per track and per bus (8). */
  max_slots: number;
  slots: SlotView[];
}

export interface ParamView {
  id: number;
  name: string;
  unit: string;
  value: number;
  text: string;
  default: number;
  /** 0 = continuous; 1 = switch; n = n+1 positions. */
  steps: number;
  automatable: boolean;
  read_only: boolean;
  is_bypass: boolean;
  /** MIDI address, e.g. `track.1.insert.1.param.42`. */
  address: string;
}

export interface ParamValue {
  id: number;
  value: number;
  text: string;
}

/** What arrives on `looper://plugins`. */
export type PluginEvent =
  | { kind: "rack"; rack: RackView }
  | { kind: "params"; target: string; slot: number; values: ParamValue[] }
  | { kind: "scan"; label: string }
  | { kind: "catalog"; catalog: CatalogView }
  | { kind: "notice"; message: string };

export const EMPTY_RACK: RackView = {
  supported: true,
  engine_running: false,
  sample_rate: null,
  max_slots: 8,
  slots: [],
};
