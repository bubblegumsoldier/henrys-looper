//! The one place the engine speaks up, as a band under the title bar - only when there is
//! something to decide. Running normally it says nothing at all; the status pill is enough.
//!
//! * Monitoring muted: the output is a pair of built-in speakers and a monitored microphone would
//!   feed back. "Unmute anyway", or click it away.
//! * Waiting for an interface that is not plugged in: it starts by itself once it is; "Use … now"
//!   takes the default device instead.
//! * The start failed: the reason, "Retry", "Setup" - and the way to the privacy settings when
//!   macOS refused the microphone.
//! * The setup changed in something only a restart applies: "Restart now" / "Later". With loops
//!   recorded the band says that they go.

import { useState } from "react";
import { api } from "../api";
import { currentConfig } from "../setup/config";
import { useStatusSlice } from "../status";
import { Icon } from "./Icon";
import { fail } from "./notes";
import { restartEngine, startEngine, unmuteMonitoring, useEngine, useFallbackDevice } from "./engine";
import { T } from "./terms";

export function EngineBanner({ onSetup }: { onSetup: () => void }) {
  const { view, busy } = useEngine();
  const muted = useStatusSlice((s) => s.monitor_muted);
  const loops = useStatusSlice((s) => s.running && s.tracks.some((t) => t.layers.length > 0));
  const [dismissed, setDismissed] = useState<string | null>(null);

  const key = muted ? "muted" : view?.phase === "running" && view.restart_needed ? `restart:${loops}` : `${view?.phase}:${view?.message ?? ""}`;
  if (dismissed === key) return null;
  const later = () => setDismissed(key);

  if (muted) {
    return (
      <div className="ebanner warn" role="alert">
        <Icon name="warn" />
        <span className="grow">{T.monitorMutedSpeakers}</span>
        <button className="btn s" onClick={unmuteMonitoring}>
          {T.unmuteAnyway}
        </button>
        <button className="btn s sq ghost" onClick={later} title="Dismiss - monitoring stays muted" aria-label="Dismiss">
          <Icon name="x" />
        </button>
      </div>
    );
  }
  if (!view) return null;

  if (view.phase === "waiting") {
    return (
      <div className="ebanner calm" role="status">
        <Icon name="power" />
        <span className="grow">{view.message}</span>
        {view.alternative && (
          <button className="btn s" disabled={busy} onClick={() => void useFallbackDevice()}>
            Use {view.alternative} now
          </button>
        )}
        <button className="btn s" onClick={later} title="Keep waiting - the engine starts when the interface is plugged in">
          Wait for it
        </button>
        <button className="btn s ghost" onClick={onSetup}>
          <Icon name="gear" />
          {T.setup}
        </button>
      </div>
    );
  }

  if (view.phase === "failed") {
    return (
      <div className="ebanner err" role="alert">
        <Icon name="warn" />
        <span className="grow">{view.message}</span>
        {view.permission && (
          <button className="btn s" onClick={() => void api.openMicSettings().catch((e) => fail((e as Error).message))}>
            Open privacy settings
          </button>
        )}
        <button className="btn s" disabled={busy} onClick={() => void startEngine(currentConfig())}>
          {busy ? "Starting…" : "Retry"}
        </button>
        <button className="btn s ghost" onClick={onSetup}>
          <Icon name="gear" />
          {T.setup}
        </button>
      </div>
    );
  }

  if (view.phase === "running" && view.restart_needed) {
    return (
      <div className={`ebanner ${loops ? "warn" : "calm"}`} role="status">
        <Icon name="power" />
        <span className="grow">
          {loops ? T.restartClearsLoops : "The setup changed - the engine restarts to apply it."}
        </span>
        <button className="btn s" disabled={busy} onClick={() => void restartEngine()}>
          {busy ? "Restarting…" : "Restart now"}
        </button>
        <button className="btn s" onClick={later}>
          Later
        </button>
      </div>
    );
  }
  return null;
}
