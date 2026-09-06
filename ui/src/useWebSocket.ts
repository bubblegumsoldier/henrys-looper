//! The bridge from Tauri's events into the status hub.
//!
//! This used to be a WebSocket to a Python backend, with exponential-backoff reconnects. There is
//! nothing left to reconnect to: the engine runs in this very process and pushes through
//! `app.emit`, so a listener either exists or the app is not running inside Tauri at all. The
//! reconnect machinery is gone on purpose.
//!
//! `looper://ready` is emitted once during setup and can therefore be missed by a frontend that
//! starts listening a moment later - so `app_info` is called as well and whichever arrives first
//! wins.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, insideTauri } from "./api";
import { pushStatus } from "./status";
import { pushMidiFeed } from "./midi/store";
import type { MidiFeed } from "./midi/types";
import type { AppInfo, LooperStatus } from "./types";

/** Kept for the components written against the old WebSocket indicator. */
export type WsStatus = "connecting" | "open" | "closed";

export interface Bridge {
  status: WsStatus;
  /** Version, log file and the limits of this build; null until it has been answered. */
  info: AppInfo | null;
  /** Set when the Rust side could not be reached at all - a browser instead of the app window. */
  error: string | null;
}

/** Mount exactly once, at the root. */
export function useLooperEvents(): Bridge {
  const [status, setStatus] = useState<WsStatus>("connecting");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    const unlisten: Array<() => void> = [];

    (async () => {
      try {
        const offStatus = await listen<LooperStatus>("looper://status", (e) => pushStatus(e.payload));
        const offReady = await listen<AppInfo>("looper://ready", (e) => setInfo(e.payload));
        // MIDI has its own event and its own hub: a queue of discrete facts next to a sampling of a
        // continuous state. See `midi/store.ts`.
        const offMidi = await listen<MidiFeed>("looper://midi", (e) => pushMidiFeed(e.payload));
        if (disposed) {
          offStatus();
          offReady();
          offMidi();
          return;
        }
        unlisten.push(offStatus, offReady, offMidi);
        setStatus("open");
      } catch (e) {
        if (disposed) return;
        setStatus("closed");
        setError(
          insideTauri()
            ? `Die Rust-Seite meldet sich nicht: ${(e as Error).message}`
            : "Diese Oberfläche läuft nicht im Anwendungsfenster, sondern im Browser - dort gibt es keine Engine. Starte sie mit `cargo tauri dev`.",
        );
        return;
      }

      // `looper://ready` fires once during startup and may already be gone. Ask for the same facts.
      try {
        const fetched = await api.appInfo();
        if (!disposed) setInfo((current) => current ?? fetched);
      } catch (e) {
        if (!disposed) setError((current) => current ?? (e as Error).message);
      }
    })();

    return () => {
      disposed = true;
      for (const off of unlisten) off();
    };
  }, []);

  return { status, info, error };
}
