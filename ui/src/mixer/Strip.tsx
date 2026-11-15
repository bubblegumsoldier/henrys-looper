//! One channel strip of the mixer, top to bottom as on a console:
//!
//! ```text
//!  DEVICES preset ▾ ⏻  the device column in signal order, one narrow row each: [instrument],
//!  ⏻ High-p. …         the five built-in effects, the plug-ins, "+" - a click on a name opens
//!  ⏻ Plugin  +         that device in the channel panel (devices/CompactChain)
//!  BUSES        Main Monitor   loop and monitored input, each to the room and the headphones;
//!  Loop         [ ]  [ ]       the ear switches input monitoring itself on and off
//!  Input        [ ]  [ ]
//!  pan · M · S
//!  scale | fader | meter       the channel fader (post chain and inserts, pre pan) and what the
//!                              strip sends, post fader
//!  −3.5        −9.2 dB         fader value, highest peak (click resets)
//!  ▌name In 1          ●       colour, name, input, state
//! ```
//!
//! A click on the strip's background selects the channel for the detail panel.

import { memo, useMemo, useState, type CSSProperties } from "react";
import { fxPresetAddress, sendAddress, trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { ENGINE_STATE_KIND, STATE_ICON, STATE_WORD, T, type StateKind } from "../shell/terms";
import type { BusId, LooperStatus, LooperTrackStatus, TrackSourceId } from "../types";
import { Fader, Knob, linear, type Curve } from "./controls";
import { FX_PRESETS, presetLabel, shownPreset } from "./fxModel";
import { CompactChain } from "../devices/CompactChain";
import { trackOwner } from "../devices/model";
import { Popover } from "./Popover";
import { PeakHold, VMeter } from "./meters";
import { MAX_TRACK_GAIN, SCALE_MARKS, faderText, gainToDb, gainToPos, panText, posToGain } from "./scale";
import type { ChannelActions } from "./actions";

export const FADER_CURVE: Curve = { toPos: gainToPos, fromPos: posToGain };
export const BUS_FADER_CURVE: Curve = { toPos: gainToPos, fromPos: (p) => Math.min(posToGain(p), 4) };
const PAN_CURVE = linear(-1, 1);

/** The colour of a track by position: `--trk-1` … `--trk-8`. */
export const trackColor = (index: number) => `var(--trk-${(index % 8) + 1})`;

/** What a strip shows as the state of its track: monitoring counts while nothing plays. */
export function stripKind(t: LooperTrackStatus): StateKind {
  const kind = ENGINE_STATE_KIND[t.state] ?? "stop";
  if ((kind === "empty" || kind === "ready") && t.monitor) return "hear";
  if (kind === "ready") return "stop";
  return kind;
}

const stateTitle = (t: LooperTrackStatus, kind: StateKind) =>
  `${STATE_WORD[kind]}${t.layers.length ? ` · ${t.layers.length} ${t.layers.length === 1 ? "layer" : "layers"}` : ""}`;

/** The dB scale beside a fader, positioned with the fader law. */
export function Scale() {
  return (
    <div className="scale" aria-hidden="true">
      {SCALE_MARKS.map((m) => (
        <span key={m.label} style={{ bottom: `calc(4px + (100% - 8px) * ${m.pos.toFixed(4)})` }}>
          {m.label}
        </span>
      ))}
    </div>
  );
}

/** The preset name in the strip's caption, opening a small menu of the three presets. */
function PresetMenu({ track, preset, actions }: { track: number; preset: LooperTrackStatus["fx"]["preset"]; actions: ChannelActions }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  return (
    <>
      <button
        className="presetbtn dim2"
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
        title="Effect preset of this channel"
        aria-expanded={anchor !== null}
      >
        {presetLabel(preset)}
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} className="menu">
          {FX_PRESETS.map((p) => (
            <button
              key={p.name}
              role="menuitemradio"
              aria-checked={preset === p.name}
              className={preset === p.name ? "on" : ""}
              data-midi={fxPresetAddress(track, p.name)}
              title={p.hint}
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

function SendCell({
  track,
  source,
  bus,
  on,
  actions,
}: {
  track: number;
  source: TrackSourceId;
  bus: BusId;
  on: boolean;
  actions: ChannelActions;
}) {
  return (
    <button
      className={`sw${on ? " on" : ""}${source === "monitor" ? " h" : ""}`}
      data-midi={sendAddress(track, source, bus)}
      onClick={() => actions.bus(track, source, bus, !on)}
      aria-pressed={on}
      title={`${source === "loop" ? "Loop playback" : "Monitored input"} → ${bus === "main" ? "Main (room)" : "Monitor (headphones)"}: ${on ? "on" : "off"}`}
    >
      <Icon name={bus === "main" ? "speaker" : "phones"} />
    </button>
  );
}

interface Props {
  track: LooperTrackStatus;
  selected: boolean;
  /** Some other track is soloed and this one is not: it is silent right now. */
  implicitMute: boolean;
  onSelect: (track: number) => void;
  actions: ChannelActions;
}

function StripImpl({ track: t, selected, implicitMute, onSelect, actions }: Props) {
  const i = t.index;
  const kind = stripKind(t);
  const fx = t.fx;
  const gain = t.gain ?? 1;
  const stereo = t.channels === 2;
  const chain = useMemo(
    () => (fx ? { index: i, name: t.name, instrument: !!t.instrument, fx, actions } : undefined),
    [fx, i, t.name, t.instrument, actions],
  );

  return (
    <div
      className={`strip${selected ? " sel" : ""}${t.muted || implicitMute ? " silent" : ""}`}
      style={{ "--c": trackColor(i) } as CSSProperties}
      onPointerDown={() => onSelect(i)}
      aria-label={`Channel ${i + 1}: ${t.name}`}
    >
      <div className="zone">
        <CompactChain
          owner={trackOwner(i)}
          track={chain}
          onSelect={() => onSelect(i)}
          caption={fx && <PresetMenu track={i} preset={shownPreset(fx)} actions={actions} />}
        />
      </div>

      <div className="zone">
        <span className="caps">
          Buses
          <button
            className={`mon${t.monitor ? " on" : ""}`}
            data-midi={trackAddress(i, "monitor")}
            onClick={() => actions.monitor(i, !t.monitor)}
            aria-pressed={t.monitor}
            title={t.monitor ? "Input monitoring on - click to switch off" : "Input monitoring off - click to hear the input"}
          >
            <Icon name="hear" />
            {t.monitor ? "on" : "off"}
          </button>
        </span>
        <div className="sends">
          <span />
          <span className="hd">{T.main}</span>
          <span className="hd">{T.monitorBus}</span>
          <span>{T.loop}</span>
          <SendCell track={i} source="loop" bus="main" on={t.bus_main} actions={actions} />
          <SendCell track={i} source="loop" bus="monitor" on={t.bus_monitor} actions={actions} />
          <span>Input</span>
          <SendCell track={i} source="monitor" bus="main" on={t.monitor_bus_main} actions={actions} />
          <SendCell track={i} source="monitor" bus="monitor" on={t.monitor_bus_monitor} actions={actions} />
        </div>
      </div>

      <div className="panrow">
        <Knob
          value={t.pan}
          def={0}
          curve={PAN_CURVE}
          format={panText}
          onChange={(v) => actions.pan(i, Math.round(v * 100) / 100)}
          address={trackAddress(i, "pan")}
          label={T.pan}
          bipolar
          show="side"
        />
        <span className="ms">
          <button
            className={`m${t.muted ? " on" : ""}`}
            data-midi={trackAddress(i, "mute")}
            onClick={() => actions.mute(i, !t.muted)}
            aria-pressed={t.muted}
            title={t.muted ? "Muted - click to unmute" : "Mute this channel (listening only)"}
          >
            M
          </button>
          <button
            className={`s${t.soloed ? " on" : ""}`}
            data-midi={trackAddress(i, "solo")}
            onClick={() => actions.solo(i, !t.soloed)}
            aria-pressed={t.soloed}
            title={t.soloed ? "Soloed - click to release" : "Solo: hear only soloed channels (never affects recording)"}
          >
            S
          </button>
        </span>
      </div>

      <div className="fz">
        <Scale />
        <div className="pair">
          <Fader
            value={gain}
            def={1}
            curve={FADER_CURVE}
            format={(g) => `${faderText(g)} dB`}
            onChange={(g) => actions.gain(i, Math.min(MAX_TRACK_GAIN, g))}
            address={trackAddress(i, "gain")}
            label={`${t.name} fader`}
            title="Channel fader: after the effect chain and the plugins, before the pan. Listening only."
          />
          <VMeter pick={(s: LooperStatus) => s.tracks[i]?.strip_peaks ?? s.tracks[i]?.output_peaks ?? [0, 0]} channels={2} wide={!stereo} />
        </div>
      </div>

      <div className="stripfoot">
        <div className="vol">
          <b title={`Fader ${Number.isFinite(gainToDb(gain)) ? gainToDb(gain).toFixed(1) : "−∞"} dB`}>{faderText(gain)}</b>
          <PeakHold pick={(s: LooperStatus) => s.tracks[i]?.strip_peak ?? s.tracks[i]?.output_peak ?? 0} />
        </div>
        <div className="nm">
          <span className="tname">{t.name}</span>
          <span className="in">{t.instrument ? T.midiIn : `${T.input} ${t.input_channels.join("–")}`}</span>
          <div className="grow" />
          <span className={`st ${kind}`} title={stateTitle(t, kind)}>
            <Icon name={STATE_ICON[kind]} />
          </span>
        </div>
      </div>
    </div>
  );
}

export const Strip = memo(StripImpl);
