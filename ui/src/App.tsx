//! The shell: one event bridge, two views.
//!
//! `Live` is the looper - device setup while the engine is down, the stage view while it runs.
//! `Partitur` is the score: the editor, the block preview and the transport of the runner.
//!
//! **Two tabs, and each borrows a little of the other.** The two views answer different questions -
//! "how does it sound" and "what does the song say" - and while a score plays a musician needs
//! both. A third combined tab would be a third thing to choose on stage; a split screen would halve
//! exactly the two elements that have to carry across a room, the release button and the meters. So
//! each view keeps its own subject at full size and shows the other one compressed: the live view
//! gets the score transport as a strip above its track cards, the score view gets the meters and
//! layer counts as a column beside the editor. Whichever tab is open, the song can be run and the
//! levels can be watched; the tab only decides what is available in *detail*.

import { useState } from "react";
import { LiveView } from "./live/LiveView";
import { ScoreView } from "./components/ScoreView";
import { LearnLayer } from "./midi/LearnLayer";
import { MidiPanel } from "./midi/MidiPanel";
import { useLooperEvents } from "./useWebSocket";

type Tab = "live" | "score";

export default function App() {
  const [tab, setTab] = useState<Tab>("live");
  const { status, info, error } = useLooperEvents();

  return (
    <div className="app">
      <header className="app-bar">
        <div className="brand">Henrys Looper</div>
        <nav className="tabs">
          <button className={`tab${tab === "live" ? " tab-on" : ""}`} onClick={() => setTab("live")}>
            Live
          </button>
          <button className={`tab${tab === "score" ? " tab-on" : ""}`} onClick={() => setTab("score")}>
            Partitur
          </button>
        </nav>
        <div className="spacer" />
        <span className="ws" title={info ? `Logdatei: ${info.log_path}` : undefined}>
          <span className={`dot ${status === "open" ? "dot-ok" : status === "connecting" ? "dot-warn" : "dot-err"}`} />
          {status === "open" ? "verbunden" : status === "connecting" ? "verbinde…" : "getrennt"}
        </span>
        {info && (
          <span className="muted mono app-bar-info">
            v{info.version}
            {info.asio_built ? " · ASIO" : " · ohne ASIO"}
            {info.logging ? "" : " · kein Log"}
          </span>
        )}
      </header>

      {error && (
        <div className="banner" role="alert">
          {error}
        </div>
      )}

      {/* MIDI sits above both views rather than inside one: the learn mode has to reach the score
          transport as well as the track cards, and „hängt der Controller noch?“ is a question one
          asks in whichever tab is open. `LearnLayer` renders nothing - it is the one listener that
          turns a click on a control into a binding. */}
      <LearnLayer />
      <MidiPanel />

      {tab === "live" ? <LiveView info={info} /> : <ScoreView />}
    </div>
  );
}
