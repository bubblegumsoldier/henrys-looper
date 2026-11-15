//! A channel's built-in effect chain, every parameter of it: preset and chain bypass, the five
//! effects as a chain of letters, the EQ curve with draggable bands, and the knobs of whichever
//! effect is selected. Used by the mixer's detail panel and by the Live view's track drawer.
//!
//! This replaces the old `FxRow`; everything it could do is here: presets, the bypass ("panic")
//! switch, each effect on and off, the high-pass cutoff, three bands with shape/frequency/Q/gain,
//! six compressor knobs with a gain-reduction meter, the delay's note value, feedback and mix, and
//! the reverb's size, damping and mix. Every control carries its `data-midi` address.

import { useEffect, useRef, useState } from "react";
import { METER_FLOOR_DB, READOUT_MS, createMeter, meterFeedDb, meterTick, onFrame } from "../design/meter";
import { Popover } from "./Popover";
import {
  fxAddress,
  fxBandAddress,
  fxBandKindAddress,
  fxDelayNoteAddress,
  fxPresetAddress,
  fxSlotAddress,
} from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { useStatusEffect, useStatusSlice } from "../status";
import type { FxParamName, FxSlotName, FxStatus, LooperStatus } from "../types";
import { HBar, Knob, linear, logarithmic, type Curve } from "./controls";
import { EqGraph, type EqPick } from "./EqGraph";
import { PluginSlots } from "../plugins/PluginSlots";
import {
  BAND_KINDS,
  BAND_PARAMS,
  DELAY_NOTES,
  FX_LETTER,
  FX_NAME,
  FX_PARAMS,
  FX_PRESETS,
  FX_SHORT,
  FX_SLOT_ORDER,
  presetLabel,
  type FxActions,
  type ParamSpec,
} from "./fxModel";

const curveOf = (p: ParamSpec): Curve => (p.log ? logarithmic(p.min, p.max) : linear(p.min, p.max));

/** One chain-wide knob by its `fx_set` name. */
function ParamKnob({
  track,
  name,
  value,
  actions,
  caption,
}: {
  track: number;
  name: keyof typeof FX_PARAMS;
  value: number;
  actions: FxActions;
  caption?: string;
}) {
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
      label={caption ?? p.label}
      caption={caption ?? p.label}
      title={p.title}
    />
  );
}

/** Bottom of the reduction meter, and the depth below which it turns red. */
const REDUCTION_FLOOR_DB = -20;
const REDUCTION_DEEP_DB = -12;

/**
 * The compressor's gain reduction. Never renders: the status feeds the shared meter ballistics
 * (design/meter.ts) - the bar deepens at once and comes back slowly - and the shared frame loop
 * draws it; the number changes at most four times a second.
 */
