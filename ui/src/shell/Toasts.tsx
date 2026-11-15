//! Messages and alarms.
//!
//! * `Alarm`: input samples were lost (`fifo_overruns`). Not one warning among others - every take
//!   recorded afterwards sits at the wrong place in the loop forever - so it is a banner that stays.
//! * `Ticker`: the newest sentence from the engine or the runner, or a refusal, as one line in the
//!   title bar. It never covers a control - on a stage nothing may float over a button. A sentence
//!   fades after a few seconds; an error stays until it is clicked away. Hover shows the last few.

import { useEffect, useState } from "react";
import { useStatusSlice } from "../status";
import { clearNotes, note, useNotes } from "./notes";
import { Icon } from "./Icon";

export function Alarm() {
  const overruns = useStatusSlice((s) => s.fifo_overruns);
  if (overruns === 0) return null;
  return (
    <div className="alarm" role="alert">
      <Icon name="warn" />
      <b>Input samples lost ({overruns}×).</b>
      Every take from here on sits shifted in the loop. Stop the engine, start it again, redo the takes. A larger
      buffer helps against the cause.
    </div>
  );
}

const FADE_MS = 6000;

/**
 * `feed`: this instance turns the engine's sentences into notes. Exactly one ticker does that (the
 * one in the title bar, always mounted); a second one - in the Arrangement's status bar, where the
 * title bar has no room next to the transport - only shows.
 */
export function Ticker({ feed = true }: { feed?: boolean } = {}) {
  const notes = useNotes();
  const [now, setNow] = useState(Date.now());

  // The engine's own sentence about the last thing that happened.
  const message = useStatusSlice((s) => s.message);
  useEffect(() => {
    if (feed && message) note(message, "engine");
  }, [feed, message]);

  useEffect(() => {
    if (notes.length === 0) return;
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [notes.length]);

  const newest = notes[0];
  const shown = newest && (newest.kind === "error" || now - newest.at < FADE_MS) ? newest : null;
  return (
    <button
      className={`ticker${shown ? ` ${shown.kind}` : " idle"}`}
      onClick={clearNotes}
      title={notes.map((n) => n.text).join("\n") || undefined}
      aria-live="polite"
      tabIndex={shown ? 0 : -1}
    >
      {shown && (
        <>
          <Icon name={shown.kind === "error" ? "warn" : "info"} />
          <span>{shown.text}</span>
        </>
      )}
    </button>
  );
}
