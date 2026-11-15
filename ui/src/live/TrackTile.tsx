//! One track on stage: what it is doing (big), what comes next, its levels, its layers as dots, and
//! the five buttons. Readable from a few metres; every button at least 48 px tall.
//!
//! Re-renders only when the track's structure signature changes (see `status.ts`); the meters draw
//! themselves.

import { memo, useCallback, type CSSProperties } from "react";
import { HMeter } from "./HMeter";
import { api } from "../api";
import { confirmAction } from "../dialogs";
import { run } from "../shell/notes";
import type { TrackActions } from "./TrackDetail";
import { trackAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { FX_LETTER, FX_NAME, fxOnList } from "../mixer/fxModel";
import {
  ENGINE_STATE_KIND,
  PENDING,
  SCORE_STATE,
  SCORE_STATE_KIND,
  STATE_ICON,
  STATE_WORD,
  T,
  bars,
  countdown,
  num,
  type StateKind,
} from "../shell/terms";
import type { LooperStatus, LooperTrackStatus, ScoreState } from "../types";

/** "Clear track", asked first: every layer of this track is gone afterwards. */
export function clearTrackAsked(track: number, name: string): void {
  void confirmAction(`Clear track "${name}" deletes every layer of it.`, T.clearTrack, T.clearTrack).then((ok) => {
    if (ok) run(() => api.trackClear(track));
  });
}

/** The identity colour of a track: --trk-1 … --trk-8, by position. */
export function trackColor(index: number): string {
  return `var(--trk-${(index % 8) + 1})`;
}

/** What the tile shows as the track's state - monitoring counts as a state only while nothing plays. */
export function trackKind(t: LooperTrackStatus): StateKind {
  const kind = ENGINE_STATE_KIND[t.state] ?? "stop";
  if ((kind === "empty" || kind === "ready") && t.monitor) return "hear";
  if (kind === "empty") return "stop";
  return kind;
}

function stateLine(t: LooperTrackStatus, kind: StateKind, barInLoop: number, loopBars: number, latchMode: boolean): string {
  const loop = t.loop_seconds > 0 ? `${T.loop} ${bars(loopBars)} · ${num(t.loop_seconds)} s` : T.noLoop;
  switch (kind) {
    case "rec":
      return `${T.bar} ${barInLoop} / ${loopBars} · ${t.channels_label === "stereo" ? T.stereo : T.mono}`;
    case "dub":
      // A continuous overdub that was switched off still finishes its pass - say so, or the
      // musician presses O again thinking it did not take.
      if (latchMode && !t.latched) return `${T.lastPass} · ${T.bar} ${barInLoop} / ${loopBars}`;
      return `${T.bar} ${barInLoop} / ${loopBars}`;
    case "armed":
      return t.pending_kind === "overdub" ? "overdub waits for the grid" : "record waits for the grid";
    case "hear":
      return t.loop_seconds > 0 ? `input live · ${loop}` : "input live · no loop";
    default:
      return loop;
  }
}

interface Props {
  track: LooperTrackStatus;
  selected: boolean;
  onSelect: (track: number) => void;
  onDetail: (track: number) => void;
  actions: TrackActions;
  /** What happens to this track next (the next section of the score), or null. */
  next: ScoreState | null;
  /** Bar inside the loop, for the recording/overdub sub line. */
  barInLoop: number;
  loopBars: number;
  /** O keeps overdubbing until pressed again (free looping, `overdub_mode: latch`). */
  latchMode: boolean;
}

function TrackTileInner({ track, selected, onSelect, onDetail, actions, next, barInLoop, loopBars, latchMode }: Props) {
  const i = track.index;
  const kind = trackKind(track);
  const pending = track.pending_kind ? PENDING[track.pending_kind] : null;
  const empty = track.state === "empty";
  const inPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.input_peaks ?? [0], [i]);
  const outPeaks = useCallback((s: LooperStatus) => s.tracks[i]?.output_peaks ?? [0, 0], [i]);

  // Layer dots: at least three places, one per layer beyond that; the one being written glows.
  const writing = track.state === "overdub" || track.state === "recording";
  const dotCount = Math.max(3, track.layers.length + (writing && track.state === "recording" ? 1 : 0));
  const dots = Array.from({ length: Math.min(dotCount, 8) }, (_, n) => {
    const layer = track.layers[n];
    if (writing && n === (track.state === "recording" ? 0 : track.layers.length - 1)) return "rec";
    if (layer) return layer.muted ? "muted" : "on";
    return "";
  });

  const tileClass =
    kind === "rec" ? " rec" : kind === "dub" ? " dub" : pending || kind === "armed" ? " armed" : "";

  // The countdown wins the "next" row: it is the more immediate future.
  let soll: { kind: StateKind; word: string; armed: boolean } | null = null;
  // "Overdub next" wins over the record's own countdown: it is what happens at the loop end.
  const overdubNext = track.latched && (track.state === "recording" || track.state === "armed");
  if (overdubNext) {
    soll = { kind: "dub", word: T.overdubNext, armed: true };
  } else if (pending && track.pending_kind) {
    const value = countdown(track.pending_bars, track.pending_beats);
    soll = { kind: pending, word: `${STATE_WORD[pending] === "Stopped" ? T.stop : STATE_WORD[pending]} in ${value}`, armed: true };
  } else if (next) {
    soll = { kind: SCORE_STATE_KIND[next], word: SCORE_STATE[next], armed: false };
  }

  return (
    <article
      className={`tile${tileClass}${selected ? " sel" : ""}`}
      style={{ "--c": trackColor(i) } as CSSProperties}
      onClick={() => onSelect(i)}
    >
      <div className="tn">
        <span className="key" title="This digit selects the track">
          {i + 1}
        </span>
        <b>{track.name}</b>
        <span>
          {track.instrument ? T.midiIn : `${T.input} ${track.input_channels.join("–")}`}
        </span>
        <FxLetters track={track} />
        <span className="lay" title={`${track.layers.length} ${track.layers.length === 1 ? "layer" : "layers"}`}>
          {dots.map((d, n) => (
            <i key={n} className={d} />
          ))}
        </span>
        <button
          className="more"
          data-midi={trackAddress(i, "clear")}
          disabled={empty && !writing}
          onClick={(e) => {
            e.stopPropagation();
            clearTrackAsked(i, track.name);
          }}
          title={`${T.clearTrack} · key C (asks first)`}
          aria-label={T.clearTrack}
        >
          <Icon name="trash" />
        </button>
        <button
          className="more"
          onClick={(e) => {
            e.stopPropagation();
            onDetail(i);
          }}
          title="Pan, sends, latency, effects, layers · key I"
        >
          <Icon name="more" />
        </button>
      </div>

      <div className={`is ${kind}`}>
        <Icon name={STATE_ICON[kind]} />
        <div className="w">
          <b>{kind === "dub" ? `${STATE_WORD[kind]} · ${T.layer.toLowerCase()} ${Math.max(1, track.layers.length)}` : STATE_WORD[kind]}</b>
          <span>{stateLine(track, kind, barInLoop, loopBars, latchMode)}</span>
        </div>
      </div>

      <div className={`soll${soll ? "" : " none"}`}>
        {soll ? (
          <>
            <span className="lbl">{T.next}</span>
            <Icon name="arr-r" />
            <span className={`st ${soll.armed ? "armed" : soll.kind}`}>
              <Icon name={soll.armed ? "armed" : STATE_ICON[soll.kind]} />
              {soll.word}
            </span>
          </>
        ) : (
          <span className="lbl dim2">&nbsp;</span>
        )}
      </div>

      <div className="tm">
        <span>{T.input}</span>
        <HMeter pick={inPeaks} channels={1} />
        <span>{T.output}</span>
        <HMeter pick={outPeaks} channels={1} />
      </div>

      <div className="tbtns">
        <button
          className={`tb rec${track.state === "recording" ? " on" : ""}${pending === "rec" ? " pend" : ""}`}
          data-midi={trackAddress(i, "record")}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.record(i);
          }}
          title={`${T.record} · key R`}
        >
          <Icon name="rec" />
          <span>{T.record}</span>
        </button>
        <button
          className={`tb dub${track.state === "overdub" && (!latchMode || track.latched) ? " on" : ""}${pending === "dub" || overdubNext ? " pend" : ""}`}
          aria-pressed={latchMode ? track.latched : undefined}
          data-midi={trackAddress(i, "overdub")}
          disabled={empty}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.overdub(i);
          }}
          title={
            empty
              ? "Record a loop first, then overdub"
              : latchMode
                ? `${T.overdub} · key O · stays on until pressed again`
                : `${T.overdub} · key O`
          }
        >
          <Icon name="dub" />
          <span>{T.overdub}</span>
        </button>
        <button
          className={`tb play${track.state === "playing" ? " on" : ""}${pending === "play" ? " pend" : ""}`}
          data-midi={trackAddress(i, "play")}
          disabled={empty}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.play(i);
          }}
          title={`${T.play} · key P`}
        >
          <Icon name="play" />
          <span>{T.play}</span>
        </button>
        <button
          className={`tb stop${pending === "stop" ? " pend" : ""}`}
          data-midi={trackAddress(i, "stop")}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.stop(i);
          }}
          title={`${T.stop} · key S`}
        >
          <Icon name="stop" />
          <span>{T.stop}</span>
        </button>
        <button
          className={`tb hear${track.monitor ? " on" : ""}`}
          data-midi={trackAddress(i, "monitor")}
          // An instrument track is heard whenever it is played; only its setup or the score's
          // `monitor: false` silences it.
          disabled={track.instrument}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.monitor(i, !track.monitor);
          }}
          title={
            track.instrument
              ? track.monitor
                ? "Instrument track: heard whenever it is played. Mute it to silence it."
                : "Instrument track set up as never heard (monitor: false)"
              : `${T.monitor} · key M`
          }
        >
          <Icon name="hear" />
          <span>{T.monitor}</span>
        </button>
        <button
          className="tb hear undo"
          data-midi={trackAddress(i, "undo")}
          disabled={empty && !writing}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.undo(i);
          }}
          title={`${T.undo} · key U: the running take, or else the newest layer. Z or Backspace undoes the last take of any track.`}
        >
          <Icon name="arr-l" />
          <span>{T.undo}</span>
        </button>
        <button
          className={`tb hear mute${track.muted ? " on" : ""}`}
          data-midi={trackAddress(i, "mute")}
          aria-pressed={track.muted}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(i);
            actions.mute(i, !track.muted);
          }}
          title={`${T.mute} · key Q · silent at once, the loop keeps running in time`}
        >
          <Icon name="mute" />
          <span>{track.muted ? T.muted : T.mute}</span>
        </button>
      </div>
    </article>
  );
}

/** The built-in effects that are on, as their letters; struck through while the chain is bypassed. */
function FxLetters({ track }: { track: LooperTrackStatus }) {
  const on = track.fx ? fxOnList(track.fx) : [];
  if (!track.fx || on.length === 0) return null;
  const bypassed = track.fx.bypass;
  const names = on.map((n) => FX_NAME[n]).join(", ");
  return (
    <span
      className={`fxs${bypassed ? " byp" : ""}`}
      title={bypassed ? `${T.fxBypassedShort}: ${names} · key I` : `Effects on: ${names} · key I`}
    >
      {on.map((n) => FX_LETTER[n]).join("")}
    </span>
  );
}

export const TrackTile = memo(TrackTileInner);
