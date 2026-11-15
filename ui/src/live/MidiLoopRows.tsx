//! What a take of an instrument track stores - its sound or its notes - and, for a MIDI loop, the
//! note grid it is played on. Shown in the track drawer of an instrument track.
//!
//! The loop type only changes while the track is empty (the engine refuses otherwise). The grid acts
//! at once on every layer, the recorded ones included, and never changes what was played: "Off"
//! brings the notes back to where they were.

import { api } from "../api";
import { setConfig } from "../setup/config";
import { fail, run } from "../shell/notes";
import { T } from "../shell/terms";
import type { LooperTrackStatus, StartConfig, StartInstrument } from "../types";

export const GRID_CHOICES: { value: string; label: string }[] = [
  { value: "none", label: T.gridNone },
  { value: "1/4", label: "1/4" },
  { value: "1/8", label: "1/8" },
  { value: "1/16", label: "1/16" },
  { value: "1/8t", label: "1/8 T" },
  { value: "1/16t", label: "1/16 T" },
];

/** −12 / value / +12: semitones an instrument track's live notes are shifted by. */
export function TransposeControl({
  value,
  disabled,
  onChange,
}: {
  value: number;
  disabled?: boolean;
  onChange: (semitones: number) => void;
}) {
  const set = (v: number) => {
    const c = Math.max(-48, Math.min(48, Math.round(v)));
    if (c !== value) onChange(c);
  };
  return (
    <span style={{ display: "inline-flex", gap: 4, alignItems: "center" }}>
      <button className="btn s" disabled={disabled || value <= -48} onClick={() => set(value - 12)} title="One octave down">
        −12
      </button>
      <input
        className="field num"
        style={{ width: 52, textAlign: "center" }}
        type="number"
        min={-48}
        max={48}
        value={value}
        disabled={disabled}
        title="Semitones the notes are shifted by before they reach the instrument (−48 to 48). The key range applies to the notes as they come in."
        onChange={(e) => set(Number(e.target.value) || 0)}
      />
      <button className="btn s" disabled={disabled || value >= 48} onClick={() => set(value + 12)} title="One octave up">
        +12
      </button>
    </span>
  );
}

/** Keep the stored setup in step, so the next start opens the track the same way. */
function remember(name: string, change: Partial<StartInstrument>) {
  setConfig((c: StartConfig) => ({
    ...c,
    tracks: c.tracks.map((t) => (t.name === name && t.instrument ? { ...t, instrument: { ...t.instrument, ...change } } : t)),
  }));
}

type TrackLike = Pick<LooperTrackStatus, "index" | "name" | "state" | "layers" | "midi_loop">;

/**
 * Why the loop type of this track cannot change right now, or null when it can. The engine only
 * takes the change on an empty track; the sentence says what is in the way.
 */
export function loopKindBlock(track: TrackLike): string | null {
  const layers = track.layers.length;
  switch (track.state) {
    case "empty":
      return layers === 0 ? null : `"${track.name}" still holds ${layers} ${layers === 1 ? "layer" : "layers"}.`;
    case "armed":
      return `"${track.name}" is waiting for a take to start.`;
    case "recording":
    case "overdub":
      return `"${track.name}" is recording.`;
    default:
      return `"${track.name}" holds a loop (${layers} ${layers === 1 ? "layer" : "layers"}).`;
  }
}

/** Switch what a take of an instrument track stores. Refused (with a note) while it holds a loop. */
export function setLoopKind(track: TrackLike, kind: "audio" | "midi"): void {
  if ((kind === "midi") === !!track.midi_loop) return;
  const block = loopKindBlock(track);
  if (block) {
    fail(`Clear the track first: ${block} The loop type changes only while the track is empty.`);
    return;
  }
  run(async () => {
    await api.trackLoopKind(track.index, kind);
    remember(track.name, { loop: kind === "midi" ? "midi" : null });
  });
}

export function setNoteGrid(track: TrackLike, grid: string): void {
  run(async () => {
    await api.trackGrid(track.index, grid);
    remember(track.name, { grid: grid === "none" ? null : grid });
  });
}

export function setTranspose(track: TrackLike, semitones: number): void {
  run(async () => {
    await api.trackTranspose(track.index, semitones);
    remember(track.name, { transpose: semitones === 0 ? null : semitones });
  });
}

export function MidiLoopRows({ track }: { track: LooperTrackStatus }) {
  const midi = !!track.midi_loop;
  const empty = loopKindBlock(track) === null;
  return (
    <>
      <div className="frow">
        <label>{T.transpose}</label>
        <div>
          <TransposeControl value={track.transpose ?? 0} onChange={(v) => setTranspose(track, v)} />
        </div>
      </div>
      <div className="frow">
        <label>{T.loopKind}</label>
        <div>
          <div className={`seg${empty ? "" : " off"}`} style={{ "--ctl-h": "22px" } as React.CSSProperties}>
            {(["audio", "midi"] as const).map((k) => (
              <button
                key={k}
                className={(k === "midi") === midi ? "on" : ""}
                aria-disabled={!empty}
                onClick={() => setLoopKind(track, k)}
                title={
                  empty
                    ? k === "midi"
                      ? "Takes store the notes and play them through the instrument"
                      : "Takes store the instrument's sound"
                    : "The loop type changes only while the track is empty - clear it first"
                }
              >
                {k === "midi" ? T.loopMidi : T.loopAudio}
              </button>
            ))}
          </div>
        </div>
      </div>
      {midi && (
        <div className="frow">
          <label>{T.noteGrid}</label>
          <div>
            <select
              className="field sel"
              value={track.grid ?? "none"}
              onChange={(e) => setNoteGrid(track, e.target.value)}
              title="Notes play on this grid - also the ones already recorded. The recording itself never changes."
            >
              {GRID_CHOICES.map((g) => (
                <option key={g.value} value={g.value}>
                  {g.label}
                </option>
              ))}
            </select>
          </div>
        </div>
      )}
    </>
  );
}
