//! Everything a channel can ask the engine to do, wired to the commands once. The mixer, the
//! detail panel and the Live view's track drawer all use this one object, so a control means the
//! same thing wherever it is.
//!
//! A refusal arrives as a sentence and goes to the toasts (`run`); nothing here throws.

import { api } from "../api";
import { run } from "../shell/notes";
import type { BusId, TrackSourceId } from "../types";
import type { FxActions } from "./fxModel";

export interface ChannelActions extends FxActions {
  gain: (track: number, gain: number) => void;
  mute: (track: number, on: boolean) => void;
  solo: (track: number, on: boolean) => void;
  monitor: (track: number, on: boolean) => void;
  bus: (track: number, source: TrackSourceId, bus: BusId, on: boolean) => void;
  pan: (track: number, pan: number) => void;
  latency: (track: number, measured: number | null, trim: number) => void;
  layerMute: (track: number, layer: number, muted: boolean) => void;
  layerRemove: (track: number, layer: number) => void;
  layerGain: (track: number, layer: number, gain: number) => void;
  clear: (track: number) => void;
}

export const channelActions: ChannelActions = {
  gain: (t, gain) => run(() => api.trackGain(t, gain)),
  mute: (t, on) => run(() => api.trackMute(t, on)),
  solo: (t, on) => run(() => api.trackSolo(t, on)),
  monitor: (t, on) => run(() => api.trackMonitor(t, on)),
  bus: (t, source, bus, on) => run(() => api.trackBus(t, source, bus, on)),
  pan: (t, pan) => run(() => api.trackPan(t, pan)),
  latency: (t, measured, trim) => run(() => api.trackLatency(t, measured, trim)),
  layerMute: (t, l, muted) => run(() => api.layerMute(t, l, muted)),
  layerRemove: (t, l) => run(() => api.layerRemove(t, l)),
  layerGain: (t, l, gain) => run(() => api.layerGain(t, l, gain)),
  clear: (t) => run(() => api.trackClear(t)),
  fxBypass: (t, on) => run(() => api.fxBypass(t, on)),
  fxEnable: (t, effect, on) => run(() => api.fxEnable(t, effect, on)),
  fxPreset: (t, preset) => run(() => api.fxPreset(t, preset)),
  fxSet: (t, param, value, band) => run(() => api.fxSet(t, param, value, band)),
  fxBandKind: (t, band, kind) => run(() => api.fxBandKind(t, band, kind)),
  fxDelayNote: (t, note) => run(() => api.fxDelayNote(t, note)),
};

export const busActions = {
  gain: (bus: BusId, gain: number) => run(() => api.busGain(bus, gain)),
  output: (bus: BusId, channel: number, width: number) => run(() => api.busOutput(bus, channel, width)),
  click: (on: boolean) => run(() => api.setClick(on)),
  clickGain: (gain: number) => run(() => api.clickGain(gain)),
  monitorGain: (gain: number) => run(() => api.monitorGain(gain)),
};
