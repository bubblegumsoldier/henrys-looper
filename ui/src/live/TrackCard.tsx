//! One track: what it is doing, how loud it is, six big buttons, and its layers.
//!
//! The card only re-renders when the structure signature changes (see `status.ts`) - a peak moving
//! does not reach it, the meters draw themselves.

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Meter } from "./Meter";
import type { LooperStatus, LooperTrackStatus } from "../types";

export interface TrackActions {
  record: (track: number) => void;
  overdub: (track: number) => void;
  stop: (track: number) => void;
  play: (track: number) => void;
  clear: (track: number) => void;
  monitor: (track: number, on: boolean) => void;
  pan: (track: number, pan: number) => void;
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

function TrackCardInner({ track, selected, onSelect, actions }: Props) {
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
