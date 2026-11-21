//! The title bar: the macOS overlay title bar with the traffic lights let into it, and everything
//! that has to be reachable from every view.
//!
//! ```text
//!  ● ● ●  Title       [▶][■][▶| Release]  [SECTION|BAR|BEAT|TEMPO|TIME SIG]   [♪ Click ▾] Arrangement|Live   MIDI Learn  (status)  ⚙
//!         file state   ^ Arrangement only (Live has the same controls full size)   ^ every view
//! ```
//!
//! The bar itself is the window's drag region (`data-tauri-drag-region`); the controls inside are
//! not, so a press on a button never starts moving the window.

import type { ReactNode } from "react";
import { midiApi } from "../api";
import { FREE_TRANSPORT, TRANSPORT } from "../midi/addresses";
import { setLearnMode, setMidiView, sayMidi, useLearnMode, useMidiView } from "../midi/store";
import { IS_MAC } from "../platform";
import { useStatusSlice } from "../status";
import { Icon } from "./Icon";
import { engineWords, useEngine } from "./engine";
import { Ticker } from "./Toasts";
import { ClickSwitch } from "./ClickSwitch";
import { OutputClip } from "./OutputClip";
import { RecordingBadge } from "../recording/RecordingBadge";
import { isDirty, useScoreFile } from "../scoreFile";
import {
  freeTransport,
  scoreCanStart,
  scoreNext,
  scoreRewind,
  scoreRuns,
  scoreStart,
  scoreStopAll,
  useScore,
  type ScoreView,
} from "./scoreTransport";
import { PHASE, T, num } from "./terms";
import type { LooperStatus } from "../types";

export type Mode = "arrangement" | "live";

interface Props {
  mode: Mode;
  setupOpen: boolean;
  onMode: (mode: Mode) => void;
  onSetup: () => void;
  enginePanelOpen: boolean;
  onEnginePanel: () => void;
}

interface Facts {
  running: boolean;
  /** The stage can be shown - the engine runs, or the resting status of the setup is there. */
  ready: boolean;
  bpm: number;
  bpb: number;
  unit: number;
  bars: number;
  click: boolean;
  rate: number;
  xruns: number;
  overruns: number;
  bar: number;
  beat: number;
  /** Whether the clock runs; while it stands the LCD stays at bar 1 and lights no beat. */
  transport: boolean;
}

function pickFacts(s: LooperStatus): Facts {
  return {
    running: s.running,
    ready: s.ready,
    bpm: s.bpm,
    bpb: s.beats_per_bar,
    unit: s.beat_unit,
    bars: s.bars,
    click: s.click,
    rate: s.sample_rate,
    xruns: s.xruns + s.other_errors,
    overruns: s.fifo_overruns,
    bar: s.bar,
    beat: s.beat,
    transport: s.transport,
  };
}

function sameFacts(a: Facts, b: Facts): boolean {
  return (
    a.running === b.running &&
    a.ready === b.ready &&
    a.bpm === b.bpm &&
    a.bpb === b.bpb &&
    a.unit === b.unit &&
    a.bars === b.bars &&
    a.click === b.click &&
    a.rate === b.rate &&
    a.xruns === b.xruns &&
    a.overruns === b.overruns &&
    a.bar === b.bar &&
    a.beat === b.beat &&
    a.transport === b.transport
  );
}

export function useFacts(): Facts {
  return useStatusSlice(pickFacts, sameFacts);
}

