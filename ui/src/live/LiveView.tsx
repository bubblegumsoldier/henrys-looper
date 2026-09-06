//! The live view. Two states, and which one is showing is decided by the engine, not by a click:
//! `running` from the status event picks the setup screen or the stage screen.
//!
//! Rendering discipline (the engine pushes 20 events a second):
//!
//! * this component watches `running`, the tempo block, the newest message and the track
//!   *structure* - all of which change when something musical happens, not on a tick;
//! * bar and beat live in `Position`, which renders a few times a second;
//! * peaks and the pulse inside a beat are written into the DOM by `Meter` and `Position` without
//!   any render at all.
//!
//! Keyboard: a digit picks a track, then R O S P M act on it, K toggles the click and F takes the
//! selected track's effect chain in or out. X, V and D are the three effect commands that need an
//! argument, so they wait for one more digit - `X 3` switches the compressor, `V 1` loads "Stimme",
//! `D 2` sets the delay to a dotted eighth, exactly as in the CLI. No Enter, no modifier - the
//! hands are on an instrument. Anything typed into a field is left alone.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import { Position } from "./Position";
import { Setup } from "./Setup";
import { TrackCard, type TrackActions } from "./TrackCard";
import { DELAY_NOTES, FX_PRESETS, FX_SLOT_ORDER } from "./FxRow";
import { Warnings } from "./Warnings";
import { Meter } from "./Meter";
import type { AppInfo, EngineInfo, LooperStatus, LooperTrackStatus, Quantize, StartConfig } from "../types";

/**
 * A letter that waits for a digit, exactly like the CLI's `x 3`, `v stimme` and `d 1/8.`.
 *
 * The single letters are taken (R O S P M K F), and a digit on its own already picks a track, so
 * the three effect commands that need an argument became two-key chords. The banner below the head
 * says which digit means what while one is half typed, so nothing has to be remembered.
 */
type Chord = "x" | "v" | "d";

const CHORDS: Record<Chord, { title: string; options: string[] }> = {
  x: { title: "Effekt umschalten", options: ["Hochpass", "EQ", "Kompressor", "Delay", "Hall"] },
  v: { title: "Preset laden", options: FX_PRESETS.map((p) => p.label) },
  d: { title: "Delay-Notenwert", options: DELAY_NOTES.map((n) => n.label) },
};

/** How long a half-typed chord waits for its digit before it is forgotten again. */
const CHORD_TIMEOUT_MS = 3000;

/** The second half of a chord: the digit, acting on the selected track. */
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

interface Tempo {
  bpm: number;
  beatsPerBar: number;
  beatUnit: number;
  bars: number;
  loopSeconds: number;
  click: boolean;
  quantize: Quantize;
  sampleRate: number;
  /** The engine's default compensation in frames; a track can carry its own. */
  latencyFrames: number;
}

function pickTempo(s: LooperStatus): Tempo {
  return {
    bpm: s.bpm,
    beatsPerBar: s.beats_per_bar,
    beatUnit: s.beat_unit,
    bars: s.bars,
    loopSeconds: s.loop_seconds,
    click: s.click,
    quantize: s.quantize,
    sampleRate: s.sample_rate,
    latencyFrames: s.latency_frames,
  };
}

function sameTempo(a: Tempo, b: Tempo): boolean {
  return (
    a.bpm === b.bpm &&
    a.beatsPerBar === b.beatsPerBar &&
    a.beatUnit === b.beatUnit &&
    a.bars === b.bars &&
    a.loopSeconds === b.loopSeconds &&
    a.click === b.click &&
    a.quantize === b.quantize &&
    a.sampleRate === b.sampleRate &&
    a.latencyFrames === b.latencyFrames
  );
}

// The master, per side: a mix that clips only on the right has to say so on the right.
const masterLeft = (s: LooperStatus) => s.output_peaks?.[0] ?? 0;
const masterRight = (s: LooperStatus) => s.output_peaks?.[1] ?? 0;

interface Note {
  text: string;
  kind: "engine" | "error";
  seq: number;
}

let noteSeq = 0;

