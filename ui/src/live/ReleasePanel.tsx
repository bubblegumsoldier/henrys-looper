//! The one control that matters on stage, and the two big buttons under it.
//!
//! With a score, the whole surface **is** the Release button (`transport.next`): ready to start,
//! running and waiting for Release, a switch armed with its countdown, the end. Without a score the
//! same place shows the free looper's next event - a take armed on some track, with its countdown -
//! and the buttons become Start/Stop of the clock (`transport.toggle`, Space), the grid and the
//! click.

import { ClockBar } from "./ClockBar";
import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { FREE_TRANSPORT, GLOBAL, TRANSPORT, quantizeAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { run } from "../shell/notes";
import {
  freeTransport,
  scoreCanStart,
  scoreNext,
  scoreRewind,
  scoreRuns,
  scoreStart,
  scoreStopAll,
  type ScoreView,
} from "../shell/scoreTransport";
import { STATE_WORD, T, bars, countdown, num, PENDING } from "../shell/terms";
import type { CompiledScore, LooperTrackStatus } from "../types";
import type { Clock } from "./PositionPanel";


function Countdown({ barsLeft }: { barsLeft: number }) {
  const n = Math.max(1, Math.min(barsLeft + 1, 8));
  return (
    <div className="countdown">
      {Array.from({ length: n }, (_, i) => (
        <i key={i} className={i === 0 ? "cur" : ""} />
      ))}
    </div>
  );
}

/** Minutes:seconds since the score started running, ticking on its own. */
function Since({ from }: { from: number | null }) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (from === null) return;
    const id = window.setInterval(() => tick((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [from]);
  if (from === null) return null;
  const s = Math.max(0, Math.floor((Date.now() - from) / 1000));
  return (
    <span className="sub">
      {T.since} {Math.floor(s / 60)}:{String(s % 60).padStart(2, "0")}
    </span>
  );
}

interface Props {
  score: ScoreView;
  compiled: CompiledScore | null;
  clock: Clock;
  tracks: LooperTrackStatus[];
}

export function ReleasePanel({ score, compiled, clock, tracks }: Props) {
  // When the run began, for "Running since 1:13". Kept here: the runner does not report it.
  const startedAt = useRef<number | null>(null);
  const runs = scoreRuns(score);
  if (runs && startedAt.current === null) startedAt.current = Date.now();
  if (!runs) startedAt.current = null;

  if (!score.loaded) return <FreePanel clock={clock} tracks={tracks} />;

  const armed = score.armed;
  const section = compiled && score.section !== null ? compiled.sections[score.section] : null;
  const nextIndex = armed?.section ?? (score.section !== null ? score.section + 1 : 0);
  const nextSection = compiled?.sections[nextIndex] ?? null;

  let cls = "release";
  let state: React.ReactNode;
  let to: React.ReactNode = null;
  let when: React.ReactNode = null;
  let meter: React.ReactNode = null;
  const over = score.phase === "finished" || score.phase === "stopped";
  const enabled = runs || over;

  if (score.phase === "idle") {
    state = (
      <>
        <Icon name="start" />
        Ready to start
      </>
    );
    to = (
      <>
        <span>with</span>
        {compiled?.sections[0]?.id ?? "section 1"}
      </>
    );
    when = (
      <>
        Press <b>{T.start}</b> or Space
      </>
    );
  } else if (score.phase === "finished" || score.phase === "stopped") {
    cls += " done";
    state = (
      <>
        <Icon name={score.phase === "finished" ? "check" : "stop"} />
        {score.phase === "finished" ? "Score finished" : "Stopped"}
      </>
    );
    to = <span className="muted-to">{score.title}</span>;
    when = (
      <>
        Press for <b>Back to start</b> - clears every track
      </>
    );
  } else if (armed) {
    cls += " armed";
    state = (
      <>
        <Icon name="armed" />
        {armed.countIn ? T.countIn : T.switchArmed}
      </>
    );
    to = (
      <>
        <span>to</span>
        {armed.section === null ? T.end : armed.label}
      </>
    );
    const grid = armed.countIn
      ? "count-in"
      : section?.autorelease
        ? "at section end"
        : section?.quantize === "bar"
          ? T.atNextBar
          : T.atLoopEnd;
    when = (
      <>
        {grid} · in <b>{countdown(armed.bars, armed.beats)}</b>
      </>
    );
    meter = <Countdown barsLeft={armed.bars} />;
  } else {
    cls += " waiting";
    state = (
      <>
        <Icon name="loop" />
        {section && !section.autorelease ? T.loopsUntilRelease : "Waiting for Release"}
      </>
    );
    to = (
      <>
        <span>next</span>
        {nextSection?.id ?? (score.section !== null && score.section + 1 >= score.sectionCount ? T.end : "next section")}
      </>
    );
    when = (
      <>
        Press <b>{T.release}</b>, Space or the pedal
      </>
    );
  }

  const releaseTitle =
    score.phase === "idle"
      ? `${T.start} · Space`
      : over
        ? "Back to start: clears all tracks and returns to the first section. Asks first."
        : `${T.release}: arm the switch to the next section · Space or N`;

  return (
    <div className="ctlcol">
      <button
        className={cls}
        data-midi={score.phase === "idle" ? TRANSPORT.start : over ? undefined : TRANSPORT.next}
        disabled={score.phase === "idle" ? !scoreCanStart(score) : !enabled}
        onClick={score.phase === "idle" ? scoreStart : over ? scoreRewind : scoreNext}
        title={releaseTitle}
      >
        <div className="rh">
          <span className="lbl">
            <Icon name="release" />
            {T.release}
          </span>
          <div className="grow" />
          {section && score.phase === "running" && (
            <span className="rq">
              <Icon name={section.autorelease ? "auto" : "hand"} />
              {section.autorelease ? "auto" : section.quantize === "bar" ? T.atNextBar : T.atLoopEnd}
            </span>
          )}
        </div>
        <div className="state">{state}</div>
        <div className="to">{to}</div>
        <div className="when">{when}</div>
        {meter ?? <div className="countdown placeholder" />}
      </button>
      <div className="lv-row">
        <button
          className={`bigbtn${runs ? " running" : scoreCanStart(score) ? " ready" : ""}`}
          data-midi={TRANSPORT.start}
          disabled={!scoreCanStart(score)}
          onClick={scoreStart}
          title={`${T.start} the score · Space`}
        >
          <Icon name="start" />
          {runs ? T.running : T.start}
          {runs && <Since from={startedAt.current} />}
        </button>
        <button
          className="bigbtn"
          data-midi={TRANSPORT.stopAll}
          disabled={!runs}
          onClick={scoreStopAll}
          title={`${T.stopAll}: every track stops and the run ends · Shift+S`}
        >
          <Icon name="stopall" />
          {T.stopAll}
        </button>
      </div>
    </div>
  );
}

/** Without a score: the next thing the looper will do by itself, plus grid and click. */
function FreePanel({ clock, tracks }: { clock: Clock; tracks: LooperTrackStatus[] }) {
  const waiting = tracks
    .filter((t) => t.pending_kind)
    .sort((a, b) => a.pending_bars * 64 + a.pending_beats - (b.pending_bars * 64 + b.pending_beats));
  const first = waiting[0];
  const other = clock.quantize === "loop" ? "bar" : "loop";

  return (
    <div className="ctlcol">
      <section className={`release${first && clock.transport ? " armed" : " free"}`}>
        <div className="rh">
          <span className="lbl">
            <Icon name="loop" />
            {T.loop}
          </span>
        </div>
        {!clock.transport ? (
          <>
            <div className="state">
              <Icon name="stop" />
              {T.transportStopped}
            </div>
            <div className="to">
              <span>at</span>
              {T.bar.toLowerCase()} 1
            </div>
            <div className="when">
              Press <b>{T.transportStart}</b> or Space · <b>R</b> records from bar 1
            </div>
            <div className="countdown placeholder" />
          </>
        ) : first ? (
          <>
            <div className="state">
              <Icon name="armed" />
              {STATE_WORD[PENDING[first.pending_kind!]] === "Stopped" ? T.stop : STATE_WORD[PENDING[first.pending_kind!]]} armed
            </div>
            <div className="to">
              <span>on</span>
              {first.name}
              {waiting.length > 1 && <span className="plus">+{waiting.length - 1}</span>}
            </div>
            <div className="when">
              {clock.quantize === "loop" ? "at loop start" : "at next bar"} · in{" "}
              <b>{countdown(first.pending_bars, first.pending_beats)}</b>
            </div>
            <Countdown barsLeft={first.pending_bars} />
          </>
        ) : (
          <>
            <div className="state">
              <Icon name="loop" />
              Free looping
            </div>
            <div className="to">
              <span>loop</span>
              {bars(clock.bars)}
            </div>
            <div className="when">
              {num(clock.loopSeconds, 2)} s · keys <b>1–{Math.max(1, tracks.length)}</b> select, <b>R O P S M</b> act
            </div>
            <div className="countdown placeholder" />
          </>
        )}
      </section>
      <ClockBar clock={clock} free />
      <div className="lv-row three">
        <button
          className={`bigbtn transport${clock.transport ? " running" : ""}`}
          data-midi={FREE_TRANSPORT}
          onClick={() => freeTransport(!clock.transport)}
          title={
            clock.transport
              ? "Stop the clock · Space. The loops are kept and fall silent; a recording that has not reached its end is dropped, an overdub ends here."
              : "Start the clock at bar 1 · Space. Every track on play starts from the top of its loop."
          }
        >
          <Icon name={clock.transport ? "stop" : "start"} />
          {clock.transport ? T.transportStop : T.transportStart}
        </button>
        <button
          className="bigbtn"
          data-midi={quantizeAddress(other)}
          onClick={() => run(() => api.setQuantize(other))}
          title={
            clock.quantize === "loop"
              ? "Takes start at the next loop start. Click to start at the next bar instead."
              : "Takes start at the next bar. Click to start at the next loop start instead."
          }
        >
          <Icon name={clock.quantize === "loop" ? "loop" : "bar"} />
          {clock.quantize === "loop" ? T.quantizeLoop : T.quantizeBar}
        </button>
        <button
          className={`bigbtn${clock.click ? " on" : ""}`}
          data-midi={GLOBAL.click}
          onClick={() => run(() => api.setClick(!clock.click))}
          title="Key K. The click is on the monitor bus and never reaches Main."
        >
          <Icon name="metro" />
          {clock.click ? T.clickOn : T.clickOff}
        </button>
      </div>
    </div>
  );
}
