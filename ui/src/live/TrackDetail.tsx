//! Everything about one track that does not fit on its Live tile: levels, the channel fader with
//! mute and solo, pan, bus sends, the effect chain and the plugins, the layers, latency and "clear".
//! A drawer over the right edge of the Live view, opened from the tile's "more" button or with the
//! key `I` for the selected track.
//!
//! The same controls live in the Arrangement mode's mixer and its channel panel; this drawer is
//! built from the same pieces (`devices/DeviceChain`, `mixer/ChannelParts`, `mixer/controls`), so a
//! control looks, behaves and is addressed by MIDI the same way in both modes.

import { useCallback, useEffect, useMemo } from "react";
import { HMeter } from "./HMeter";
import { sendAddress, trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { T, num } from "../shell/terms";
import type { BusId, LooperStatus, LooperTrackStatus, TrackSourceId } from "../types";
import type { ChannelActions } from "../mixer/actions";
import { Knob, linear } from "../mixer/controls";
import { DeviceChain } from "../devices/DeviceChain";
import { trackOwner } from "../devices/model";
import { LatencyRows, LayerList } from "../mixer/ChannelParts";
import { MidiLoopRows } from "./MidiLoopRows";
import { FADER_CURVE } from "../mixer/Strip";
import { MAX_TRACK_GAIN, faderText, panText } from "../mixer/scale";
import "../mixer/mixer.css";

/** The transport, the routing, the layers, the fader - and the effect chain. */
export interface TrackActions extends ChannelActions {
  record: (track: number) => void;
  overdub: (track: number) => void;
  /** Stop at the end of the bar - the stop button's big half, key S. */
  stop: (track: number) => void;
  /** Stop now, mid-bar - the small red half, Shift+S. */
  stopNow: (track: number) => void;
  play: (track: number) => void;
}

const PAN_CURVE = linear(-1, 1);

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

/** Fader, pan, mute and solo of the channel, as knobs - the drawer has no room for a fader. */
function ChannelRow({ track, actions }: { track: LooperTrackStatus; actions: TrackActions }) {
  const i = track.index;
  return (
    <div className="kn-row drawer-ch">
      <Knob
        small
        value={track.gain ?? 1}
        def={1}
        curve={FADER_CURVE}
        format={(g) => `${faderText(g)} dB`}
        onChange={(g) => actions.gain(i, Math.min(MAX_TRACK_GAIN, g))}
        address={trackAddress(i, "gain")}
        label="Fader"
        caption="Fader"
        title="Channel fader: after the effect chain and the plugins, before the pan. Listening only."
      />
      <Knob
        small
        bipolar
        value={track.pan}
        def={0}
        curve={PAN_CURVE}
        format={panText}
        onChange={(v) => actions.pan(i, Math.round(v * 100) / 100)}
        address={trackAddress(i, "pan")}
        label={T.pan}
        caption={T.pan}
      />
      <div className="kn">
        <button
          className={`btn s${track.muted ? " on mute-on" : ""}`}
          data-midi={trackAddress(i, "mute")}
          onClick={() => actions.mute(i, !track.muted)}
          aria-pressed={track.muted}
          title={track.muted ? "Muted - click to unmute" : "Mute this channel (listening only)"}
        >
          M
        </button>
        <label>Mute</label>
      </div>
      <div className="kn">
        <button
          className={`btn s${track.soloed ? " on solo-on" : ""}`}
          data-midi={trackAddress(i, "solo")}
          onClick={() => actions.solo(i, !track.soloed)}
          aria-pressed={track.soloed}
          title={track.soloed ? "Soloed - click to release" : "Solo: hear only soloed channels (never affects recording)"}
        >
          S
        </button>
        <label>Solo</label>
      </div>
    </div>
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
  const chain = useMemo(
    () => (track.fx ? { index: i, name: track.name, instrument: !!track.instrument, fx: track.fx, actions } : undefined),
    [track.fx, i, track.name, track.instrument, actions],
  );
  const inPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks ?? [0], [i]);
  const outPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.strip_peaks ?? s.tracks[i]?.output_peaks ?? [0, 0], [i]);

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
            Track {i + 1} · {track.instrument ? T.instrument : `${T.input} ${track.input_channels.join("–")}`} · {track.channels_label === "stereo" ? T.stereo : T.mono}
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
          <div className="caps">Channel</div>
          <ChannelRow track={track} actions={actions} />
          <SendRow track={i} status={track} actions={actions} />
          {track.instrument && <MidiLoopRows track={track} />}
        </section>

        <section className="insp-s drawer-fx">
          <DeviceChain owner={trackOwner(i)} track={chain} size="stage" />
        </section>

        <section className="insp-s drawer-fx">
          <LayerList track={track} actions={actions} />
        </section>

        <section className="insp-s">
          <div className="caps">{T.latency}</div>
          <LatencyRows track={track} actions={actions} defaultLatency={defaultLatency} />
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
