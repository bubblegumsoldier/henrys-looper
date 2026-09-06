//! The effect chain of one track, on the track card.
//!
//! Two ways of using this contradict each other, and the layout is the answer to that:
//!
//! * **While playing** a musician wants a name and a switch. Nobody dials a corner frequency on
//!   stage with a guitar in his hands. So the presets, the panic switch and the five effect
//!   switches are always visible and big enough to hit.
//! * **While producing** the same person wants three EQ bands. So everything finer sits behind
//!   `Details`, folded away, and the card stays as short as it was without it.
//!
//! Rendering: this component is inside `TrackCard`, which only re-renders when the structure
//! signature changes (see `status.ts`) - and the whole chain apart from one field is in that
//! signature. The exception is the compressor's gain reduction, which moves in every one of the
//! twenty events per second; `CompMeter` writes it straight into the DOM and never renders.
//!
//! Every range below is read off `engine/src/engine/fx/mod.rs`, where the engine clamps it. The
//! engine clamps again anyway, so a slider cannot produce anything unusable - but a slider whose
//! end is not the engine's end is a slider that lies about what is reachable.

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  fxAddress,
  fxBandAddress,
  fxDelayNoteAddress,
  fxPresetAddress,
  fxSlotAddress,
} from "../midi/addresses";
import { useStatusEffect } from "../status";
import type {
  BandKindName,
  DelayNoteName,
  FxParamName,
  FxPresetName,
  FxSlotName,
  FxStatus,
  LooperStatus,
} from "../types";

/** Everything the effect part of a card can ask the engine to do. */
export interface FxActions {
  fxBypass: (track: number, on: boolean) => void;
  fxEnable: (track: number, effect: FxSlotName, on: boolean) => void;
  fxPreset: (track: number, preset: LoadablePreset) => void;
  fxSet: (track: number, param: FxParamName, value: number, band?: number) => void;
  fxBandKind: (track: number, band: number, kind: BandKindName) => void;
  fxDelayNote: (track: number, note: DelayNoteName) => void;
}

/** `custom` is something the engine reports, never something anybody can load. */
export type LoadablePreset = Exclude<FxPresetName, "custom">;

/** A dragged slider would otherwise send a command per pixel - the same window the gains use. */
const FX_THROTTLE_MS = 60;

/**
 * The three chains a user can ask for, in the order the buttons stand in. The digit is what the
 * keyboard chord `V` expects, so the button and the key can never disagree about the order.
 */
export const FX_PRESETS: { name: LoadablePreset; label: string; hint: string }[] = [
  {
    name: "voice",
    label: "Stimme",
    hint: "80 Hz Hochpass, Präsenz bei 3 kHz, 3:1 ab -18 dBFS, 18 % Hall",
  },
  {
    name: "piezo_guitar",
    label: "Gitarre",
    hint: "100 Hz Hochpass, -4 dB auf das Quäkband bei 3 kHz, 2,5:1, 10 % Hall",
  },
  { name: "dry", label: "Trocken", hint: "Alles aus, Kette umgangen - bitgleich durchgereicht" },
];

/** Mirrors `FxSlot::letter` in the engine: the letters that make up `HEK-R`. */
const FX_LETTER: Record<FxSlotName, string> = {
  high_pass: "H",
  eq: "E",
  comp: "K",
  delay: "D",
  reverb: "R",
};

/** In signal order, so the digit of the keyboard chord `X` is the position in the chain. */
export const FX_SLOT_ORDER: FxSlotName[] = ["high_pass", "eq", "comp", "delay", "reverb"];

/** The note values the tempo-synchronous delay knows, in the order the chord `D` numbers them. */
export const DELAY_NOTES: { name: DelayNoteName; label: string }[] = [
  { name: "quarter", label: "1/4" },
  { name: "dotted_eighth", label: "1/8." },
  { name: "eighth", label: "1/8" },
  { name: "triplet_eighth", label: "1/8T" },
];

const BAND_KINDS: { name: BandKindName; label: string }[] = [
  { name: "peak", label: "Glocke" },
  { name: "low_shelf", label: "Kuhschwanz tief" },
  { name: "high_shelf", label: "Kuhschwanz hoch" },
];

// --- how a number is printed ------------------------------------------------

