//! What is actually arriving, and what the mapping makes of it.
//!
//! This is the tool the controller is *set up* with, and it exists because the manual of a control
//! surface is usually wrong or missing: every pad bank of an MPD218 sends different notes, and
//! `examples/midi-mpd218.yaml` says in its own header that its numbers are guessed. One press here
//! settles it - `Pad 36`, and next to it what that pad currently does.
//!
//! Four columns, in the order the question is asked: which control, what kind of message, what
//! value, and what came of it. The last one is the reason this is not just a byte log: „nicht
//! belegt“ and „abgelehnt: der Runner hat den Transport“ look identical on a pad that does nothing,
//! and they need completely different fixes.

import { clearMidiEvents, useMidiEvents } from "./store";
import type { MidiEventReport, MidiOutcome } from "./types";

/** German word for each outcome, and the class that colours the row. */
const OUTCOME: Record<MidiOutcome, { label: string; className: string }> = {
  unbound: { label: "nicht belegt", className: "midi-out-unbound" },
  absorbed: { label: "geschluckt", className: "midi-out-absorbed" },
  action: { label: "ausgeführt", className: "midi-out-action" },
  refused: { label: "abgelehnt", className: "midi-out-refused" },
  learned: { label: "gelernt", className: "midi-out-learned" },
};

function Row({ event }: { event: MidiEventReport }) {
  const outcome = OUTCOME[event.outcome];
  return (
    <li className={`midi-event ${outcome.className}`}>
      <span className="midi-event-key mono">{event.short}</span>
      <span className="midi-event-what">{event.what}</span>
      <span className="midi-event-channel mono" title="MIDI-Kanal">
        K{event.channel}
      </span>
      <span className="midi-event-value mono">{event.value}</span>
      <span className="midi-event-target">
        {event.target_label ?? <span className="muted">—</span>}
        {event.note && <span className="midi-event-note">{event.note}</span>}
      </span>
      <span className={`midi-event-outcome ${outcome.className}`}>{outcome.label}</span>
    </li>
  );
}

export function MidiMonitor() {
  const events = useMidiEvents();

  return (
    <section className="midi-monitor">
      <div className="midi-monitor-head">
        <h3>Eingehend</h3>
        <span className="muted">
          {events.length === 0
            ? "Ein Pad drücken oder einen Regler drehen — hier steht, was ankommt."
            : `${events.length} zuletzt`}
        </span>
        <div className="spacer" />
        <button className="btn btn-mini" onClick={clearMidiEvents} disabled={events.length === 0}>
          Leeren
        </button>
      </div>
      {events.length > 0 && (
        <ul className="midi-events">
          {events.map((event) => (
            <Row key={event.seq} event={event} />
          ))}
        </ul>
      )}
    </section>
  );
}