export function TitleBar({ mode, setupOpen, onMode, onSetup, enginePanelOpen, onEnginePanel }: Props) {
  const facts = useFacts();
  const score = useScore();
  const learn = useLearnMode();
  const midi = useMidiView();

  const toggleLearn = () => {
    const next = !learn;
    setLearnMode(next);
    if (!next && midi.learning) {
      midiApi
        .learn(null)
        .then(setMidiView)
        .catch((e) => sayMidi((e as Error).message));
    }
  };

  const showTransport = mode === "arrangement" && !setupOpen;
  const locked = mode === "live" && !setupOpen && scoreRuns(score);

  return (
    <header className={`titlebar${IS_MAC ? " mac" : ""}${showTransport ? " with-transport" : ""}`} data-tauri-drag-region>
      <Doc score={score} />
      {showTransport && (
        <>
          <span className="sep" />
          {score.loaded ? <Transport score={score} /> : <FreeTransport facts={facts} />}
          <Lcd facts={facts} score={score} />
        </>
      )}
      <div className="grow ticker-slot" data-tauri-drag-region>
        <Ticker />
      </div>
      <RecordingBadge />
      <ClickSwitch variant="bar" />
      <div className="seg mode" role="tablist">
        <button
          role="tab"
          aria-selected={mode === "arrangement"}
          className={mode === "arrangement" && !setupOpen ? "on" : ""}
          onClick={() => onMode("arrangement")}
        >
          <Icon name="grid" />
          {T.arrangement}
        </button>
        <button
          role="tab"
          aria-selected={mode === "live"}
          className={mode === "live" && !setupOpen ? "on" : ""}
          onClick={() => onMode("live")}
        >
          <Icon name="play-o" />
          {T.live}
        </button>
      </div>
      <div className="grow" data-tauri-drag-region />
      <button
        className={`btn learn-btn${learn ? " on" : ""}`}
        onClick={toggleLearn}
        title={`While on, clicking a control assigns it instead of using it; then press the pad or key. ${IS_MAC ? "⌘" : "Ctrl"}+click works without the switch.`}
      >
        <Icon name="midi" />
        <span className="tb-word">{T.midiLearn}</span>
      </button>
      {locked && (
        <button
          className="pill hide-narrow lockpill"
          onClick={() => onMode("arrangement")}
          title="While the score runs, the arrangement can be looked at but not edited - press Stop all to edit it again. Click to view it."
        >
          <Icon name="lock" size={11} />
          Arrangement locked
        </button>
      )}
      <OutputClip onOpen={() => !enginePanelOpen && onEnginePanel()} />
      <StatusPill facts={facts} open={enginePanelOpen} onClick={onEnginePanel} />
      <button
        className={`btn sq ghost${setupOpen ? " on" : ""}`}
        onClick={onSetup}
        title={T.setup}
        aria-pressed={setupOpen}
      >
        <Icon name="gear" />
      </button>
    </header>
  );
}

/** The score: its title (or file name), whether the file is saved, and what the runner does. */
function Doc({ score }: { score: ScoreView }) {
  const file = useScoreFile();
  const dirty = isDirty(file);
  const name = score.loaded ? score.title : (file.name?.replace(/\.ya?ml$/i, "") ?? "Untitled");
  const saved = file.path === null && !dirty ? "no file" : dirty ? T.unsaved : T.saved;
  const phase = score.loaded && score.phase !== "none" ? ` · ${PHASE[score.phase]}` : score.loaded ? "" : " · not loaded";
  return (
    <div className="doc" data-tauri-drag-region title={file.path ?? undefined}>
      <b data-tauri-drag-region>{name}</b>
      <span data-tauri-drag-region className={dirty ? "dirty" : ""}>
        <i className="dot" />
        {saved}
        {phase}
      </span>
    </div>
  );
}

function Transport({ score }: { score: ScoreView }) {
  const runs = scoreRuns(score);
  const armed = score.armed !== null && !score.armed.countIn;
  const over = score.phase === "finished" || score.phase === "stopped";
  return (
    <div className="transport">
      <button
        className="tp"
        disabled={!over}
        onClick={scoreRewind}
        title="Back to start: clears all tracks and returns to the first section. Asks first."
      >
        <Icon name="tostart" />
      </button>
      <button
        className={`tp start${runs ? " on" : ""}`}
        data-midi={TRANSPORT.start}
        disabled={!scoreCanStart(score)}
        onClick={scoreStart}
        title={`${T.start} the score · Space`}
      >
        <Icon name="play" />
      </button>
      <button
        className="tp"
        data-midi={TRANSPORT.stopAll}
        disabled={!runs}
        onClick={scoreStopAll}
        title={`${T.stopAll} · Shift+S`}
      >
        <Icon name="stop" />
      </button>
      <button
        className={`tp release${armed ? " armed" : ""}`}
        data-midi={TRANSPORT.next}
        disabled={!runs}
        onClick={scoreNext}
        title={`${T.release}: arm the switch to the next section · Space or N`}
      >
        <Icon name={armed ? "armed" : "release"} />
        {T.release}
      </button>
    </div>
  );
}

