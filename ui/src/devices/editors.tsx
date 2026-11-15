//! The editor of each built-in device - only that device's parameters, nothing of its neighbours.
//! What unfolds under a device row of `DeviceChain`.
//!
//! Every knob carries its `data-midi` address. Every one of them switches its device on (and the
//! chain in) when turned: the host turns a hand on a knob into `TouchFxParam`.

import { useEffect, useRef, useState } from "react";
import { METER_FLOOR_DB, READOUT_MS, createMeter, meterFeedDb, meterTick, onFrame } from "../design/meter";
import { fxAddress, fxBandAddress, fxBandKindAddress, fxDelayNoteAddress, fxPresetAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { useStatusEffect, useStatusSlice } from "../status";
import type { FxParamName, FxStatus, LooperStatus } from "../types";
import { Knob, linear, logarithmic, type Curve } from "../mixer/controls";
import { EqGraph, type EqPick } from "../mixer/EqGraph";
import { Popover } from "../mixer/Popover";
import {
  BAND_KINDS,
  BAND_PARAMS,
  DELAY_NOTES,
  FX_PARAMS,
  FX_PRESETS,
  presetLabel,
  shownPreset,
  type FxActions,
  type ParamSpec,
} from "../mixer/fxModel";

const curveOf = (p: ParamSpec): Curve => (p.log ? logarithmic(p.min, p.max) : linear(p.min, p.max));

interface EditorProps {
  track: number;
  fx: FxStatus;
  actions: FxActions;
}

/** One chain-wide knob by its `fx_set` name. */
function ParamKnob({ track, name, value, actions }: { track: number; name: keyof typeof FX_PARAMS; value: number; actions: FxActions }) {
  const p = FX_PARAMS[name];
  return (
    <Knob
      small
      value={value}
      def={p.def}
      curve={curveOf(p)}
      format={p.format}
      onChange={(v) => actions.fxSet(track, name as FxParamName, v)}
      address={fxAddress(track, p.address)}
      label={p.label}
      caption={p.label}
      title={p.title}
    />
  );
}

export function HighPassEditor({ track, fx, actions }: EditorProps) {
  const sampleRate = useStatusSlice((s) => s.sample_rate);
  return (
    <>
      <EqGraph track={track} fx={fx} sampleRate={sampleRate} actions={actions} mode="hp" pick="hp" onPick={() => {}} />
      <div className="kn-row">
        <ParamKnob track={track} name="high_pass_hz" value={fx.high_pass_hz} actions={actions} />
        <div className="kn-note">12 dB/oct Butterworth, −3 dB at the cutoff. Takes out rumble, handling noise and plosives.</div>
      </div>
    </>
  );
}

export function EqEditor({ track, fx, actions }: EditorProps) {
  const sampleRate = useStatusSlice((s) => s.sample_rate);
  const [pick, setPick] = useState<number>(0);
  const band = fx.bands[pick] ?? fx.bands[0];
  if (!band) return null;
  return (
    <>
      <EqGraph
        track={track}
        fx={fx}
        sampleRate={sampleRate}
        actions={actions}
        mode="eq"
        pick={pick}
        onPick={(p: EqPick) => typeof p === "number" && setPick(p)}
      />
      <div className="dev-sub">
        <span className="seg bandpick" role="radiogroup" aria-label="Band">
          {fx.bands.map((b) => (
            <button key={b.index} className={pick === b.index ? "on" : ""} onClick={() => setPick(b.index)} role="radio" aria-checked={pick === b.index}>
              Band {b.number}
            </button>
          ))}
        </span>
      </div>
      <div className="seg kinds" role="radiogroup" aria-label="Band shape">
        {BAND_KINDS.map((k) => (
          <button
            key={k.name}
            role="radio"
            aria-checked={band.kind === k.name}
            className={band.kind === k.name ? "on" : ""}
            data-midi={fxBandKindAddress(track, band.index, k.name)}
            onClick={() => actions.fxBandKind(track, band.index, k.name)}
            title="Peak lifts or cuts around one frequency; a shelf moves everything above or below it."
          >
            {k.label}
          </button>
        ))}
      </div>
      <div className="kn-row">
        {(["hz", "gain", "q"] as const).map((part) => {
          const p = BAND_PARAMS[part];
          const value = part === "hz" ? band.hz : part === "gain" ? band.gain_db : band.q;
          const shelf = part === "q" && band.kind !== "peak";
          return (
            <Knob
              key={part}
              small
              value={value}
              def={p.def}
              curve={curveOf(p)}
              format={p.format}
              onChange={(v) => actions.fxSet(track, p.param, v, band.index)}
              address={fxBandAddress(track, band.index, part)}
              label={`Band ${band.number} ${p.label}`}
              caption={p.label}
              title={shelf ? "A shelf has a fixed slope; Q only shapes a peak." : p.title}
              disabled={shelf}
            />
          );
        })}
        <div className="kn">
          <span className={`flat${band.gain_db === 0 ? " on" : ""}`} title="A band at 0 dB is a wire - the engine skips it">
            {band.gain_db === 0 ? "flat" : "active"}
          </span>
          <label>State</label>
        </div>
      </div>
    </>
  );
}

export function CompEditor({ track, fx, actions, on }: EditorProps & { on: boolean }) {
  return (
    <>
      <CompReduction track={track} on={on} />
      <div className="kn-row three">
        <ParamKnob track={track} name="comp_threshold_db" value={fx.comp_threshold_db} actions={actions} />
        <ParamKnob track={track} name="comp_ratio" value={fx.comp_ratio} actions={actions} />
        <ParamKnob track={track} name="comp_makeup_db" value={fx.comp_makeup_db} actions={actions} />
        <ParamKnob track={track} name="comp_attack_ms" value={fx.comp_attack_ms} actions={actions} />
        <ParamKnob track={track} name="comp_release_ms" value={fx.comp_release_ms} actions={actions} />
        <ParamKnob track={track} name="comp_knee_db" value={fx.comp_knee_db} actions={actions} />
      </div>
    </>
  );
}

export function DelayEditor({ track, fx, actions }: EditorProps) {
  return (
    <>
      <div className="dev-sub">
        <div className="seg" role="radiogroup" aria-label="Delay note">
          {DELAY_NOTES.map((n) => (
            <button
              key={n.name}
              role="radio"
              aria-checked={fx.delay_note === n.name}
              className={fx.delay_note === n.name ? "on" : ""}
              data-midi={fxDelayNoteAddress(track, n.name)}
              onClick={() => actions.fxDelayNote(track, n.name)}
              title="The delay follows the tempo, not milliseconds - a tempo change takes it along."
            >
              {n.label}
            </button>
          ))}
        </div>
        <span className="dim num">{Math.round(fx.delay_ms)} ms</span>
      </div>
      <div className="kn-row">
        <ParamKnob track={track} name="delay_feedback" value={fx.delay_feedback} actions={actions} />
        <ParamKnob track={track} name="delay_mix" value={fx.delay_mix} actions={actions} />
      </div>
    </>
  );
}

export function ReverbEditor({ track, fx, actions }: EditorProps) {
  return (
    <div className="kn-row">
      <ParamKnob track={track} name="reverb_size" value={fx.reverb_size} actions={actions} />
      <ParamKnob track={track} name="reverb_damping" value={fx.reverb_damping} actions={actions} />
      <ParamKnob track={track} name="reverb_mix" value={fx.reverb_mix} actions={actions} />
    </div>
  );
}

/** Bottom of the reduction meter, and the depth below which it turns red. */
const REDUCTION_FLOOR_DB = -20;
const REDUCTION_DEEP_DB = -12;

/**
 * The compressor's gain reduction. Never renders: the status feeds the shared meter ballistics
 * (design/meter.ts) - the bar deepens at once and comes back slowly - and the shared frame loop
 * draws it; the number changes at most four times a second. `mini` is the bar alone, for the row.
 */
export function CompReduction({ track, on, mini }: { track: number; on: boolean; mini?: boolean }) {
  const bar = useRef<HTMLElement | null>(null);
  const text = useRef<HTMLSpanElement | null>(null);
  // The ballistics run in dB above the meter floor: a reduction of 6 dB is "floor + 6".
  const meter = useRef(createMeter());
  useStatusEffect((s: LooperStatus) => {
    const depth = Math.max(0, -(s.tracks[track]?.fx?.comp_reduction_db ?? 0));
    meterFeedDb(meter.current, METER_FLOOR_DB + depth, performance.now());
  });
  useEffect(() => {
    const drawn = { w: "", deep: false, t: "", at: -Infinity };
    return onFrame((now, b) => {
      const m = meter.current;
      meterTick(m, now, b);
      const depth = Math.max(0, m.level - METER_FLOOR_DB);
      const w = Math.min(1, depth / -REDUCTION_FLOOR_DB).toFixed(3);
      if (w !== drawn.w && bar.current) {
        bar.current.style.transform = `scaleX(${w})`;
        drawn.w = w;
      }
      const deep = depth >= -REDUCTION_DEEP_DB;
      if (deep !== drawn.deep && bar.current) {
        bar.current.classList.toggle("deep", deep);
        drawn.deep = deep;
      }
      if (now - drawn.at >= READOUT_MS && text.current) {
        drawn.at = now;
        const t = depth >= 0.05 ? `−${depth.toFixed(1)} dB` : "0.0 dB";
        if (t !== drawn.t) {
          text.current.textContent = t;
          drawn.t = t;
        }
      }
    });
  }, []);
  if (mini) {
    return (
      <span className={`gr mini${on ? "" : " off"}`} title="Gain reduction of the compressor right now">
        <span className="bar">
          <i ref={bar} />
        </span>
      </span>
    );
  }
  return (
    <div className={`gr${on ? "" : " off"}`} title={`Gain reduction of the compressor right now. Full length is ${-REDUCTION_FLOOR_DB} dB.`}>
      <span className="grl">Reduction</span>
      <span className="bar">
        <i ref={bar} />
      </span>
      <span className="num" ref={text}>
        0.0 dB
      </span>
    </div>
  );
}

/** The preset as a drop-down; every entry carries its MIDI address. */
export function PresetPicker({ track, fx, actions }: EditorProps) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const shown = shownPreset(fx);
  return (
    <>
      <button
        className="field sel presetfield"
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
        aria-expanded={anchor !== null}
        title={shown === "custom" ? "A knob was moved or an effect switched since the last preset" : "Load a ready-made chain"}
      >
        <span className="dim2">Preset</span>
        {presetLabel(shown)}
        <Icon name="chev-d" className="chev" />
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} className="menu" width={Math.max(160, anchor.offsetWidth)}>
          {FX_PRESETS.map((p, n) => (
            <button
              key={p.name}
              role="menuitemradio"
              aria-checked={shown === p.name}
              className={shown === p.name ? "on" : ""}
              data-midi={fxPresetAddress(track, p.name)}
              title={`${p.hint}. In Live: key V, then ${n + 1}.`}
              onClick={() => {
                actions.fxPreset(track, p.name);
                setAnchor(null);
              }}
            >
              {p.label}
            </button>
          ))}
        </Popover>
      )}
    </>
  );
}
