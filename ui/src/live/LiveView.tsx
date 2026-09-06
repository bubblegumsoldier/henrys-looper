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
//! Keyboard: a digit picks a track, then R O S P M act on it, K toggles the click. No Enter, no
//! modifier - the hands are on an instrument. Anything typed into a field is left alone.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import { Position } from "./Position";
import { Setup } from "./Setup";
import { TrackCard, type TrackActions } from "./TrackCard";
import { Warnings } from "./Warnings";
import { Meter } from "./Meter";
import type { AppInfo, EngineInfo, LooperStatus, StartConfig } from "../types";

interface Tempo {
  bpm: number;
  beatsPerBar: number;
  beatUnit: number;
  bars: number;
  loopSeconds: number;
  click: boolean;
  sampleRate: number;
  latencySamples: number;
}

function pickTempo(s: LooperStatus): Tempo {
  return {
    bpm: s.bpm,
    beatsPerBar: s.beats_per_bar,
    beatUnit: s.beat_unit,
    bars: s.bars,
    loopSeconds: s.loop_seconds,
    click: s.click,
    sampleRate: s.sample_rate,
    latencySamples: s.latency_samples,
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
    a.sampleRate === b.sampleRate &&
    a.latencySamples === b.latencySamples
  );
}

const masterPeak = (s: LooperStatus) => s.output_peak;

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
      layerMute: (t, l, muted) => run(() => api.layerMute(t, l, muted)),
      layerRemove: (t, l) => run(() => api.layerRemove(t, l)),
      layerGain: (t, l, gain) => run(() => api.layerGain(t, l, gain)),
    }),
    [run],
  );

  const tempo = useStatusSlice(pickTempo, sameTempo);

  // --- keyboard ------------------------------------------------------------
  // Kept in refs so the listener is installed once and never sees a stale track list.
  const state = useRef({ tracks, selected, actions, click: tempo.click });
  state.current = { tracks, selected, actions, click: tempo.click };

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

      if (e.key >= "1" && e.key <= "9") {
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
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [running, run]);

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
            {tempo.sampleRate} Hz · {tempo.latencySamples} Samples Kompensation
          </span>
        </div>
        <div className="spacer" />
        <Meter label="Summe" kind="out" pick={masterPeak} />
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

      <div className="live-tracks">
        {tracks.map((t) => (
          <TrackCard
            key={t.index}
            track={t}
            selected={t.index === selected}
            onSelect={setSelected}
            actions={actions}
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