const asHz = (v: number) => (v < 1000 ? `${Math.round(v)} Hz` : `${(v / 1000).toFixed(2)} kHz`);
const asDb = (v: number) => `${v > 0 ? "+" : ""}${v.toFixed(1)} dB`;
/** A width, not a gain: a knee of 8 dB is eight dB wide, not eight dB louder. */
const asDbWidth = (v: number) => `${v.toFixed(1)} dB`;
const asQ = (v: number) => `Q ${v.toFixed(2)}`;
const asMs = (v: number) => (v < 10 ? `${v.toFixed(1)} ms` : `${Math.round(v)} ms`);
const asRatio = (v: number) => `${v.toFixed(1)}:1`;
const asPercent = (v: number) => `${Math.round(v * 100)} %`;

// ---------------------------------------------------------------------------------------------
// The compressor meter
// ---------------------------------------------------------------------------------------------

/** Bottom of the reduction scale. Deeper than this and the setting is wrong, not the meter. */
const REDUCTION_FLOOR_DB = -20;
/** Below this much reduction the compressor is holding the track down rather than levelling it. */
const REDUCTION_DEEP_DB = -12;

/**
 * How much the compressor is pulling right now, as a bar that grows to the left.
 *
 * This says more about what the compressor does than any of its six numbers: a musician sees at a
 * glance whether it is levelling (a bar that breathes with the phrase) or squashing (a bar that
 * stands still near the end of its travel).
 *
 * Like the level meters, it never renders - `comp_reduction_db` differs in every event.
 */
function CompMeter({ track, on }: { track: number; on: boolean }) {
  const fill = useRef<HTMLDivElement | null>(null);
  const value = useRef<HTMLSpanElement | null>(null);
  const drawn = useRef({ width: -1, text: "", deep: false });

  useStatusEffect((status: LooperStatus) => {
    const db = Math.min(0, status.tracks[track]?.fx?.comp_reduction_db ?? 0);
    const width = Math.round(Math.min(100, (db / REDUCTION_FLOOR_DB) * 100));
    if (width !== drawn.current.width && fill.current) {
      fill.current.style.width = `${width}%`;
      drawn.current.width = width;
    }
    // Anything shallower than a tenth of a dB is not a reduction, it is the detector breathing.
    const text = db <= -0.05 ? `${db.toFixed(1)} dB` : "0.0 dB";
    if (text !== drawn.current.text && value.current) {
      value.current.textContent = text;
      drawn.current.text = text;
    }
    const deep = db <= REDUCTION_DEEP_DB;
    if (deep !== drawn.current.deep && fill.current) {
      fill.current.classList.toggle("live-fx-comp-deep", deep);
      drawn.current.deep = deep;
    }
  });

  return (
    <div
      className={`live-fx-comp${on ? "" : " live-fx-comp-off"}`}
      title={
        on
          ? `Wie viel der Kompressor gerade herunterzieht. Volle Länge sind ${-REDUCTION_FLOOR_DB} dB.`
          : "Der Kompressor ist aus - er zieht nichts."
      }
    >
      <span className="live-fx-comp-label">Komp</span>
      <div className="live-fx-comp-bar">
        <div className="live-fx-comp-fill" ref={fill} style={{ width: "0%" }} />
      </div>
      <span className="live-fx-comp-value mono" ref={value}>
        0.0 dB
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// One knob
// ---------------------------------------------------------------------------------------------

/** Positions of a logarithmic slider. Fine enough that a drag feels continuous. */
const LOG_STEPS = 1000;

interface SliderProps {
  label: string;
  value: number;
  /**
   * Address of the parameter tree this knob is, for the learn mode. The whole label carries it
   * rather than the input, because a range input is a replaced element and cannot show the badge.
   */
  address: string;
  min: number;
  max: number;
  /** Step in the parameter's own unit. A logarithmic slider ignores it and uses `LOG_STEPS`. */
  step: number;
  /**
   * Frequencies want a logarithmic scale: on a linear 20 Hz to 20 kHz slider everything musical
   * happens in the first eighth of an inch, and the last three quarters are dog whistles.
   */
  log?: boolean;
  format: (value: number) => string;
  title?: string;
  onChange: (value: number) => void;
}

/**
 * A parameter slider. Throttled exactly like the layer gain: the slider follows the finger, the
 * engine gets at most one command per window, and the value the finger stopped on is sent at once
 * on release - otherwise the next status would push the previous value back into the slider.
 */
function FxSlider({
  label,
  value,
  address,
  min,
  max,
  step,
  log = false,
  format,
  title,
  onChange,
}: SliderProps) {
  const [local, setLocal] = useState(value);
  const dragging = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pending = useRef<number | null>(null);
  const send = useRef(onChange);
  send.current = onChange;

  useEffect(() => {
    if (!dragging.current) setLocal(value);
  }, [value]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    if (pending.current !== null) {
      send.current(pending.current);
      pending.current = null;
    }
  }, []);

  const push = useCallback((next: number) => {
    pending.current = next;
    if (timer.current !== undefined) return;
    timer.current = window.setTimeout(() => {
      timer.current = undefined;
      if (pending.current !== null) {
        send.current(pending.current);
        pending.current = null;
      }
    }, FX_THROTTLE_MS);
  }, []);

  const toPos = (v: number) =>
    log ? (Math.log(Math.max(v, min) / min) / Math.log(max / min)) * LOG_STEPS : v;
  const fromPos = (p: number) => (log ? min * Math.pow(max / min, p / LOG_STEPS) : p);

  return (
    <label className="live-fx-knob" title={title} data-midi={address}>
      <span className="live-fx-knob-label">{label}</span>
      <input
        className="live-fx-knob-slider"
        type="range"
        min={log ? 0 : min}
        max={log ? LOG_STEPS : max}
        step={log ? 1 : step}
        value={toPos(local)}
        onPointerDown={() => {
          dragging.current = true;
        }}
        onPointerUp={() => {
          flush();
          dragging.current = false;
        }}
        onBlur={flush}
        onChange={(e) => {
          const next = fromPos(Number(e.target.value));
          setLocal(next);
          push(next);
        }}
      />
      <span className="live-fx-knob-value mono">{format(local)}</span>
    </label>
  );
}

// ---------------------------------------------------------------------------------------------
// The folded-out details
// ---------------------------------------------------------------------------------------------

function FxBlock({
  letter,
  title,
  on,
  children,
}: {
  letter: string;
  title: string;
  on: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`live-fx-block${on ? "" : " live-fx-block-off"}`}>
      <div className="live-fx-block-head">
        <span className="live-fx-block-letter mono">{letter}</span>
        <span className="live-fx-block-title">{title}</span>
        {!on && <span className="live-fx-block-tag">aus</span>}
      </div>
      {children}
    </div>
  );
}

