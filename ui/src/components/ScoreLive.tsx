//! The live strip inside the score view: the levels and the layers, small.
//!
//! **Why this exists at all.** The two views answer two different questions - "what does the song
//! say" and "how does it sound" - and a musician running a score needs both at once. A third tab
//! would make him choose a third time; a split screen would halve the two things that have to be
//! readable from several metres (the release button and the meters). So each view keeps its own
//! subject at full size and borrows the other one small: the score view gets this strip, and the
//! live view gets the compact transport (see `LiveView`). Either tab is then usable on stage, and
//! the choice is only about what one wants in *detail*.
//!
//! What it shows per track is exactly the CLI's `Soll | Ist`: what the score asks of the track next
//! to what the engine is really doing with it. Those two disagreeing for more than a moment is the
//! first sign that something is wrong, and it is invisible in either half alone.

import { useMemo } from "react";
import { Meter } from "../live/Meter";
import { currentStatus, useStatusSlice } from "../status";
import { SCORE_STATE_LABEL, type LooperStatus } from "../types";

const masterLeft = (s: LooperStatus) => s.output_peaks?.[0] ?? 0;
const masterRight = (s: LooperStatus) => s.output_peaks?.[1] ?? 0;

/** Names, states, layer counts and the score's target per track - everything but the levels. */
function signature(s: LooperStatus): string {
  let sig = s.running ? "1" : "0";
  for (const t of s.tracks) sig += `|${t.name}~${t.state}~${t.layers.length}~${t.monitor ? 1 : 0}`;
  for (const t of s.score?.tracks ?? []) sig += `>${t.state}`;
  return sig;
}

export function ScoreLive() {
  const sig = useStatusSlice(signature);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const status = useMemo(() => currentStatus(), [sig]);
  const targets = status.score?.tracks ?? [];

  return (
    <section className="score-live">
      <div className="panel-title">
        Live
        <span className="muted">Pegel und Ebenen, während die Partitur läuft</span>
      </div>

      {!status.running ? (
        <div className="blocks-empty">
          Die Engine läuft nicht. Im Reiter „Live" starten — erst dann lässt sich eine Partitur laden.
        </div>
      ) : (
        <>
          <div className="score-live-master">
            <Meter label="Summe L" kind="out" pick={masterLeft} />
            <Meter label="Summe R" kind="out" pick={masterRight} />
          </div>
          <ul className="score-live-tracks">
            {status.tracks.map((track) => {
              const target = targets[track.index];
              return (
                <li key={track.index} className={`score-live-track state-${track.state}`}>
                  <div className="score-live-head">
                    <span className="score-live-name">{track.name}</span>
                    {target && (
                      <span className={`state state-${target.state}`} title="Soll: was die Partitur hier verlangt">
                        Soll {SCORE_STATE_LABEL[target.state]}
                      </span>
                    )}
                    <span className={`live-state state-badge-${track.state}`} title="Was die Engine wirklich tut">
                      {track.state_label}
                    </span>
                  </div>
                  <Meter label="Ein" kind="in" pick={(s) => s.tracks[track.index]?.input_peak ?? 0} />
                  <Meter label="Aus" kind="out" pick={(s) => s.tracks[track.index]?.output_peak ?? 0} />
                  <div className="score-live-layers mono muted">
                    {track.layers.length === 0
                      ? "keine Ebene"
                      : `${track.layers.length} Ebene${track.layers.length === 1 ? "" : "n"}` +
                        (track.monitor ? " · mithören" : "")}
                  </div>
                </li>
              );
            })}
          </ul>
        </>
      )}
    </section>
  );
}