export function LiveView({ info }: { info: AppInfo | null }) {
  const running = useStatusSlice((s) => s.running);
  const [engine, setEngine] = useState<EngineInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState(0);
  const [notes, setNotes] = useState<Note[]>([]);

  const note = useCallback((text: string, kind: Note["kind"]) => {
    setNotes((list) => [{ text, kind, seq: ++noteSeq }, ...list].slice(0, 6));
  }, []);

  const fail = useCallback((message: string) => note(message, "error"), [note]);

  /** Every command goes through here, so a refusal always ends up on screen. */
  const run = useCallback(
    (work: () => Promise<unknown>) => {
      void work().catch((e) => fail((e as Error).message));
    },
    [fail],
  );

  // The engine's own sentence about the last thing that happened.
  const message = useStatusSlice((s) => s.message);
  useEffect(() => {
    if (message) note(message, "engine");
  }, [message, note]);

  // --- setup -> running ----------------------------------------------------
  const start = useCallback(
    (config: StartConfig) => {
      setBusy(true);
      api
        .engineStart(config)
        .then((opened) => {
          setEngine(opened);
          note(
            `Engine läuft: ${opened.host}, ${opened.input_device}, ${opened.sample_rate} Hz, ` +
              `${opened.driver_input_frames ?? opened.buffer_frames} Frames, Latenz ${opened.latency_ms.toFixed(2)} ms.`,
            "engine",
          );
        })
        .catch((e) => fail((e as Error).message))
        .finally(() => setBusy(false));
    },
    [note, fail],
  );

  const stop = useCallback(() => {
    setBusy(true);
    api
      .engineStop()
      .then(() => setEngine(null))
      .catch((e) => fail((e as Error).message))
      .finally(() => setBusy(false));
  }, [fail]);

  // --- track structure -----------------------------------------------------
  // A signature over everything that changes the layout. The array itself is read from the hub, so
  // the cards are rebuilt when a state, a name, a layer or a mute changes - and never on a peak.
  const signature = useStatusSlice(structureSignature);
  const tracks = useMemo(() => currentStatus().tracks, [signature]);

  useEffect(() => {
    if (selected >= tracks.length && tracks.length > 0) setSelected(tracks.length - 1);
  }, [tracks.length, selected]);

  const actions = useMemo<TrackActions>(
    () => ({
      record: (t) => run(() => api.trackRecord(t)),
      overdub: (t) => run(() => api.trackOverdub(t)),
      stop: (t) => run(() => api.trackStop(t)),
      play: (t) => run(() => api.trackPlay(t)),
      clear: (t) => run(() => api.trackClear(t)),
      monitor: (t, on) => run(() => api.trackMonitor(t, on)),
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
    }),
    [run],
  );

  const tempo = useStatusSlice(pickTempo, sameTempo);

  // --- keyboard ------------------------------------------------------------
  // Kept in refs so the listener is installed once and never sees a stale track list.
  const state = useRef({ tracks, selected, actions, click: tempo.click });
  state.current = { tracks, selected, actions, click: tempo.click };

  // A half-typed chord. The ref is what the listener reads, the state is what the banner draws -
  // one render when a chord is armed and one when it is spent, not one per event.
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
    if (!running) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
      const target = e.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable) return;
        if (target.closest(".cm-editor")) return;
      }
      const { tracks: list, selected: current, actions: act, click } = state.current;

      if (e.key === "Escape" && chord.current) {
        e.preventDefault();
        clearChord();
        return;
      }

      // Anything that is not a digit ends a half-typed chord, and then acts as it normally would.
      // Without this an abandoned `X` would still be waiting when the next digit picks a track.
      if (chord.current && !(e.key >= "1" && e.key <= "9")) clearChord();

      if (e.key >= "1" && e.key <= "9") {
        // A digit closes a half-typed chord; only an unclaimed digit picks a track.
        const pending = chord.current;
        if (pending) {
          e.preventDefault();
          clearChord();
          const track = list[current];
          if (track) runChord(pending, Number(e.key), track, act);
          return;
        }
        const index = Number(e.key) - 1;
        if (index < list.length) {
          e.preventDefault();
          setSelected(index);
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
          act.record(track.index);
          break;
        case "o":
          e.preventDefault();
          act.overdub(track.index);
          break;
        case "s":
          e.preventDefault();
          act.stop(track.index);
          break;
        case "p":
          e.preventDefault();
          act.play(track.index);
          break;
        case "m":
          e.preventDefault();
          act.monitor(track.index, !track.monitor);
          break;
        // --- the effect chain, the same four keys the CLI has --------------
        case "f":
          e.preventDefault();
          if (track.fx) act.fxBypass(track.index, !track.fx.bypass);
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
  }, [running, run, armChord, clearChord]);

  // --- render --------------------------------------------------------------
  if (!running) {
    return (
      <div className="live live-setup">
        <Notes notes={notes} onClear={() => setNotes([])} />
        <Setup info={info} busy={busy} onStart={start} onError={fail} />
      </div>
    );
  }

  return (
    <div className="live live-running">
      <header className="live-head">
        <div className="live-head-facts">
          <span className="live-fact">
            <b>{tempo.bpm.toFixed(tempo.bpm % 1 === 0 ? 0 : 1)}</b> BPM
          </span>
          <span className="live-fact">
            <b>
              {tempo.beatsPerBar}/{tempo.beatUnit}
            </b>{" "}
            Takt
          </span>
          <span className="live-fact">
            <b>{tempo.bars}</b> Takte · <b>{tempo.loopSeconds.toFixed(2)}</b> s Loop
          </span>
          <span className="live-fact muted mono">
            {tempo.sampleRate} Hz · {tempo.latencyFrames} Frames Kompensation (Vorgabe)
          </span>
        </div>
        <div className="spacer" />
        <div className="live-master-meters">
          <Meter label="Summe L" kind="out" pick={masterLeft} />
          <Meter label="Summe R" kind="out" pick={masterRight} />
        </div>
        <button
          className="btn btn-big btn-quantize"
          onClick={() => run(() => api.setQuantize(tempo.quantize === "loop" ? "bar" : "loop"))}
          title={
            tempo.quantize === "loop"
              ? "Aufnahme beginnt am nächsten Loop-Anfang. Klick schaltet auf die nächste Taktgrenze um."
              : "Aufnahme beginnt an der nächsten Taktgrenze. Klick schaltet auf den nächsten Loop-Anfang um."
          }
        >
          Start ab {tempo.quantize === "loop" ? "Loop" : "Takt"}
        </button>
        <button
          className={`btn btn-big${tempo.click ? " btn-on" : ""}`}
          onClick={() => run(() => api.setClick(!tempo.click))}
          title="Taste K"
        >
          Klick {tempo.click ? "an" : "aus"} <kbd>K</kbd>
        </button>
        <button className="btn btn-big btn-danger" onClick={() => run(() => api.clearAll())}>
          Alles leeren
        </button>
        <button className="btn btn-big btn-stop" onClick={stop} disabled={busy}>
          Engine stoppen
        </button>
      </header>

      <Warnings />
      <Position />
      <Notes notes={notes} onClear={() => setNotes([])} />

      {chordShown && (
        <div className="live-chord" aria-live="polite">
          <kbd className="live-chord-key">{chordShown.toUpperCase()}</kbd>
          <span className="live-chord-title">{CHORDS[chordShown].title}</span>
          <span className="live-chord-track">
            Track {selected + 1}
            {tracks[selected] ? ` · ${tracks[selected].name}` : ""}
          </span>
          <ol className="live-chord-options">
            {CHORDS[chordShown].options.map((option, n) => (
              <li key={option}>
                <kbd>{n + 1}</kbd> {option}
              </li>
            ))}
          </ol>
          <span className="live-chord-esc">
            <kbd>Esc</kbd> abbrechen
          </span>
        </div>
      )}

      <div className="live-tracks">
        {tracks.map((t) => (
          <TrackCard
            key={t.index}
            track={t}
            selected={t.index === selected}
            onSelect={setSelected}
            actions={actions}
            defaultLatency={tempo.latencyFrames}
          />
        ))}
      </div>

      {engine && (
        <footer className="live-foot mono muted">
          {engine.host} · {engine.input_device} · {engine.input_format}/{engine.output_format} ·{" "}
          {engine.driver_input_frames ?? engine.buffer_frames} Frames · Latenz {engine.latency_ms.toFixed(2)} ms ·
          höchstens {engine.max_layers} Ebenen je Track · bis {engine.memory_mb.toFixed(0)} MB
        </footer>
      )}
    </div>
  );
}

function Notes({ notes, onClear }: { notes: Note[]; onClear: () => void }) {
  if (notes.length === 0) return null;
  const [newest, ...rest] = notes;
  return (
    <section className="live-notes">
      <div className={`live-note live-note-${newest.kind} live-note-new`} onClick={onClear} title="Klick leert die Liste">
        {newest.text}
      </div>
      {rest.length > 0 && (
        <ul className="live-note-old">
          {rest.map((n) => (
            <li key={n.seq} className={`live-note-${n.kind}`}>
              {n.text}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