export function CompReduction({ track, on }: { track: number; on: boolean }) {
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

function OnSwitch({ track, slot, on, actions }: { track: number; slot: FxSlotName; on: boolean; actions: FxActions }) {
  return (
    <button
      className={`btn s onsw${on ? " on" : ""}`}
      data-midi={fxSlotAddress(track, slot)}
      onClick={() => actions.fxEnable(track, slot, !on)}
      aria-pressed={on}
      title={`${FX_NAME[slot]} ${on ? "on - click to switch off" : "off - click to switch on"}`}
    >
      <Icon name="power" />
      {on ? "On" : "Off"}
    </button>
  );
}

interface Props {
  track: number;
  fx: FxStatus;
  actions: FxActions;
}

export function FxEditor({ track, fx, actions }: Props) {
  const [sel, setSel] = useState<FxSlotName | "plugins">("eq");
  const [pick, setPick] = useState<EqPick>(0);
  const sampleRate = useStatusSlice((s) => s.sample_rate);
  const on = (name: FxSlotName) => fx.effects.find((e) => e.name === name)?.on ?? false;
  const band = typeof pick === "number" ? fx.bands[pick] : undefined;

  const choosePick = (p: EqPick) => {
    setPick(p);
    setSel(p === "hp" ? "high_pass" : "eq");
  };

  return (
    <>
      <div className="fxd">
        <div className="frow">
          <label>Preset</label>
          <div>
            <PresetPicker track={track} fx={fx} actions={actions} />
            <button
              className={`btn s byp${fx.bypass ? " out" : ""}`}
              data-midi={fxAddress(track, "bypass")}
              onClick={() => actions.fxBypass(track, !fx.bypass)}
              aria-pressed={fx.bypass}
              title={
                fx.bypass
                  ? "The chain is bypassed: the track passes bit for bit. Click to switch it in."
                  : "The chain is in the signal path. Click takes it out - bit for bit, the panic switch."
              }
            >
              Bypass
            </button>
          </div>
        </div>

        <div className={`chain${fx.bypass ? " byp" : ""}`} role="tablist" aria-label="Effects">
          {FX_SLOT_ORDER.map((name) => (
            <button
              key={name}
              role="tab"
              aria-selected={sel === name}
              className={`${on(name) ? "on" : ""}${sel === name ? " sel" : ""}`}
              onClick={() => {
                setSel(name);
                if (name === "high_pass") setPick("hp");
                else if (name === "eq" && pick === "hp") setPick(0);
              }}
              onDoubleClick={() => actions.fxEnable(track, name, !on(name))}
              title={`${FX_NAME[name]} (${on(name) ? "on" : "off"}). Click to edit, double-click to switch.`}
            >
              <b>{FX_LETTER[name]}</b>
              {FX_SHORT[name]}
            </button>
          ))}
          <button
            role="tab"
            aria-selected={sel === "plugins"}
            className={`plug${sel === "plugins" ? " sel" : ""}`}
            onClick={() => setSel("plugins")}
            title="Plugin inserts of this channel: after the built-in chain, before the fader"
          >
            <b>P</b>Plugin
          </button>
        </div>

        {sel === "plugins" ? (
          <PluginSlots target={`track.${track + 1}`} />
        ) : (
          <EqGraph track={track} fx={fx} sampleRate={sampleRate} actions={actions} pick={pick} onPick={choosePick} />
        )}

        {sel !== "plugins" && (
        <div className="fxsec">
          <div className="fxsec-h">
            <span className="caps">{sel === "eq" && band ? `EQ · band ${band.number}` : FX_NAME[sel]}</span>
            {sel === "eq" && (
              <span className="bandpick seg" role="radiogroup" aria-label="Band">
                {fx.bands.map((b) => (
                  <button key={b.index} className={pick === b.index ? "on" : ""} onClick={() => setPick(b.index)} role="radio" aria-checked={pick === b.index}>
                    {b.number}
                  </button>
                ))}
              </span>
            )}
            <div className="grow" />
            <OnSwitch track={track} slot={sel} on={on(sel)} actions={actions} />
          </div>

          {sel === "high_pass" && (
            <div className="kn-row">
              <ParamKnob track={track} name="high_pass_hz" value={fx.high_pass_hz} actions={actions} />
              <div className="kn-note">12 dB/oct Butterworth, −3 dB at the cutoff</div>
            </div>
          )}

          {sel === "eq" && band && (
            <>
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
          )}

          {sel === "comp" && (
            <>
              <CompReduction track={track} on={on("comp")} />
              <div className="kn-row three">
                <ParamKnob track={track} name="comp_threshold_db" value={fx.comp_threshold_db} actions={actions} />
                <ParamKnob track={track} name="comp_ratio" value={fx.comp_ratio} actions={actions} />
                <ParamKnob track={track} name="comp_makeup_db" value={fx.comp_makeup_db} actions={actions} />
                <ParamKnob track={track} name="comp_attack_ms" value={fx.comp_attack_ms} actions={actions} />
                <ParamKnob track={track} name="comp_release_ms" value={fx.comp_release_ms} actions={actions} />
                <ParamKnob track={track} name="comp_knee_db" value={fx.comp_knee_db} actions={actions} />
              </div>
            </>
          )}

          {sel === "delay" && (
            <>
              <DelayNotes track={track} fx={fx} actions={actions} />
              <div className="kn-row">
                <ParamKnob track={track} name="delay_feedback" value={fx.delay_feedback} actions={actions} />
                <ParamKnob track={track} name="delay_mix" value={fx.delay_mix} actions={actions} />
              </div>
            </>
          )}

          {sel === "reverb" && (
            <div className="kn-row">
              <ParamKnob track={track} name="reverb_size" value={fx.reverb_size} actions={actions} />
              <ParamKnob track={track} name="reverb_damping" value={fx.reverb_damping} actions={actions} />
              <ParamKnob track={track} name="reverb_mix" value={fx.reverb_mix} actions={actions} />
            </div>
          )}
        </div>
        )}
      </div>

      {/* The three effects one rides while playing, always in view - as in the mockup. */}
      <div className="fxd">
        <span className="caps">Compressor · Delay · Reverb</span>
        <CompReduction track={track} on={on("comp")} />
        <DelayNotes track={track} fx={fx} actions={actions} />
        <div className="frow">
          <label>Reverb mix</label>
          <div>
            <MixBar track={track} value={fx.reverb_mix} actions={actions} />
          </div>
        </div>
      </div>
    </>
  );
}

function DelayNotes({ track, fx, actions }: Props) {
  return (
    <div className="frow">
      <label>
        Delay <span className="dim num">{Math.round(fx.delay_ms)} ms</span>
      </label>
      <div>
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
      </div>
    </div>
  );
}

/** The reverb share as a bar one drags sideways - the mockup's "Hall-Anteil". */
function MixBar({ track, value, actions }: { track: number; value: number; actions: FxActions }) {
  const p = FX_PARAMS.reverb_mix;
  return (
    <>
      <HBar
        className="grow"
        value={value}
        def={p.def}
        curve={linear(0, 1)}
        format={p.format}
        onChange={(v) => actions.fxSet(track, "reverb_mix", v)}
        address={fxAddress(track, p.address)}
        label="Reverb mix"
      />
      <span className="num pct">{p.format(value)}</span>
    </>
  );
}

/** The preset as a drop-down, as in the mockup; every entry carries its MIDI address. */
function PresetPicker({ track, fx, actions }: Props) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  return (
    <>
      <button
        className="field sel presetfield"
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
        aria-expanded={anchor !== null}
        title={fx.preset === "custom" ? "A knob was moved since the last preset" : "Load a ready-made chain"}
      >
        {presetLabel(fx.preset)}
        <Icon name="chev-d" className="chev" />
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} className="menu" width={anchor.offsetWidth}>
          {FX_PRESETS.map((p, n) => (
            <button
              key={p.name}
              role="menuitemradio"
              aria-checked={fx.preset === p.name}
              className={fx.preset === p.name ? "on" : ""}
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