/** Without a score: one button, Start/Stop of the clock - the same as the big one in Live. */
function FreeTransport({ facts }: { facts: Facts }) {
  return (
    <div className="transport">
      <button
        className={`tp start${facts.transport ? " on" : ""}`}
        data-midi={FREE_TRANSPORT}
        disabled={!facts.ready}
        onClick={() => freeTransport(!facts.transport)}
        title={`${facts.transport ? T.transportStop : T.transportStart} the clock · Space`}
        aria-pressed={facts.transport}
      >
        <Icon name={facts.transport ? "stop" : "play"} />
      </button>
    </div>
  );
}

function Lcd({ facts, score }: { facts: Facts; score: ScoreView }) {
  const dash = "–";
  const withScore = score.loaded && score.section !== null;
  const barInLoop = facts.bars > 0 ? ((facts.bar - 1) % facts.bars) + 1 : facts.bar;
  const bar = !facts.ready ? dash : withScore ? score.bar : barInLoop;
  const of = !facts.ready ? "" : withScore ? score.barsTotal : facts.bars;
  // A standing clock lights no beat dot - nothing on the bar may blink while one arranges.
  const beat = facts.running && facts.transport ? (withScore ? score.beat : facts.beat) : 0;
  const dots = facts.bpb > 0 ? facts.bpb : 4;
  return (
    <div className="lcd">
      {/* Not `sec`: that is the Arrangement's section marker (absolutely positioned, 32 px high
          from top 5 px), and sharing the name pushed this cell out of the bottom of the LCD. */}
      <div className="lcd-sec">
        <label>
          {withScore ? `${T.section} ${score.section! + 1} / ${score.sectionCount}` : score.loaded ? PHASE[score.phase as keyof typeof PHASE] ?? T.section : T.loop}
        </label>
        <b>
          {withScore
            ? score.sectionId
            : score.loaded
              ? score.title
              : !facts.ready
                ? dash
                : facts.transport
                  ? "free loop"
                  : T.transportStopped.toLowerCase()}
        </b>
      </div>
      <div>
        <label>{T.bar}</label>
        <b>
          {bar}
          {of !== "" && <small>/ {of}</small>}
        </b>
      </div>
      <div>
        <label>{T.beat}</label>
        <b>
          {facts.ready ? beat || 1 : dash}
          <span className="beats">
            {Array.from({ length: Math.min(dots, 12) }, (_, i) => (
              <i key={i} className={`${i === 0 ? "one" : ""}${i + 1 === beat && facts.running ? " on" : ""}`} />
            ))}
          </span>
        </b>
      </div>
      <div>
        <label>{T.tempo}</label>
        <b>
          {facts.ready ? num(facts.bpm) : dash}
          <small>BPM</small>
        </b>
      </div>
      <div>
        <label>{T.timeSig}</label>
        <b>{facts.ready ? `${facts.bpb}/${facts.unit}` : dash}</b>
      </div>
    </div>
  );
}

function StatusPill({ facts, open, onClick }: { facts: Facts; open: boolean; onClick: () => void }) {
  const { info: engine, view } = useEngine();
  const frames = engine ? (engine.driver_input_frames ?? engine.buffer_frames) : null;
  let body: ReactNode;
  let cls = "pill status-pill";
  if (!facts.running) {
    // Not an instruction - the engine is the app's business. A calm word about where it stands.
    const phase = view?.phase ?? "off";
    cls += phase === "starting" ? " starting" : phase === "failed" ? " err" : phase === "waiting" ? " warn" : " off";
    body = (
      <>
        <i className="led" />
        {engineWords(view)}
      </>
    );
  } else {
    if (facts.overruns > 0) cls += " err";
    else if (facts.xruns > 0) cls += " warn";
    body = (
      <>
        <i className="led" />
        {num(facts.rate / 1000)} kHz{frames !== null ? ` · ${frames}` : ""} · {facts.xruns} {T.dropouts}
        {facts.overruns > 0 ? ` · ${facts.overruns} lost` : ""}
      </>
    );
  }
  return (
    <button className={`${cls}${open ? " open" : ""}`} onClick={onClick} title="Engine, buses and counters">
      {body}
    </button>
  );
}
