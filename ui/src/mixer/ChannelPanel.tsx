//! The detail panel of the selected channel, right of the arrangement while the mixer is open
//! (design/arrangement-mixer.html, `aside.insp`): its device column (`devices/DeviceChain`, the same
//! one the Live view's track detail shows), its layers, and its input side - level, latency and
//! trim. With a bus selected (its strip was clicked) it shows that bus's inserts instead.

import { useCallback, useMemo, type CSSProperties } from "react";
import { trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { HMeter } from "../live/HMeter";
import { STATE_ICON, STATE_WORD, T } from "../shell/terms";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import type { LooperStatus, LooperTrackStatus } from "../types";
import { channelActions } from "./actions";
import { LatencyRows, LayerList } from "./ChannelParts";
import { GRID_CHOICES } from "../live/MidiLoopRows";
import { DeviceChain } from "../devices/DeviceChain";
import { busOwner, trackOwner } from "../devices/model";
import { stripKind, trackColor } from "./Strip";
import { useMixer } from "./store";

interface Props {
  /** The score track the arrangement has selected: a channel of another name is not shown for it. */
  expect?: string;
  /** Opens the arrangement's "Track" tab, where the score's loop type, grid and transpose are edited. */
  onTrackTab?: () => void;
}

export function ChannelPanel({ expect, onTrackTab }: Props = {}) {
  const { selected, bus } = useMixer();
  const signature = useStatusSlice(structureSignature);
  const defaultLatency = useStatusSlice((s) => s.latency_frames);
  const sampleRate = useStatusSlice((s) => s.sample_rate);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const track = useMemo(() => {
    const all = currentStatus().tracks;
    // A track chosen in the arrangement is found by its name: positions need not agree.
    if (expect !== undefined) return all.find((t) => t.name === expect) ?? all[selected] ?? all[0];
    return all[selected] ?? all[0];
  }, [signature, selected, expect]);
  const i = track?.index ?? 0;
  const inPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks ?? [0], [i]);

  const chain = useMemo(
    () => (track?.fx ? { index: i, name: track.name, instrument: !!track.instrument, fx: track.fx, actions: channelActions } : undefined),
    [track, i],
  );

  if (bus) {
    const name = bus === "main" ? `${T.main} · Room` : `${T.monitorBus} · Phones`;
    return (
      <aside className="insp chan" aria-label={`${name} bus`}>
        <div className="insp-h">
          <div className="kicker">
            <span className="caps">Bus · {T.plugIns}</span>
          </div>
          <h2>
            <Icon name={bus === "main" ? "speaker" : "phones"} />
            {name}
          </h2>
          <div className="sub">Inserts where the bus leaves the machine, before its volume.</div>
        </div>
        <div className="chan-body">
          <DeviceChain owner={busOwner(bus)} size="dense" title={bus === "main" ? T.main : T.monitorBus} />
        </div>
      </aside>
    );
  }

  if (track && expect !== undefined && track.name !== expect) {
    return (
      <aside className="insp chan">
        <div className="insp-h">
          <span className="caps">Channel</span>
          <h2>{expect}</h2>
          <div className="sub">The engine has no channel named {expect} - Load the score to create it.</div>
        </div>
      </aside>
    );
  }

  if (!track) {
    return (
      <aside className="insp chan">
        <div className="insp-h">
          <span className="caps">Channel</span>
          <div className="sub">No engine running - start it to see its channels.</div>
        </div>
      </aside>
    );
  }

  const kind = stripKind(track);
  const recordingLayer = track.state === "overdub" || track.state === "recording" ? track.layers.length : 0;
  const ms = (frames: number) => (sampleRate > 0 ? (frames / sampleRate) * 1000 : 0);
  const measured = track.latency_frames - track.latency_trim;

  return (
    <aside className="insp chan" style={{ "--c": trackColor(i) } as CSSProperties} aria-label={`Channel ${i + 1}: ${track.name}`}>
      <div className="insp-h">
        <div className="kicker">
          <span className="caps">Channel {i + 1} · {T.devices}</span>
          <span className={`st ${kind}`}>
            <Icon name={STATE_ICON[kind]} />
            {STATE_WORD[kind]}
            {recordingLayer > 0 ? ` · ${T.layer} ${recordingLayer}` : ""}
          </span>
        </div>
        <h2>
          <i className="c" />
          {track.name}
        </h2>
        <div className="sub">
          {track.instrument ? T.instrument : `${T.input} ${track.input_channels.join("–")}`} · {track.channels === 2 ? T.stereo : T.mono} · {T.latency}{" "}
          {ms(measured).toFixed(1)} ms {track.instrument ? "automatic" : track.latency_inherited ? "default" : "measured"} + {ms(track.latency_trim).toFixed(1)} ms
        </div>
      </div>

      <div className="chan-body">
        {track.instrument && <LoopLine track={track} onTrackTab={onTrackTab} />}
        <DeviceChain owner={trackOwner(i)} track={chain} size="dense" />

        <LayerList track={track} actions={channelActions} />

        <div className="fxd">
          <span className="caps">{track.instrument ? T.instrument : "Input"} · {T.latency}</span>
          <div className="frow">
            <label>{T.input}</label>
            <div>
              <HMeter pick={inPeaks} channels={track.input_channels.length || 1} className="grow" />
            </div>
          </div>
          <LatencyRows track={track} actions={channelActions} defaultLatency={defaultLatency} />
          <div className="frow">
            <label />
            <div>
              <button
                className="btn s danger"
                data-midi={trackAddress(i, "clear")}
                disabled={track.state === "empty"}
                onClick={() => channelActions.clear(i)}
                title="Delete every layer of this track"
              >
                <Icon name="trash" />
                {T.clear} track
              </button>
            </div>
          </div>
        </div>
      </div>
    </aside>
  );
}

/**
 * What a take of this instrument track stores and the grid it plays on, as the engine has it now -
 * with a way to where it is changed (the arrangement's "Track" tab), so nobody looks for it here.
 */
function LoopLine({ track, onTrackTab }: { track: LooperTrackStatus; onTrackTab?: () => void }) {
  const midi = !!track.midi_loop;
  const grid = GRID_CHOICES.find((g) => g.value === (track.grid ?? "none"))?.label ?? track.grid ?? "";
  const transpose = track.transpose ?? 0;
  const text = `${T.loopKind}: ${midi ? T.loopMidi : T.loopAudio}${midi ? ` · grid ${grid.toLowerCase()}` : ""}${transpose ? ` · ${transpose > 0 ? "+" : ""}${transpose} st` : ""}`;
  if (!onTrackTab) return <div className="loopline">{text}</div>;
  return (
    <button className="loopline" onClick={onTrackTab} title="Loop type, note grid and transpose belong to the score - edit them in the Track tab">
      <span className="what">{text}</span>
      <span className="go">
        edit in Track <Icon name="arr-r" />
      </span>
    </button>
  );
}
