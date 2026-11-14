//! The Live mode: the stage screen (design/live.html).
//!
//! ```text
//!  ┌ position ─────────────────────┐ ┌ Release (the surface is the button) ┐
//!  │ SECTION 4/6  ▶ running        │ │ switch armed · to alles · in 2 bars │
//!  │ 7 /8   voice_3   ● ○ ○        │ ├──────────────────┬──────────────────┤
//!  │ ▬▬▬▬▬▬▭▨                      │ │ ▶ Running 1:13   │ ■ Stop all       │
//!  └───────────────────────────────┘ └──────────────────┴──────────────────┘
//!  [ done | done | NOW | UP NEXT | … ]                      arrangement strip
//!  [ tile ][ tile ][ tile ][ tile ][ tile ]                one per track
//! ```
//!
//! Without a score the same frame is a free looper: the position block counts the loop, the
//! Release surface shows the next armed take with its countdown, the two big buttons become the
//! grid and the click. With the engine stopped it offers to start it.
//!
//! Rendering discipline (the engine pushes 20 events a second): this component watches the track
//! *structure*, the clock (bar/beat) and the runner view, all of which change a few times a second
//! at most. Levels and the progress inside a bar are written into the DOM without a render.
//!
//! Keyboard: a digit picks a track, then R O S P M act on it, K toggles the click, F takes the
//! selected track's effect chain in or out, I opens its details. X, V and D wait for one more digit
//! - `X 3` switches the compressor, `V 1` loads "Voice", `D 2` sets the delay to a dotted eighth,
//! exactly as in the CLI. Space/N and Shift+S belong to the score transport (see `scoreTransport`).

import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { api } from "../api";
import { currentStatus, structureSignature, useStatusEffect, useStatusSlice } from "../status";
import { run } from "../shell/notes";
import { Icon } from "../shell/Icon";
import { startEngine, useEngine } from "../shell/engine";
import { useScore } from "../shell/scoreTransport";
import { useLoadedScore } from "../shell/scoreModel";
import { T } from "../shell/terms";
import { currentConfig } from "../setup/config";
import { PositionPanel, type Clock } from "./PositionPanel";
import { ReleasePanel } from "./ReleasePanel";
import { ArrangementStrip } from "./ArrangementStrip";
import { TrackTile, trackColor } from "./TrackTile";
import { TrackDetail, type TrackActions } from "./TrackDetail";
import { DELAY_NOTES, FX_NAME, FX_PRESETS, FX_SLOT_ORDER } from "./FxRow";
import type { LooperStatus, LooperTrackStatus, ScoreState } from "../types";

type Chord = "x" | "v" | "d";

const CHORDS: Record<Chord, { title: string; options: string[] }> = {
  x: { title: "Toggle effect", options: FX_SLOT_ORDER.map((n) => FX_NAME[n]) },
  v: { title: "Load preset", options: FX_PRESETS.map((p) => p.label) },
  d: { title: "Delay note", options: DELAY_NOTES.map((n) => n.label) },
};

const CHORD_TIMEOUT_MS = 3000;

function runChord(key: Chord, digit: number, track: LooperTrackStatus, act: TrackActions): void {
  if (key === "x") {
    const name = FX_SLOT_ORDER[digit - 1];
    const effect = track.fx?.effects.find((e) => e.name === name);
    if (name && effect) act.fxEnable(track.index, name, !effect.on);
    return;
  }
  if (key === "v") {
    const preset = FX_PRESETS[digit - 1];
    if (preset) act.fxPreset(track.index, preset.name);
    return;
  }
  const note = DELAY_NOTES[digit - 1];
  if (note) act.fxDelayNote(track.index, note.name);
}

function pickClock(s: LooperStatus): Clock & { latency: number } {
  return {
    bar: s.bar,
    beat: s.beat,
    bpb: s.beats_per_bar,
    unit: s.beat_unit,
    bars: s.bars,
    bpm: s.bpm,
    click: s.click,
    loopSeconds: s.loop_seconds,
    quantize: s.quantize,
    latency: s.latency_frames,
  };
}

