//! The frame: one window, one title bar, two modes and a setup page.
//!
//! ```text
//!  ┌ TitleBar ───────────────────────────────────────────────────────────────┐
//!  ├ bars: input-loss alarm · MIDI learn band (while learning)               ┤
//!  │ Arrangement  |  Live  |  Setup (gear)          [MIDI sidebar, learning]  │
//!  └──────────────────────────────────────────────────── toasts, popovers ───┘
//! ```
//!
//! **Arrangement** is where a song is written (grid, inspector and YAML text of one document;
//! the mixer is a later stage). **Live** is the stage: big, readable from across the room,
//! every control reachable by MIDI. **Setup** is reachable from the gear at any time; with the
//! engine stopped it is where the app opens, and starting the engine from it switches to Live.
//!
//! MIDI learn reaches every view: `LearnLayer` is the one document-level listener that turns a click
//! on any element with a `data-midi` address into a binding, and `.learn-on` on the window lets the
//! CSS mark every learnable control.

import { useCallback, useEffect, useRef, useState } from "react";
import { LiveView } from "./live/LiveView";
import { ArrangementView } from "./arrangement/ArrangementView";
import { SetupView, type SetupPage } from "./setup/SetupView";
import { LearnLayer } from "./midi/LearnLayer";
import { LearnBar, MidiSide } from "./midi/MidiSide";
import { useLearnMode } from "./midi/store";
import { useLooperEvents } from "./useWebSocket";
import { useStatusSlice } from "./status";
import { TitleBar, type Mode } from "./shell/TitleBar";
import { EnginePanel } from "./shell/EnginePanel";
import { Alarm } from "./shell/Toasts";
import { fail } from "./shell/notes";
import { useScoreKeys } from "./shell/scoreTransport";
import { useScoreModelSync } from "./shell/scoreModel";
import { useWindowTitle } from "./shell/windowTitle";
import { IS_MAC } from "./platform";

const MODE_KEY = "looper.mode";

function storedMode(): Mode {
  try {
    return localStorage.getItem(MODE_KEY) === "arrangement" ? "arrangement" : "live";
  } catch {
    return "live";
  }
}

export default function App() {
  const { info, error } = useLooperEvents();
  const running = useStatusSlice((s) => s.running);
  const learn = useLearnMode();
  const [mode, setMode] = useState<Mode>(storedMode);
  // The app opens on Setup while the engine is down - that is where the first thing to do is.
  const [setupOpen, setSetupOpen] = useState(!running);
  const [page, setPage] = useState<SetupPage>("audio");
  const [enginePanel, setEnginePanel] = useState(false);

  useScoreModelSync();
  useWindowTitle();
  // Space / N start or release, Shift+S stops - in every mode: the arrangement has no transport of
  // its own any more, the title bar is the transport.
  useScoreKeys(true);

  useEffect(() => {
    if (error) fail(error);
  }, [error]);

  // Development only: the autopilot (dev/autopilot.ts) switches views through this event.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const onPilot = (e: Event) => {
      const step = (e as CustomEvent<Record<string, unknown>>).detail;
      if (step.do === "mode") chooseModeRef.current(step.to === "arrangement" ? "arrangement" : "live");
      const d = step.detail as { setup?: SetupPage; engine?: boolean } | undefined;
      if (d?.setup) {
        setPage(d.setup);
        setSetupOpen(true);
      }
      if (d?.engine !== undefined) setEnginePanel(d.engine);
    };
    window.addEventListener("looper-autopilot", onPilot);
    return () => window.removeEventListener("looper-autopilot", onPilot);
  }, []);

  const chooseMode = useCallback((m: Mode) => {
    setMode(m);
    setSetupOpen(false);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* not worth an error */
    }
  }, []);

  const chooseModeRef = useRef(chooseMode);
  chooseModeRef.current = chooseMode;

  const openSetup = useCallback(() => {
    setEnginePanel(false);
    setSetupOpen(true);
  }, []);

  // While learning: the band under the title bar and the sidebar next to whatever is open
  // (design/midi-learn.html). On Setup → MIDI the page itself is the sidebar, so it is not doubled.
  const showMidiSide = learn && !(setupOpen && page === "midi");

  return (
    <div className={`window${learn ? " learn-on" : ""}${IS_MAC ? " mac" : ""}`}>
      <TitleBar
        mode={mode}
        setupOpen={setupOpen}
        onMode={chooseMode}
        onSetup={() => (setupOpen ? setSetupOpen(false) : openSetup())}
        enginePanelOpen={enginePanel}
        onEnginePanel={() => setEnginePanel((o) => !o)}
      />

      <div className="bars">
        <Alarm />
        {learn && <LearnBar mode={setupOpen ? "setup" : mode} />}
      </div>

      <div className={`content${showMidiSide ? " with-midi" : ""}`}>
        {setupOpen ? (
          <SetupView info={info} page={page} onPage={setPage} onStarted={() => chooseMode("live")} />
        ) : mode === "live" ? (
          <LiveView onOpenSetup={openSetup} onOpenArrangement={() => chooseMode("arrangement")} />
        ) : (
          <ArrangementView />
        )}
        {showMidiSide && <MidiSide />}
      </div>

      {enginePanel && <EnginePanel onClose={() => setEnginePanel(false)} onSetup={openSetup} />}
      <LearnLayer />
    </div>
  );
}
