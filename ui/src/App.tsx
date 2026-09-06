//! The shell: one event bridge, two views.
//!
//! `Live` is the looper - device setup while the engine is down, the stage view while it runs.
//! `Partitur` is the YAML editor from the Ableton days; it is kept but has nothing to talk to
//! until phase 3 brings the compiler.

import { useState } from "react";
import { LiveView } from "./live/LiveView";
import { ScoreView } from "./components/ScoreView";
import { useLooperEvents } from "./useWebSocket";
import { SCORE_HINT } from "./api";

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
          <button
            className={`tab${tab === "score" ? " tab-on" : ""}`}
            onClick={() => setTab("score")}
            title={SCORE_HINT}
          >
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

      {tab === "live" ? <LiveView info={info} /> : <ScoreView />}
    </div>
  );
}