function sameClock(a: ReturnType<typeof pickClock>, b: ReturnType<typeof pickClock>): boolean {
  return (
    a.bar === b.bar &&
    a.beat === b.beat &&
    a.bpb === b.bpb &&
    a.unit === b.unit &&
    a.bars === b.bars &&
    a.bpm === b.bpm &&
    a.click === b.click &&
    a.loopSeconds === b.loopSeconds &&
    a.quantize === b.quantize &&
    a.latency === b.latency
  );
}

const actions: TrackActions = {
  record: (t) => run(() => api.trackRecord(t)),
  overdub: (t) => run(() => api.trackOverdub(t)),
  stop: (t) => run(() => api.trackStop(t)),
  play: (t) => run(() => api.trackPlay(t)),
  clear: (t) => run(() => api.trackClear(t)),
  monitor: (t, on) => run(() => api.trackMonitor(t, on)),
  bus: (t, source, bus, on) => run(() => api.trackBus(t, source, bus, on)),
  pan: (t, pan) => run(() => api.trackPan(t, pan)),
  latency: (t, measured, trim) => run(() => api.trackLatency(t, measured, trim)),
  layerMute: (t, l, muted) => run(() => api.layerMute(t, l, muted)),
  layerRemove: (t, l) => run(() => api.layerRemove(t, l)),
  layerGain: (t, l, gain) => run(() => api.layerGain(t, l, gain)),
  fxBypass: (t, on) => run(() => api.fxBypass(t, on)),
  fxEnable: (t, effect, on) => run(() => api.fxEnable(t, effect, on)),
  fxPreset: (t, preset) => run(() => api.fxPreset(t, preset)),
  fxSet: (t, param, value, band) => run(() => api.fxSet(t, param, value, band)),
  fxBandKind: (t, band, kind) => run(() => api.fxBandKind(t, band, kind)),
  fxDelayNote: (t, note) => run(() => api.fxDelayNote(t, note)),
};

/** True while the Live area is narrower than 1200 px - the same break live.css uses. */
function useNarrow(ref: React.RefObject<HTMLElement | null>): boolean {
  const [narrow, setNarrow] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => setNarrow(entry.contentRect.width < 1200));
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  return narrow;
}

interface Props {
  onOpenSetup: () => void;
  onOpenArrangement: () => void;
}

export function LiveView(props: Props) {
  const running = useStatusSlice((s) => s.running);
  if (!running) return <EngineOff {...props} />;
  return <Stage {...props} />;
}

function EngineOff({ onOpenSetup }: Props) {
  const { busy } = useEngine();
  const config = currentConfig();
  return (
    <div className="live-wrap">
      <div className="live live-off">
        <section className="panel offcard">
          <Icon name="power" size={40} />
          <h2>{T.engineStopped}</h2>
          <p>
            {config.host} · {config.device ?? "default device"} · {config.sample_rate / 1000} kHz ·{" "}
            {config.buffer_frames} frames · {config.tracks.length} tracks
          </p>
          <div className="row">
            <button className="bigbtn running" disabled={busy} onClick={() => void startEngine(currentConfig())}>
              <Icon name="power" />
              {busy ? "Starting…" : T.startEngine}
            </button>
            <button className="bigbtn" onClick={onOpenSetup}>
              <Icon name="gear" />
              {T.setup}
            </button>
          </div>
        </section>
      </div>
    </div>
  );
}