function FxDetails({ track, fx, actions }: { track: number; fx: FxStatus; actions: FxActions }) {
  const on = (name: FxSlotName) => fx.effects.find((e) => e.name === name)?.on ?? false;

  return (
    <div className="live-fx-details">
      <FxBlock letter="H" title="Hochpass" on={on("high_pass")}>
        <FxSlider
          label="Grenze"
          value={fx.high_pass_hz}
          address={fxAddress(track, "high_pass.hz")}
          min={20}
          max={1000}
          step={1}
          log
          format={asHz}
          title="Butterworth, 12 dB je Oktave, exakt -3 dB an der Grenze. Nimmt Trittschall, Griffgeräusche und Popplaute weg."
          onChange={(v) => actions.fxSet(track, "high_pass_hz", v)}
        />
      </FxBlock>

      <FxBlock letter="E" title="EQ · drei Bänder" on={on("eq")}>
        {fx.bands.map((b) => (
          <div className="live-fx-band" key={b.index}>
            <div className="live-fx-band-head">
              <span className="live-fx-band-name">Band {b.number}</span>
              <select
                className="live-fx-band-kind"
                value={b.kind}
                title="Glocke hebt oder senkt um eine Frequenz herum, ein Kuhschwanz das ganze Ende darüber oder darunter."
                onChange={(e) => actions.fxBandKind(track, b.index, e.target.value as BandKindName)}
              >
                {BAND_KINDS.map((k) => (
                  <option key={k.name} value={k.name}>
                    {k.label}
                  </option>
                ))}
              </select>
              {b.gain_db === 0 && (
                <span className="live-fx-band-flat" title="Ein Band bei 0 dB ist ein Draht - die Engine rechnet es gar nicht erst">
                  flach
                </span>
              )}
            </div>
            <FxSlider
              label="Frequenz"
              value={b.hz}
              address={fxBandAddress(track, b.index, "hz")}
              min={20}
              max={20000}
              step={1}
              log
              format={asHz}
              onChange={(v) => actions.fxSet(track, "band_hz", v, b.index)}
            />
            <FxSlider
              label="Güte"
              value={b.q}
              address={fxBandAddress(track, b.index, "q")}
              min={0.1}
              max={12}
              step={0.05}
              format={asQ}
              title="Wie breit das Band greift. Klein ist breit und musikalisch, groß ist chirurgisch."
              onChange={(v) => actions.fxSet(track, "band_q", v, b.index)}
            />
            <FxSlider
              label="Pegel"
              value={b.gain_db}
              address={fxBandAddress(track, b.index, "gain")}
              min={-24}
              max={24}
              step={0.5}
              format={asDb}
              onChange={(v) => actions.fxSet(track, "band_gain_db", v, b.index)}
            />
          </div>
        ))}
      </FxBlock>

      <FxBlock letter="K" title="Kompressor" on={on("comp")}>
        <FxSlider
          label="Schwelle"
          value={fx.comp_threshold_db}
          address={fxAddress(track, "comp.threshold")}
          min={-60}
          max={0}
          step={0.5}
          format={asDb}
          title="Ab diesem Pegel wird heruntergezogen. Eine Stimme am Scarlett bei vernünftiger Verstärkung liegt um -10 dBFS."
          onChange={(v) => actions.fxSet(track, "comp_threshold_db", v)}
        />
        <FxSlider
          label="Verhältnis"
          value={fx.comp_ratio}
          address={fxAddress(track, "comp.ratio")}
          min={1}
          max={20}
          step={0.1}
          format={asRatio}
          title="3:1 ist Pegeln, über 6:1 klingt eine Stimme festgehalten."
          onChange={(v) => actions.fxSet(track, "comp_ratio", v)}
        />
        <FxSlider
          label="Attack"
          value={fx.comp_attack_ms}
          address={fxAddress(track, "comp.attack")}
          min={0.1}
          max={200}
          step={0.1}
          log
          format={asMs}
          title="Wie lange der Einschwinger durchgelassen wird. Zu schnell frisst die Konsonanten und den Anschlag."
          onChange={(v) => actions.fxSet(track, "comp_attack_ms", v)}
        />
        <FxSlider
          label="Release"
          value={fx.comp_release_ms}
          address={fxAddress(track, "comp.release")}
          min={5}
          max={2000}
          step={1}
          log
          format={asMs}
          title="Etwa eine Silbe oder ein Anschlag: schnell genug zum Erholen, langsam genug, um nicht zu pumpen."
          onChange={(v) => actions.fxSet(track, "comp_release_ms", v)}
        />
        <FxSlider
          label="Knie"
          value={fx.comp_knee_db}
          address={fxAddress(track, "comp.knee")}
          min={0}
          max={24}
          step={0.5}
          format={asDbWidth}
          title="Breite des weichen Übergangs um die Schwelle. 0 ist ein hartes Knie, das an der Schwelle mehrmals je Sekunde umschaltet."
          onChange={(v) => actions.fxSet(track, "comp_knee_db", v)}
        />
        <FxSlider
          label="Ausgleich"
          value={fx.comp_makeup_db}
          address={fxAddress(track, "comp.makeup")}
          min={-24}
          max={24}
          step={0.5}
          format={asDb}
          title="Gibt zurück, was die Kompression gekostet hat. Einschalten soll dichter machen, nicht lauter."
          onChange={(v) => actions.fxSet(track, "comp_makeup_db", v)}
        />
      </FxBlock>

      <FxBlock letter="D" title="Delay · tempo-synchron" on={on("delay")}>
        <div className="live-fx-notes">
          <span className="live-fx-knob-label">Notenwert</span>
          {DELAY_NOTES.map((n) => (
            <button
              key={n.name}
              className={`btn btn-mini live-fx-note${fx.delay_note === n.name ? " btn-on" : ""}`}
              data-midi={fxDelayNoteAddress(track, n.name)}
              onClick={() => actions.fxDelayNote(track, n.name)}
              title="Die Verzögerung kommt aus der Zeitachse der Engine, nicht aus einer Millisekundenzahl - ein Tempowechsel nimmt sie mit."
            >
              {n.label}
            </button>
          ))}
          <span className="live-fx-knob-value mono">{Math.round(fx.delay_ms)} ms</span>
        </div>
        <FxSlider
          label="Rückkopplung"
          value={fx.delay_feedback}
          address={fxAddress(track, "delay.feedback")}
          min={0}
          max={0.95}
          step={0.01}
          format={asPercent}
          title="Wie viel jeder Wiederholung in die Leitung zurückgeht - also wie viele Echos man hört."
          onChange={(v) => actions.fxSet(track, "delay_feedback", v)}
        />
        <FxSlider
          label="Anteil"
          value={fx.delay_mix}
          address={fxAddress(track, "delay.mix")}
          min={0}
          max={1}
          step={0.01}
          format={asPercent}
          onChange={(v) => actions.fxSet(track, "delay_mix", v)}
        />
      </FxBlock>

      <FxBlock letter="R" title="Hall" on={on("reverb")}>
        <FxSlider
          label="Größe"
          value={fx.reverb_size}
          address={fxAddress(track, "reverb.size")}
          min={0}
          max={1}
          step={0.01}
          format={asPercent}
          title="Wie groß der Raum ist. 62 % sind etwa anderthalb Sekunden."
          onChange={(v) => actions.fxSet(track, "reverb_size", v)}
        />
        <FxSlider
          label="Dämpfung"
          value={fx.reverb_damping}
          address={fxAddress(track, "reverb.damping")}
          min={0}
          max={1}
          step={0.01}
          format={asPercent}
          title="Nimmt dem Nachhall das obere Ende, damit er keine Zischlaute nachbaut."
          onChange={(v) => actions.fxSet(track, "reverb_damping", v)}
        />
        <FxSlider
          label="Anteil"
          value={fx.reverb_mix}
          address={fxAddress(track, "reverb.mix")}
          min={0}
          max={1}
          step={0.01}
          format={asPercent}
          onChange={(v) => actions.fxSet(track, "reverb_mix", v)}
        />
      </FxBlock>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The row itself
// ---------------------------------------------------------------------------------------------

export function FxRow({ track, fx, actions }: { track: number; fx: FxStatus; actions: FxActions }) {
  const [open, setOpen] = useState(false);
  const delayOn = fx.effects.find((e) => e.name === "delay")?.on ?? false;
  const compOn = fx.effects.find((e) => e.name === "comp")?.on ?? false;

  return (
    <section className={`live-fx${fx.bypass ? " live-fx-bypassed" : ""}`}>
      <div className="live-fx-top">
        <span className="live-fx-label">Effekte</span>
        <div className="live-fx-presets">
          {FX_PRESETS.map((p, n) => (
            <button
              key={p.name}
              className={`btn live-fx-preset${fx.preset === p.name ? " btn-on" : ""}`}
              data-midi={fxPresetAddress(track, p.name)}
              onClick={() => actions.fxPreset(track, p.name)}
              title={`${p.hint}. Taste V, dann ${n + 1}.`}
            >
              {p.label}
            </button>
          ))}
          {fx.preset === "custom" && (
            <span className="live-fx-custom" title="Seit dem letzten Preset wurde an einem Regler gedreht">
              eigen
            </span>
          )}
        </div>
        <button
          className={`btn live-fx-bypass${fx.bypass ? " live-fx-bypass-out" : " live-fx-bypass-in"}`}
          data-midi={fxAddress(track, "bypass")}
          onClick={() => actions.fxBypass(track, !fx.bypass)}
          title={
            fx.bypass
              ? "Die Kette ist umgangen: der Track wird bitgleich durchgereicht. Klick schaltet sie ein. Taste F."
              : "Die Kette ist im Signalweg. Klick nimmt sie heraus - bitgleich, das ist der Notschalter. Taste F."
          }
        >
          {fx.bypass ? "Kette aus" : "Kette an"} <kbd>F</kbd>
        </button>
      </div>

      <div className="live-fx-slots">
        {FX_SLOT_ORDER.map((name, n) => {
          const e = fx.effects.find((x) => x.name === name);
          if (!e) return null;
          return (
            <button
              key={name}
              className={`btn live-fx-slot${e.on ? " live-fx-slot-on" : ""}`}
              data-midi={fxSlotAddress(track, name)}
              onClick={() => actions.fxEnable(track, name, !e.on)}
              title={`${e.label} ist ${e.on ? "an" : "aus"}. Taste X, dann ${n + 1}.`}
            >
              <span className="live-fx-slot-letter mono">{e.on ? FX_LETTER[name] : "–"}</span>
              <span className="live-fx-slot-name">{e.label}</span>
            </button>
          );
        })}
      </div>

      <div className="live-fx-readout">
        <span
          className="live-fx-letters mono"
          title="Dieselbe Kurzform wie im Terminal: ein Buchstabe je eingeschaltetem Effekt, ein Strich für aus."
        >
          {fx.letters}
        </span>
        {delayOn && (
          <span
            className="live-fx-delay mono"
            title="Der Delay rechnet aus der Zeitachse der Engine, nicht in Millisekunden - ein Tempowechsel nimmt ihn mit."
          >
            {fx.delay_note_label} · {Math.round(fx.delay_ms)} ms
          </span>
        )}
        <CompMeter track={track} on={compOn} />
        <button
          className="btn btn-mini live-fx-more"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          title="Alle Parameter der fünf Effekte - beim Produzieren, nicht auf der Bühne"
        >
          Details {open ? "▲" : "▼"}
        </button>
      </div>

      {open && <FxDetails track={track} fx={fx} actions={actions} />}
    </section>
  );
}
