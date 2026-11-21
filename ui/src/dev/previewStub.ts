//! Browser-only stand-in for the Tauri bridge, for `dev/devchain.html`: answers the plugin
//! commands a device column sends from an in-memory rack and logs every call in
//! `window.__calls`, so a drag can be checked end to end with real pointer events in a browser
//! (`scripts/drag-check.mjs`). Never part of the app bundle.

import type { RackView, SlotView } from "../plugins/types";

type Call = { cmd: string; args: Record<string, unknown> };

declare global {
  interface Window {
    __calls: Call[];
    __rack: RackView;
    __onRack?: (rack: RackView) => void;
    __TAURI_INTERNALS__?: unknown;
  }
}

const slot = (n: number, name: string, vendor: string, before: SlotView["before"], latency = 0): SlotView => ({
  target: "track.1",
  slot: n,
  plugin_id: `vst3:${name}`,
  name,
  vendor,
  format: "vst3",
  bypass: false,
  latency,
  has_editor: true,
  editor_open: false,
  active: true,
  blocks: 100,
  peak: 0.2,
  cpu_us: 40,
  error: null,
  bypass_address: `track.theme.insert.${n + 1}.bypass`,
  track: "theme",
  before,
});

const params = new URLSearchParams(location.search);
// `?plugins=2`: a second plugin at the end, to step through plugins at the same place.
const extra = params.get("plugins") === "2";

window.__calls = [];
window.__rack = {
  supported: true,
  engine_running: true,
  sample_rate: 48000,
  max_slots: 8,
  slots: [slot(0, "IRDX Studio", "Bogren Digital", null, 8), ...(extra ? [slot(1, "ValhallaRoom", "Valhalla DSP", null)] : [])],
  parked: [],
};

/** The rotation the host and the engine do. */
function rotate(index: number, from: number, to: number): number {
  if (index === from) return to;
  if (from < to && index > from && index <= to) return index - 1;
  if (to < from && index >= to && index < from) return index + 1;
  return index;
}

window.__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
    window.__calls.push({ cmd, args });
    if (cmd === "plugin_place") {
      const { from, to, before } = args as { from: number; to: number; before: SlotView["before"] };
      window.__rack = {
        ...window.__rack,
        slots: window.__rack.slots.map((s) => ({
          ...s,
          slot: rotate(s.slot, from, to),
          before: s.slot === from ? before : s.before,
        })),
      };
      return window.__rack;
    }
    if (cmd === "plugin_params") return [];
    if (cmd === "plugin_catalog") return { supported: true, plugins: [], cache_path: "" };
    if (cmd.startsWith("plugin:event")) return 0;
    if (cmd.startsWith("plugin_")) return window.__rack;
    throw new Error(`not in the preview: ${cmd}`);
  },
};