function Stage({ onOpenArrangement }: Props) {
  const [selected, setSelected] = useState(0);
  const [detail, setDetail] = useState<number | null>(null);
  const clock = useStatusSlice(pickClock, sameClock);
  const score = useScore();
  const compiled = useLoadedScore();

  const signature = useStatusSlice(structureSignature);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const tracks = useMemo(() => currentStatus().tracks, [signature]);

  useEffect(() => {
    if (selected >= tracks.length && tracks.length > 0) setSelected(tracks.length - 1);
  }, [tracks.length, selected]);

  // Development only: the autopilot can open a track's details (dev/autopilot.ts).
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const onPilot = (e: Event) => {
      const d = (e as CustomEvent<{ detail?: { track?: number | null } }>).detail.detail;
      if (d && "track" in d) setDetail(d.track ?? null);
    };
    window.addEventListener("looper-autopilot", onPilot);
    return () => window.removeEventListener("looper-autopilot", onPilot);
  }, []);

  // What each track does in the section that comes next: the armed one, else the following one.
  const nextIndex =
    score.armed && !score.armed.countIn
      ? score.armed.section
      : score.armed?.countIn
        ? 0
        : score.section !== null
          ? score.section + 1
          : score.phase === "idle"
            ? 0
            : null;
  const nextStates = useMemo<(ScoreState | null)[]>(() => {
    if (!compiled || nextIndex === null) return [];
    const section = compiled.sections[nextIndex];
    if (!section) return compiled.tracks.map(() => "stop" as ScoreState);
    return compiled.tracks.map((t) => section.tracks[t.name] ?? null);
  }, [compiled, nextIndex]);

  // --- the pulse, in time --------------------------------------------------------------------
  // `--bp` is the progress inside the bar (0..1) for the "now" segment and the countdown;
  // `data-beat` flips on every beat so the recording/armed pulse restarts on the beat, and
  // `--beat-dur` is one beat long - the glow breathes with the music rather than on its own clock.
  const root = useRef<HTMLDivElement | null>(null);
  const narrow = useNarrow(root);
  const lastBeat = useRef(-1);
  const lastBp = useRef(-1);
  useStatusEffect((s) => {
    const el = root.current;
    if (!el || !s.running) return;
    const spb = s.samples_per_beat;
    const bpb = s.beats_per_bar || 4;
    const inBeat = spb > 0 ? Math.min(1, s.beat_offset / spb) : 0;
    const bp = Math.round(((s.beat - 1 + inBeat) / bpb) * 100) / 100;
    if (bp !== lastBp.current) {
      el.style.setProperty("--bp", String(bp));
      lastBp.current = bp;
    }
    const beatId = s.bar * 64 + s.beat;
    if (beatId !== lastBeat.current) {
      lastBeat.current = beatId;
      el.dataset.beat = beatId % 2 === 0 ? "a" : "b";
      el.classList.toggle("downbeat", s.beat === 1);
    }
  });

  // --- keyboard --------------------------------------------------------------------------------
  const state = useRef({ tracks, selected, click: clock.click });
  state.current = { tracks, selected, click: clock.click };
  const chord = useRef<Chord | null>(null);
  const chordTimer = useRef<number | undefined>(undefined);
  const [chordShown, setChordShown] = useState<Chord | null>(null);

  const clearChord = useCallback(() => {
    window.clearTimeout(chordTimer.current);
    chordTimer.current = undefined;
    chord.current = null;
    setChordShown(null);
  }, []);

  const armChord = useCallback(
    (key: Chord) => {
      window.clearTimeout(chordTimer.current);
      chord.current = key;
      setChordShown(key);
      chordTimer.current = window.setTimeout(clearChord, CHORD_TIMEOUT_MS);
    },
    [clearChord],
  );

  useEffect(() => () => window.clearTimeout(chordTimer.current), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
      // Shift belongs to the score transport (Shift+S stops the whole run).
      if (e.shiftKey) return;
      const target = e.target as HTMLElement | null;
      if (target && target.tagName) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable) return;
        if (target.closest(".cm-editor")) return;
      }
      const { tracks: list, selected: current, click } = state.current;

      if (e.key === "Escape" && chord.current) {
        e.preventDefault();
        clearChord();
        return;
      }
      if (chord.current && !(e.key >= "1" && e.key <= "9")) clearChord();

      if (e.key >= "1" && e.key <= "9") {
        const pending = chord.current;
        if (pending) {
          e.preventDefault();
          clearChord();
          const track = list[current];
          if (track) runChord(pending, Number(e.key), track, actions);
          return;
        }
        const index = Number(e.key) - 1;
        if (index < list.length) {
          e.preventDefault();
          setSelected(index);
          setDetail((d) => (d === null ? null : index));
        }
        return;
      }
      const key = e.key.toLowerCase();
      if (key === "k") {
        e.preventDefault();
        run(() => api.setClick(!click));
        return;
      }
      const track = list[current];
      if (!track) return;
      switch (key) {
        case "r":
          e.preventDefault();
          actions.record(track.index);
          break;
        case "o":
          e.preventDefault();
          actions.overdub(track.index);
          break;
        case "s":
          e.preventDefault();
          actions.stop(track.index);
          break;
        case "p":
          e.preventDefault();
          actions.play(track.index);
          break;
        case "m":
          e.preventDefault();
          actions.monitor(track.index, !track.monitor);
          break;
        case "i":
          e.preventDefault();
          setDetail((d) => (d === track.index ? null : track.index));
          break;
        case "f":
          e.preventDefault();
          if (track.fx) actions.fxBypass(track.index, !track.fx.bypass);
          break;
        case "x":
        case "v":
        case "d":
          e.preventDefault();
          armChord(key);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [armChord, clearChord]);

  // --- render ----------------------------------------------------------------------------------
  const barInLoop = clock.bars > 0 ? ((clock.bar - 1) % clock.bars) + 1 : clock.bar;
  const count = tracks.length;
  // Up to five tracks in one row (the mockup). Six still fit one row on a wide window; below 1200 px
  // they, like seven or eight, go into two rows of compact tiles.
  const layout = count <= 5 ? "one" : count === 6 && !narrow ? "six" : "two";
  const beatDur = clock.bpm > 0 ? 60 / clock.bpm : 0.5;
  const detailTrack = detail !== null ? tracks[detail] : undefined;

  return (
    <div className="live-wrap">
      <div
        className={`live tiles-${layout}${detailTrack ? " with-drawer" : ""}`}
        ref={root}
        style={{ "--beat-dur": `${beatDur}s`, "--n-tracks": Math.max(count, 1), "--cols": Math.ceil(count / 2) } as CSSProperties}
      >
        <div className="lv-top">
          <PositionPanel clock={clock} score={score} compiled={compiled} />
          <ReleasePanel score={score} compiled={compiled} clock={clock} tracks={tracks} />
        </div>

        <ArrangementStrip score={score} compiled={compiled} onOpenArrangement={onOpenArrangement} />

        <div className="tiles">
          {tracks.map((t) => {
            const writing = t.state === "recording" || t.state === "overdub";
            return (
              <TrackTile
                key={t.index}
                track={t}
                selected={t.index === selected}
                onSelect={setSelected}
                onDetail={(i) => setDetail((d) => (d === i ? null : i))}
                actions={actions}
                next={nextStates[t.index] ?? null}
                barInLoop={writing ? barInLoop : 0}
                loopBars={clock.bars}
              />
            );
          })}
          {count === 0 && <div className="panel tiles-empty">No tracks. Add some in {T.setup}.</div>}
        </div>

        {chordShown && (
          <div className="chord" aria-live="polite">
            <kbd>{chordShown.toUpperCase()}</kbd>
            <b>{CHORDS[chordShown].title}</b>
            <span className="dim">
              Track {selected + 1}
              {tracks[selected] ? ` · ${tracks[selected].name}` : ""}
            </span>
            <ol>
              {CHORDS[chordShown].options.map((option, n) => (
                <li key={option}>
                  <kbd>{n + 1}</kbd> {option}
                </li>
              ))}
            </ol>
            <span className="dim">
              <kbd>Esc</kbd> cancel
            </span>
          </div>
        )}

        {detailTrack && (
          <TrackDetail
            track={detailTrack}
            color={trackColor(detailTrack.index)}
            actions={actions}
            defaultLatency={clock.latency}
            onClose={() => setDetail(null)}
          />
        )}
      </div>
    </div>
  );
}
