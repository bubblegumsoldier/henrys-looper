//! One track: what it is doing, how loud it is, six big buttons, its effect chain, and its layers.
//!
//! The card only re-renders when the structure signature changes (see `status.ts`) - a peak moving
//! does not reach it, the meters draw themselves.

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Meter } from "./Meter";
import { FxRow, type FxActions } from "./FxRow";
import type { LooperStatus, LooperTrackStatus } from "../types";

/** The transport, the two settings rows, the layers - and the effect chain, which brings its own. */
export interface TrackActions extends FxActions {
  record: (track: number) => void;
  overdub: (track: number) => void;
  stop: (track: number) => void;
  play: (track: number) => void;
  clear: (track: number) => void;
  monitor: (track: number, on: boolean) => void;
  pan: (track: number, pan: number) => void;
  latency: (track: number, measured: number | null, trim: number) => void;
  layerMute: (track: number, layer: number, muted: boolean) => void;
  layerRemove: (track: number, layer: number) => void;
  layerGain: (track: number, layer: number, gain: number) => void;
}

interface Props {
  track: LooperTrackStatus;
  /** True while the number keys and the letter keys act on this track. */
  selected: boolean;
  onSelect: (track: number) => void;
  actions: TrackActions;
  /** The engine's global compensation in frames - what a track without its own value follows. */
  defaultLatency: number;
}

/** A dragged slider would otherwise send a command per pixel. */
const GAIN_THROTTLE_MS = 60;

/** `Mitte`, `L 50`, `R 100` - the same wording the CLI prints. */
function panLabel(pan: number): string {
  const percent = Math.round(Math.abs(pan) * 100);
  if (percent === 0) return "Mitte";
  return `${pan < 0 ? "L" : "R"} ${percent}`;
}

/**
 * The panner. Mono tracks are placed with it, stereo tracks balanced; either way the centre passes
 * both sides at unity, which is why the label says "Mitte" and not "-3 dB".
 *
 * Throttled exactly like the layer gain: a drag would otherwise be one command per pixel.
 */
function PanRow({ track, pan, actions }: { track: number; pan: number; actions: TrackActions }) {
  const [local, setLocal] = useState(pan);
  const dragging = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pending = useRef<number | null>(null);

  useEffect(() => {
    if (!dragging.current) setLocal(pan);
  }, [pan]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    if (pending.current !== null) {
      actions.pan(track, pending.current);
      pending.current = null;
    }
  }, [actions, track]);

  const send = useCallback(
    (value: number) => {
      pending.current = value;
      if (timer.current !== undefined) return;
      timer.current = window.setTimeout(() => {
        timer.current = undefined;
        if (pending.current !== null) {
          actions.pan(track, pending.current);
          pending.current = null;
        }
      }, GAIN_THROTTLE_MS);
    },
    [actions, track],
  );

  return (
    <div className="live-track-pan">
      <span className="live-pan-label">Panorama</span>
      <input
        className="live-pan-slider"
        type="range"
        min={-1}
        max={1}
        step={0.05}
        value={local}
        onPointerDown={() => {
          dragging.current = true;
        }}
        onPointerUp={() => {
          flush();
          dragging.current = false;
        }}
        onBlur={flush}
        onChange={(e) => {
          const value = Number(e.target.value);
          setLocal(value);
          send(value);
        }}
        title="Wo dieser Track zwischen den Lautsprechern sitzt"
      />
      <button
        className="btn btn-mini"
        onClick={() => {
          setLocal(0);
          flush();
          actions.pan(track, 0);
        }}
        disabled={local === 0}
        title="Zurück in die Mitte"
      >
        Mitte
      </button>
      <span className="live-pan-value mono">{panLabel(local)}</span>
    </div>
  );
}

/**
 * What this track subtracts while recording, and where that number comes from.
 *
 * Two fields rather than one, mirroring the engine: `Latenz` is the measured way in (empty means
 * the global default, and the field says so instead of showing a number that looks like a
 * setting), `Zuschlag` is the manual part for what no measurement can reach - a plugin host's own
 * buffer. The engine keeps them apart so the next calibration overwrites the first and leaves the
 * second standing, and the two boxes here are that promise made visible.
 *
 * Text fields, not sliders: this is set once per interface and cabling, not dragged during a song.
 * The value is sent when the field loses focus or on Enter, so typing "1" of "1100" does not
 * shift the take by a thousand frames on the way.
 */
