//! Everything the engine counts that should be zero.
//!
//! `fifo_overruns` is not one warning among others. It means input samples were dropped, so every
//! recording made afterwards sits at the wrong place in the loop and will never line up again. It
//! gets its own banner and says what to do; the rest are small badges that appear only when they
//! are not zero.

import { useStatusSlice } from "../status";
import type { LooperStatus } from "../types";

interface Counters {
  xruns: number;
  underruns: number;
  overruns: number;
  others: number;
  ignored: number;
  maxCallbackMs: number;
}

function pick(s: LooperStatus): Counters {
  return {
    xruns: s.xruns,
    underruns: s.fifo_underruns,
    overruns: s.fifo_overruns,
    others: s.other_errors,
    ignored: s.ignored_commands,
    // Rounded, so a new record of a tenth of a millisecond does not cause a render.
    maxCallbackMs: Math.round(s.max_callback_ms * 10) / 10,
  };
}

function same(a: Counters, b: Counters): boolean {
  return (
    a.xruns === b.xruns &&
    a.underruns === b.underruns &&
    a.overruns === b.overruns &&
    a.others === b.others &&
    a.ignored === b.ignored &&
    a.maxCallbackMs === b.maxCallbackMs
  );
}

export function Warnings() {
  const c = useStatusSlice(pick, same);
  const quiet = c.xruns === 0 && c.underruns === 0 && c.others === 0;

  return (
    <>
      {c.overruns > 0 && (
        <div className="live-alarm" role="alert">
          <div className="live-alarm-head">Eingangssamples verloren ({c.overruns}×)</div>
          <div className="live-alarm-body">
            Jede Aufnahme ab hier liegt dauerhaft verschoben im Loop. Engine stoppen, neu starten,
            Aufnahmen wiederholen. Grössere Puffer helfen gegen die Ursache.
          </div>
        </div>
      )}
      <div className="live-counters">
        {quiet ? (
          <span className="live-counter live-counter-ok">keine Aussetzer</span>
        ) : (
          <>
            {c.xruns > 0 && <span className="live-counter live-counter-bad">Aussetzer {c.xruns}</span>}
            {c.underruns > 0 && <span className="live-counter live-counter-warn">FIFO leer {c.underruns}</span>}
            {c.others > 0 && <span className="live-counter live-counter-bad">Stromfehler {c.others}</span>}
          </>
        )}
        {c.ignored > 0 && <span className="live-counter">abgelehnt {c.ignored}</span>}
        <span className="live-counter">Callback max {c.maxCallbackMs.toFixed(1)} ms</span>
      </div>
    </>
  );
}
