//! The detail panel of the selected channel, right of the arrangement while the mixer is open
//! (design/arrangement-mixer.html, `aside.insp`): its effect chain with every parameter, its layers,
//! and its input side - level, latency and trim. Together with the strip this is everything the
//! old track card and the Live drawer could do.

import { useCallback, useMemo, type CSSProperties } from "react";
import { trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { HMeter } from "../live/HMeter";
import { STATE_ICON, STATE_WORD, T } from "../shell/terms";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import type { LooperStatus } from "../types";
import { channelActions } from "./actions";
import { LatencyRows, LayerList } from "./ChannelParts";
import { FxEditor } from "./FxEditor";
import { stripKind, trackColor } from "./Strip";
import { useMixer } from "./store";

export function ChannelPanel() {
  const { selected } = useMixer();
  const signature = useStatusSlice(structureSignature);
  const defaultLatency = useStatusSlice((s) => s.latency_frames);
  const sampleRate = useStatusSlice((s) => s.sample_rate);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const track = useMemo(() => currentStatus().tracks[selected] ?? currentStatus().tracks[0], [signature, selected]);
  const i = track?.index ?? 0;
  const inPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks ?? [0], [i]);

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
          <span className="caps">Channel {i + 1} · Effect chain</span>
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
          {ms(measured).toFixed(1)} ms {track.latency_inherited ? "default" : "measured"} + {ms(track.latency_trim).toFixed(1)} ms
        </div>
      </div>

      <div className="chan-body">
        {track.fx && <FxEditor track={i} fx={track.fx} actions={channelActions} />}

        <LayerList track={track} actions={channelActions} />

        <div className="fxd">
          <span className="caps">Input · {T.latency}</span>
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
