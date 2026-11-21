//! Browser-only preview of one device column with a mocked engine and plugin host - the owner's
//! case: track "theme", everything off but the reverb, the delay unfolded, IRDX Studio after the
//! chain. Open `dev/devchain.html` on a vite dev server; `scripts/drag-check.mjs` drives it with
//! real pointer events. Never part of the app bundle.

import "./previewStub";
import { createRoot } from "react-dom/client";
import "../design/tokens.css";
import "../design/icons.css";
import "../design/app.css";
import "../design/live.css";
import "../shell/shell.css";
import { DeviceChain } from "../devices/DeviceChain";
import { openDevice } from "../devices/store";
import { setRack } from "../plugins/store";
import { pushStatus } from "../status";
import { IDLE_STATUS, type FxStatus, type LooperStatus } from "../types";
import { FX_SLOT_ORDER } from "../mixer/fxModel";

const fx = {
  bypass: false,
  preset: "custom",
  preset_label: "Custom",
  letters: "-----R",
  effects: FX_SLOT_ORDER.map((name) => ({ name, label: name, on: name === "reverb" })),
  latency_frames: 0,
  octaver_dry: 1,
  octaver_sub: 0,
  octaver_tone_hz: 8000,
  octaver_drive: 0,
  high_pass_hz: 80,
  bands: [
    { index: 0, number: 1, kind: "peak", hz: 300, q: 1, gain_db: 0 },
    { index: 1, number: 2, kind: "peak", hz: 3000, q: 0.9, gain_db: 0 },
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
} as unknown as FxStatus;

pushStatus({
  ...IDLE_STATUS,
  sample_rate: 48000,
  tracks: [
    { name: "theme", fx },
    { name: "voice", fx },
  ],
} as unknown as LooperStatus);

setRack(window.__rack);
openDevice("track.1", "delay");

const noop = () => undefined;
const actions = { fxBypass: noop, fxEnable: noop, fxPreset: noop, fxSet: noop, fxBandKind: noop, fxDelayNote: noop };

createRoot(document.getElementById("root")!).render(
  <div style={{ width: 420, padding: 16, background: "var(--bg-1)" }}>
    <DeviceChain owner="track.1" track={{ index: 0, name: "theme", instrument: false, fx, actions }} size="dense" />
  </div>,
);
