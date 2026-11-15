//! The parts of a channel that are not the effect chain: its layers and its latency compensation.
//! Shared by the mixer's detail panel and the Live view's track drawer.

import { useEffect, useRef, useState } from "react";
import { layerAddress, trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import type { LooperTrackStatus } from "../types";
import type { ChannelActions } from "./actions";
import { HBar, type Curve } from "./controls";
import { gainToPos, peakText, posToGain } from "./scale";

/** Layer volume: linear 0 to 4 on the fader law, so 0 dB sits where it sits on every fader. */
const LAYER_CURVE: Curve = { toPos: gainToPos, fromPos: (p) => Math.min(4, posToGain(p)) };

/** "0.0 dB", "−3.5 dB" - a layer gain as a musician reads it. */
const layerText = peakText;

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
  actions: ChannelActions;
}) {
  return (
    <div className={`layer${recording ? " rec" : ""}${muted ? " muted" : ""}`}>
      <span className="no">{number}</span>
      <HBar
        className="grow"
        value={gain}
        def={1}
        curve={LAYER_CURVE}
        format={layerText}
        onChange={(g) => actions.layerGain(track, layer, Math.round(g * 1000) / 1000)}
        address={layerAddress(track, layer, "gain")}
        label={`Layer ${number} volume`}
      />
      {recording ? (
        <span className="st dub">
          <Icon name="dub" />
          recording
        </span>
      ) : (
        <>
          <span className="num lv">{layerText(gain)}</span>
          <button
            className={`btn s sq${muted ? " on mut" : ""}`}
            data-midi={layerAddress(track, layer, "mute")}
            onClick={() => actions.layerMute(track, layer, !muted)}
            aria-pressed={muted}
            title={muted ? "Muted - click to unmute this layer" : "Mute this layer"}
          >
            <Icon name={muted ? "mute" : "speaker"} />
          </button>
          <button
            className="btn s sq ghost"
            data-midi={layerAddress(track, layer, "remove")}
            onClick={() => actions.layerRemove(track, layer)}
            title="Remove this layer"
          >
            <Icon name="x" />
          </button>
        </>
      )}
    </div>
  );
}

export function LayerList({ track, actions }: { track: LooperTrackStatus; actions: ChannelActions }) {
  const recordingLayer = track.state === "overdub" || track.state === "recording" ? track.layers.length - 1 : -1;
  return (
    <div className="fxd layers-s">
      <span className="caps">
        {T.layers} <span className="dim2 cnt">· {track.layers.length} of 16</span>
      </span>
      {track.layers.length === 0 ? (
        <div className="hint">No layers yet. Record starts the first one.</div>
      ) : (
        track.layers.map((l) => (
          <LayerRow
            key={l.index}
            track={track.index}
            layer={l.index}
            number={l.number}
            muted={l.muted}
            gain={l.gain}
            recording={l.index === recordingLayer}
            actions={actions}
          />
        ))
      )}
    </div>
  );
}

/**
 * What this track subtracts while recording. `Measured` is the loopback value of this input (empty
 * follows the engine default), `Trim` the manual part no measurement sees - a plugin host's buffer.
 * A calibration overwrites the first and keeps the second. Sent on blur or Enter, so typing "1" of
 * "1100" does not shift the take on the way.
 */
export function LatencyRows({
  track,
  actions,
  defaultLatency,
}: {
  track: LooperTrackStatus;
  actions: ChannelActions;
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
  const field = {
    onFocus: () => {
      editing.current = true;
    },
    onBlur: () => {
      editing.current = false;
      send(base, trim);
    },
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
