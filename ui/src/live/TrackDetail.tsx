//! Everything about one track that does not fit on its Live tile: pan, bus sends, latency, the
//! effect chain, the layer list and "clear". A drawer over the right edge of the Live view, opened
//! from the tile's "more" button or with the key `I` for the selected track.
//!
//! These controls move to the Arrangement mixer and inspector in the next stage; until then this
//! drawer is where they live, so nothing the old track card could do is lost. Every control keeps
//! its `data-midi` address.

import { useCallback, useEffect, useRef, useState } from "react";
import { FxRow, type FxActions } from "./FxRow";
import { PluginSlots } from "../plugins/PluginSlots";
import { HMeter } from "./HMeter";
import { layerAddress, sendAddress, trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { T, num } from "../shell/terms";
import type { BusId, LooperStatus, LooperTrackStatus, TrackSourceId } from "../types";

/** The transport, the routing, the layers - and the effect chain, which brings its own. */
export interface TrackActions extends FxActions {
  record: (track: number) => void;
  overdub: (track: number) => void;
  stop: (track: number) => void;
  play: (track: number) => void;
  clear: (track: number) => void;
  monitor: (track: number, on: boolean) => void;
  bus: (track: number, source: TrackSourceId, bus: BusId, on: boolean) => void;
  pan: (track: number, pan: number) => void;
  latency: (track: number, measured: number | null, trim: number) => void;
  layerMute: (track: number, layer: number, muted: boolean) => void;
  layerRemove: (track: number, layer: number) => void;
  layerGain: (track: number, layer: number, gain: number) => void;
}

/** A dragged slider would otherwise send a command per pixel. */
const THROTTLE_MS = 60;

/** Slider state that follows the finger and sends at most one value per throttle window. */
function useThrottled(value: number, send: (v: number) => void) {
  const [local, setLocal] = useState(value);
  const dragging = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pending = useRef<number | null>(null);
  const out = useRef(send);
  out.current = send;

  useEffect(() => {
    if (!dragging.current) setLocal(value);
  }, [value]);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flush = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = undefined;
    if (pending.current !== null) {
      out.current(pending.current);
      pending.current = null;
    }
  }, []);

  const push = useCallback((v: number) => {
    setLocal(v);
    pending.current = v;
    if (timer.current !== undefined) return;
    timer.current = window.setTimeout(() => {
      timer.current = undefined;
      if (pending.current !== null) {
        out.current(pending.current);
        pending.current = null;
      }
    }, THROTTLE_MS);
  }, []);

  const bind = {
    onPointerDown: () => {
      dragging.current = true;
    },
    onPointerUp: () => {
      flush();
      dragging.current = false;
    },
    onBlur: flush,
  };
  return { local, setLocal, push, flush, bind };
}

/** `C`, `L 50`, `R 100`. */
function panLabel(pan: number): string {
  const percent = Math.round(Math.abs(pan) * 100);
  if (percent === 0) return "C";
  return `${pan < 0 ? "L" : "R"} ${percent}`;
}

function PanRow({ track, pan, actions }: { track: number; pan: number; actions: TrackActions }) {
  const s = useThrottled(pan, (v) => actions.pan(track, v));
  return (
    <div className="frow" data-midi={trackAddress(track, "pan")}>
      <label>{T.pan}</label>
      <div>
        <input
          className="range"
          type="range"
          min={-1}
          max={1}
          step={0.05}
          value={s.local}
          {...s.bind}
          onChange={(e) => s.push(Number(e.target.value))}
          title="Where this track sits between the speakers"
        />
        <span className="val mono">{panLabel(s.local)}</span>
        <button
          className="btn s"
          disabled={s.local === 0}
          onClick={() => {
            s.setLocal(0);
            s.flush();
            actions.pan(track, 0);
          }}
          title="Back to the centre"
        >
          Center
        </button>
      </div>
    </div>
  );
}

/**
 * Which output buses this track goes to: the loop and the monitored input, each to Main (the room)
 * and Monitor (the headphones), as four switches. Deliberately not linked - on a stage whose PA
 * already has the microphone the answers are "both" for the loop and "headphones only" for the
 * input.
 */
