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
import type { LooperStatus } from "../types";
import { SCORE_STATE, STATE_WORD } from "../shell/terms";
import { trackKind } from "../live/TrackTile";

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
        <span className="muted">Levels and layers while the score runs</span>
      </div>

      {!status.running ? (
        <div className="blocks-empty">
          The engine is not running. Start it (Live or Setup) - only then can a score be loaded.
        </div>
      ) : (
        <>
          <div className="score-live-master">
            <Meter label="Main L" kind="out" pick={masterLeft} />
            <Meter label="Main R" kind="out" pick={masterRight} />
          </div>
          <ul className="score-live-tracks">
            {status.tracks.map((track) => {
              const target = targets[track.index];
              return (
                <li key={track.index} className={`score-live-track state-${track.state}`}>
                  <div className="score-live-head">
                    <span className="score-live-name">{track.name}</span>
                    {target && (
                      <span className={`state state-${target.state}`} title="Target: what the score asks for here">
                        Target {SCORE_STATE[target.state]}
                      </span>
                    )}
                    <span className={`live-state state-badge-${track.state}`} title="What the engine really does">
                      {STATE_WORD[trackKind(track)]}
                    </span>
                  </div>
                  <Meter label="In" kind="in" pick={(s) => s.tracks[track.index]?.input_peak ?? 0} />
                  <Meter label="Out" kind="out" pick={(s) => s.tracks[track.index]?.output_peak ?? 0} />
                  <div className="score-live-layers mono muted">
                    {track.layers.length === 0
                      ? "no layer"
                      : `${track.layers.length} layer${track.layers.length === 1 ? "" : "s"}` +
                        (track.monitor ? " · monitor" : "")}
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
