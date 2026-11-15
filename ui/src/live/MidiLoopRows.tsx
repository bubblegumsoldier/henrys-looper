//! What a take of an instrument track stores - its sound or its notes - and, for a MIDI loop, the
//! note grid it is played on. Shown in the track drawer of an instrument track.
//!
//! The loop type only changes while the track is empty (the engine refuses otherwise). The grid acts
//! at once on every layer, the recorded ones included, and never changes what was played: "Off"
//! brings the notes back to where they were.

import { api } from "../api";
import { setConfig } from "../setup/config";
import { run } from "../shell/notes";
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

/** Keep the stored setup in step, so the next start opens the track the same way. */
function remember(name: string, change: Partial<StartInstrument>) {
  setConfig((c: StartConfig) => ({
    ...c,
    tracks: c.tracks.map((t) => (t.name === name && t.instrument ? { ...t, instrument: { ...t.instrument, ...change } } : t)),
  }));
}

export function MidiLoopRows({ track }: { track: LooperTrackStatus }) {
  const i = track.index;
  const midi = !!track.midi_loop;
  const empty = track.state === "empty" && track.layers.length === 0;
  const setKind = (kind: "audio" | "midi") =>
    run(async () => {
      await api.trackLoopKind(i, kind);
      remember(track.name, { loop: kind === "midi" ? "midi" : null });
    });
  const setGrid = (grid: string) =>
    run(async () => {
      await api.trackGrid(i, grid);
      remember(track.name, { grid: grid === "none" ? null : grid });
    });
  return (
    <>
      <div className="frow">
        <label>{T.loopKind}</label>
        <div>
          <div className={`seg${empty ? "" : " off"}`} style={{ "--ctl-h": "22px" } as React.CSSProperties}>
            {(["audio", "midi"] as const).map((k) => (
              <button
                key={k}
                className={(k === "midi") === midi ? "on" : ""}
                disabled={!empty}
                onClick={() => (k === "midi") !== midi && setKind(k)}
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
              onChange={(e) => setGrid(e.target.value)}
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