function SendRow({ track, status, actions }: { track: number; status: LooperTrackStatus; actions: TrackActions }) {
  const cell = (source: TrackSourceId, bus: BusId, on: boolean) => (
    <button
      className={`chip bus${on ? " on" : ""}${source === "monitor" ? " hear" : ""}`}
      data-midi={sendAddress(track, source, bus)}
      onClick={() => actions.bus(track, source, bus, !on)}
      title={`${source === "loop" ? "Loop playback" : "Monitored input"} of this track on ${bus === "main" ? "Main (room)" : "Monitor (headphones)"}`}
    >
      <Icon name={bus === "main" ? "speaker" : "phones"} />
      {bus === "main" ? T.main : T.monitorBus}
    </button>
  );
  return (
    <>
      <div className="frow">
        <label>Loop to</label>
        <div>
          {cell("loop", "main", status.bus_main)}
          {cell("loop", "monitor", status.bus_monitor)}
        </div>
      </div>
      <div className="frow">
        <label>Monitor to</label>
        <div>
          {cell("monitor", "main", status.monitor_bus_main)}
          {cell("monitor", "monitor", status.monitor_bus_monitor)}
        </div>
      </div>
    </>
  );
}

/**
 * What this track subtracts while recording. `Measured` is the loopback value of this input (empty
 * follows the engine default), `Trim` the manual part no measurement sees - a plugin host's buffer.
 * A calibration overwrites the first and keeps the second. Sent on blur or Enter, so typing "1" of
 * "1100" does not shift the take on the way.
 */
function LatencyRows({
  track,
  actions,
  defaultLatency,
}: {
  track: LooperTrackStatus;
  actions: TrackActions;
  defaultLatency: number;
}) {
  const i = track.index;
  const [base, setBase] = useState(track.latency_measured === null ? "" : String(track.latency_measured));
  const [trim, setTrim] = useState(String(track.latency_trim));
  const editing = useRef(false);

  useEffect(() => {
    if (editing.current) return;
    setBase(track.latency_measured === null ? "" : String(track.latency_measured));
    setTrim(String(track.latency_trim));
  }, [track.latency_measured, track.latency_trim]);

  const send = (b: string, t: string) => {
    const measured = b.trim() === "" ? null : Math.max(0, Math.round(Number(b) || 0));
    actions.latency(i, measured, Math.round(Number(t) || 0));
  };
  const commit = () => {
    editing.current = false;
    send(base, trim);
  };
  const field = {
    onFocus: () => {
      editing.current = true;
    },
    onBlur: commit,
    onKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") e.currentTarget.blur();
    },
  };

  return (
    <>
      <div className="frow">
        <label>Measured</label>
        <div>
          <input
            className="field num-in"
            type="number"
            min={0}
            step={1}
            value={base}
            placeholder={String(defaultLatency)}
            onChange={(e) => setBase(e.target.value)}
            title={`Measured path of this input in frames. Empty means the global default of ${defaultLatency} frames.`}
            {...field}
          />
          <span className="unit dim">frames</span>
          {track.latency_inherited ? (
            <span className="inh-tag" title="This track follows the global default">
              <Icon name="link" /> default
            </span>
          ) : (
            <button
              className="btn s"
              onClick={() => {
                editing.current = false;
                setBase("");
                send("", trim);
              }}
              title="Follow the global default again"
            >
              Use default
            </button>
          )}
        </div>
      </div>
      <div className="frow" data-midi={trackAddress(i, "latency_trim")}>
        <label>{T.trim}</label>
        <div>
          <input
            className="field num-in"
            type="number"
            step={1}
            value={trim}
            onChange={(e) => setTrim(e.target.value)}
            title="Manual surcharge in frames for what no measurement sees - e.g. a plugin host's internal buffer. Survives calibration."
            {...field}
          />
          <span className="unit dim">frames</span>
        </div>
      </div>
      <div className="frow">
        <label>Total</label>
        <div className="mono">
          {track.latency_frames} frames · {track.latency_ms.toFixed(2)} ms
        </div>
      </div>
    </>
  );
}