function LatencyRow({
  track,
  actions,
  defaultLatency,
}: {
  track: LooperTrackStatus;
  actions: TrackActions;
  defaultLatency: number;
}) {
  const i = track.index;
  const [base, setBase] = useState<string>(
    track.latency_measured === null ? "" : String(track.latency_measured),
  );
  const [trim, setTrim] = useState<string>(String(track.latency_trim));
  const editing = useRef(false);

  // Follow the engine while nobody is typing, so a value set elsewhere (or refused) shows up.
  useEffect(() => {
    if (editing.current) return;
    setBase(track.latency_measured === null ? "" : String(track.latency_measured));
    setTrim(String(track.latency_trim));
  }, [track.latency_measured, track.latency_trim]);

  const send = useCallback(
    (baseText: string, trimText: string) => {
      const measured = baseText.trim() === "" ? null : Math.max(0, Math.round(Number(baseText) || 0));
      const value = Math.round(Number(trimText) || 0);
      actions.latency(i, measured, value);
    },
    [actions, i],
  );

  const commit = () => {
    editing.current = false;
    send(base, trim);
  };

  return (
    <div className={`live-track-latency${track.latency_inherited ? " live-latency-inherited" : ""}`}>
      <span className="live-latency-label">Latenz</span>
      <label className="live-latency-field">
        <span>gemessen</span>
        <input
          type="number"
          min={0}
          step={1}
          value={base}
          placeholder={String(defaultLatency)}
          title={`Gemessener Weg dieses Eingangs in Frames. Leer heißt: die globale Vorgabe von ${defaultLatency} Frames.`}
          onFocus={() => {
            editing.current = true;
          }}
          onChange={(e) => setBase(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
      </label>
      <label className="live-latency-field">
        <span>Zuschlag</span>
        <input
          type="number"
          step={1}
          value={trim}
          title="Manueller Aufschlag in Frames für das, was keine Messung sieht — etwa der interne Puffer eines Plugin-Hosts. Bleibt beim Kalibrieren stehen."
          onFocus={() => {
            editing.current = true;
          }}
          onChange={(e) => setTrim(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
      </label>
      <span className="live-latency-value mono">
        {track.latency_frames} F · {track.latency_ms.toFixed(2)} ms
      </span>
      {track.latency_inherited ? (
        <span className="live-latency-origin" title="Dieser Track folgt der globalen Vorgabe">
          geerbt ({defaultLatency})
        </span>
      ) : (
        <button
          className="btn btn-mini"
          onClick={() => {
            editing.current = false;
            setBase("");
            send("", trim);
          }}
          title="Wieder der globalen Vorgabe folgen"
        >
          eigen ✕
        </button>
      )}
    </div>
  );
}

/**
 * The count-in, big enough to read with a guitar in your hands and a few metres of stage in
 * between. "scharf" alone never said *when*; this does.
 *
 * Whole bars while there is still time to walk over, and beats once the last bar is running -
 * that is the moment the number stops being information and becomes a count-in.
 */
function Countdown({ track }: { track: LooperTrackStatus }) {
  if (!track.pending_kind) return null;
  const bars = track.pending_bars;
  const beats = track.pending_beats;
  const lastBar = bars === 0;
  const value = lastBar ? beats : bars;
  const unit = lastBar ? (beats === 1 ? "Schlag" : "Schläge") : bars === 1 ? "Takt" : "Takte";

  return (
    <div className={`live-countdown${lastBar ? " live-countdown-now" : ""}`} aria-live="polite">
      <span className="live-countdown-what">{track.pending_label}</span>
      <span className="live-countdown-in">in</span>
      <span className="live-countdown-value mono">{value}</span>
      <span className="live-countdown-unit">{unit}</span>
      {!lastBar && beats > 0 && <span className="live-countdown-rest mono">+{beats}</span>}
    </div>
  );
}

function LayerRow({
  track,
  layer,
  number,
  muted,
  gain,
  actions,
}: {
  track: number;
  layer: number;
  number: number;
  muted: boolean;
  gain: number;
  actions: TrackActions;
}) {
  // The slider follows the finger; the engine gets at most one command per throttle window, plus
  // one final value when the user lets go.
  const [local, setLocal] = useState(gain);
  const dragging = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pending = useRef<number | null>(null);

  useEffect(() => {
    if (!dragging.current) setLocal(gain);
  }, [gain]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    if (pending.current !== null) {
      actions.layerGain(track, layer, pending.current);
      pending.current = null;
    }
  }, [actions, track, layer]);

  const send = useCallback(
    (value: number) => {
      pending.current = value;
      if (timer.current !== undefined) return;
      timer.current = window.setTimeout(() => {
        timer.current = undefined;
        if (pending.current !== null) {
          actions.layerGain(track, layer, pending.current);
          pending.current = null;
        }
      }, GAIN_THROTTLE_MS);
    },
    [actions, track, layer],
  );

  return (
    <li className={`layer${muted ? " layer-muted" : ""}`}>
      <span className="layer-number mono">{number}</span>
      <button
        className={`btn btn-mini${muted ? " btn-on" : ""}`}
        onClick={() => actions.layerMute(track, layer, !muted)}
        title={muted ? "Ebene wieder hörbar machen" : "Ebene stummschalten"}
      >
        {muted ? "stumm" : "hörbar"}
      </button>
      <input
        className="layer-gain"
        type="range"
        min={0}
        max={4}
        step={0.05}
        value={local}
        onPointerDown={() => {
          dragging.current = true;
        }}
        onPointerUp={() => {
          // Send the value the finger stopped on at once, otherwise the status would push the
          // previous gain back into the slider before the throttled command arrives.
          flush();
          dragging.current = false;
        }}
        onBlur={flush}
        onChange={(e) => {
          const value = Number(e.target.value);
          setLocal(value);
          send(value);
        }}
        title="Lautstärke dieser Ebene"
      />
      <span className="layer-gain-value mono">{local.toFixed(2)}</span>
      <button
        className="btn btn-mini btn-danger"
        onClick={() => actions.layerRemove(track, layer)}
        title="Diese Ebene entfernen"
      >
        ✕
      </button>
    </li>
  );
}

function TrackCardInner({ track, selected, onSelect, actions, defaultLatency }: Props) {
  const i = track.index;
  const stereo = track.channels >= 2;
  // One picker per bar. A stereo track shows its two input channels separately - a dead cable on
  // one side is otherwise invisible until the take is played back - and every track shows its two
  // bus channels, because the bus is stereo whatever the track records.
  const inLeft = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks?.[0] ?? 0, [i]);
  const inRight = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks?.[1] ?? 0, [i]);
  const outLeft = useCallback((s: LooperStatus) => s.tracks[i]?.output_peaks?.[0] ?? 0, [i]);
  const outRight = useCallback((s: LooperStatus) => s.tracks[i]?.output_peaks?.[1] ?? 0, [i]);
  const empty = track.state === "empty";

  return (
    <article
      className={`live-track state-${track.state}${selected ? " live-track-selected" : ""}`}
      onClick={() => onSelect(i)}
    >
      <header className="live-track-head">
        <span className="live-track-key mono" title="Diese Ziffer wählt den Track">
          {i + 1}
        </span>
        <h2 className="live-track-name">{track.name}</h2>
        <span
          className="live-track-input mono"
          title={
            stereo
              ? "Eingangskanäle am Interface; dieser Track nimmt stereo auf"
              : "Eingangskanal am Interface; dieser Track nimmt mono auf"
          }
        >
          In {track.input_channels.join("+")}
        </span>
        <span className={`live-track-channels channels-${track.channels_label}`}>
          {track.channels_label}
        </span>
        <span className={`live-state state-badge-${track.state}`}>{track.state_label}</span>
      </header>

      <Countdown track={track} />

      <div className="live-track-meters">
        {stereo ? (
          <>
            <Meter label="Ein L" kind="in" pick={inLeft} />
            <Meter label="Ein R" kind="in" pick={inRight} />
          </>
        ) : (
          <Meter label="Ein" kind="in" pick={inLeft} />
        )}
        <Meter label="Aus L" kind="out" pick={outLeft} />
        <Meter label="Aus R" kind="out" pick={outRight} />
      </div>

      <PanRow track={i} pan={track.pan} actions={actions} />
      <LatencyRow track={track} actions={actions} defaultLatency={defaultLatency} />

      <div className="live-track-buttons">
        <button className="btn btn-big btn-record" onClick={() => actions.record(i)} title="Taste R">
          Aufnahme <kbd>R</kbd>
        </button>
        <button
          className="btn btn-big btn-overdub"
          onClick={() => actions.overdub(i)}
          disabled={empty}
          title={empty ? "Erst eine Aufnahme, dann Overdub" : "Taste O"}
        >
          Overdub <kbd>O</kbd>
        </button>
        <button className="btn btn-big btn-play" onClick={() => actions.play(i)} disabled={empty} title="Taste P">
          Wiedergabe <kbd>P</kbd>
        </button>
        <button className="btn btn-big btn-stopp" onClick={() => actions.stop(i)} title="Taste S">
          Stopp <kbd>S</kbd>
        </button>
        <button
          className={`btn btn-big btn-monitor${track.monitor ? " btn-on" : ""}`}
          onClick={() => actions.monitor(i, !track.monitor)}
          title="Taste M"
        >
          Mithören <kbd>M</kbd>
        </button>
        <button className="btn btn-big btn-danger" onClick={() => actions.clear(i)} disabled={empty}>
          Leeren
        </button>
      </div>

      {track.fx && <FxRow track={i} fx={track.fx} actions={actions} />}

      <div className="live-track-layers">
        <div className="live-layers-head">
          <span>
            {track.layers.length} {track.layers.length === 1 ? "Ebene" : "Ebenen"}
          </span>
          {track.loop_seconds > 0 && <span className="muted mono">{track.loop_seconds.toFixed(2)} s</span>}
        </div>
        {track.layers.length > 0 && (
          <ul className="layer-list">
            {track.layers.map((l) => (
              <LayerRow
                key={l.index}
                track={i}
                layer={l.index}
                number={l.number}
                muted={l.muted}
                gain={l.gain}
                actions={actions}
              />
            ))}
          </ul>
        )}
      </div>
    </article>
  );
}

export const TrackCard = memo(TrackCardInner);