function LayerRow({
  track,
  layer,
  number,
  muted,
  gain,
  recording,
  actions,
}: {
  track: number;
  layer: number;
  number: number;
  muted: boolean;
  gain: number;
  recording: boolean;
  actions: TrackActions;
}) {
  const s = useThrottled(gain, (v) => actions.layerGain(track, layer, v));
  return (
    // The row stands for the gain (a range input cannot carry the badge); mute and remove inside
    // carry their own addresses and win through `closest()`.
    <li className={`layer${recording ? " rec" : ""}${muted ? " muted" : ""}`} data-midi={layerAddress(track, layer, "gain")}>
      <span className="no">{number}</span>
      <button
        className={`btn s${muted ? " on" : ""}`}
        data-midi={layerAddress(track, layer, "mute")}
        onClick={() => actions.layerMute(track, layer, !muted)}
        title={muted ? "Unmute this layer" : "Mute this layer"}
      >
        <Icon name={muted ? "mute" : "speaker"} />
        {muted ? "Muted" : "On"}
      </button>
      <input
        className="range"
        type="range"
        min={0}
        max={4}
        step={0.05}
        value={s.local}
        {...s.bind}
        onChange={(e) => s.push(Number(e.target.value))}
        title="Layer volume"
      />
      <span className="val mono">{s.local.toFixed(2)}</span>
      <button
        className="btn s sq danger"
        data-midi={layerAddress(track, layer, "remove")}
        onClick={() => actions.layerRemove(track, layer)}
        title="Remove this layer"
      >
        <Icon name="x" />
      </button>
    </li>
  );
}

interface Props {
  track: LooperTrackStatus;
  color: string;
  actions: TrackActions;
  defaultLatency: number;
  onClose: () => void;
}

export function TrackDetail({ track, color, actions, defaultLatency, onClose }: Props) {
  const i = track.index;
  const empty = track.state === "empty";
  const inPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks ?? [0], [i]);
  const outPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.output_peaks ?? [0, 0], [i]);
  const recordingLayer = track.state === "overdub" || track.state === "recording" ? track.layers.length - 1 : -1;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <aside className="drawer" style={{ "--c": color } as React.CSSProperties} aria-label={`Track ${track.name}`}>
      <div className="insp-h">
        <div className="kicker">
          <span className="caps">
            Track {i + 1} · {T.input} {track.input_channels.join("–")} · {track.channels_label === "stereo" ? T.stereo : T.mono}
          </span>
          <button className="btn sq ghost" onClick={onClose} title="Close · Esc">
            <Icon name="x" />
          </button>
        </div>
        <h2>
          <i className="swatch" />
          {track.name}
        </h2>
        <div className="sub">
          {track.loop_seconds > 0 ? `${T.loop} ${num(track.loop_seconds, 2)} s` : T.noLoop} ·{" "}
          {track.layers.length} {track.layers.length === 1 ? "layer" : "layers"}
        </div>
      </div>

      <div className="drawer-body">
        <section className="insp-s">
          <div className="caps">Levels</div>
          <div className="frow">
            <label>{T.input}</label>
            <div>
              <HMeter pick={inPeaks} channels={track.input_channels.length || 1} className="grow" />
            </div>
          </div>
          <div className="frow">
            <label>{T.output}</label>
            <div>
              <HMeter pick={outPeaks} channels={2} className="grow" />
            </div>
          </div>
        </section>

        <section className="insp-s">
          <div className="caps">Routing</div>
          <PanRow track={i} pan={track.pan} actions={actions} />
          <SendRow track={i} status={track} actions={actions} />
        </section>

        <section className="insp-s">
          <div className="caps">{T.latency}</div>
          <LatencyRows track={track} actions={actions} defaultLatency={defaultLatency} />
        </section>

        {track.fx && (
          <section className="insp-s">
            <div className="legacy legacy-fx">
              <FxRow track={i} fx={track.fx} actions={actions} />
            </div>
          </section>
        )}

        <section className="insp-s">
          <PluginSlots target={`track.${i + 1}`} />
        </section>

        <section className="insp-s">
          <div className="caps">
            {T.layers} <span className="dim2">{track.layers.length}</span>
          </div>
          {track.layers.length === 0 ? (
            <div className="hint">No layers yet. Record starts the first one.</div>
          ) : (
            <ul className="layers">
              {track.layers.map((l) => (
                <LayerRow
                  key={l.index}
                  track={i}
                  layer={l.index}
                  number={l.number}
                  muted={l.muted}
                  gain={l.gain}
                  recording={l.index === recordingLayer}
                  actions={actions}
                />
              ))}
            </ul>
          )}
        </section>
      </div>

      <div className="insp-f">
        <button
          className="btn danger"
          data-midi={trackAddress(i, "clear")}
          disabled={empty}
          onClick={() => actions.clear(i)}
          title="Delete every layer of this track"
        >
          <Icon name="trash" />
          {T.clear} track
        </button>
      </div>
    </aside>
  );
}
