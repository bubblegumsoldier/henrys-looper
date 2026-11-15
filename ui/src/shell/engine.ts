//! Starting and stopping the engine, from wherever the button is: the Setup page, the empty Live
//! view, or the engine popover in the title bar. One store, so all three agree on "busy" and on what
//! the driver actually opened.

import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, insideTauri } from "../api";
import { fail, note } from "./notes";
import type { EngineInfo, StartConfig } from "../types";

interface EngineState {
  info: EngineInfo | null;
  busy: boolean;
}

let state: EngineState = { info: null, busy: false };
const listeners = new Set<() => void>();

function set(next: Partial<EngineState>): void {
  state = { ...state, ...next };
  for (const l of listeners) l();
}

export function useEngine(): EngineState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}

// Loading a score with other tracks rebuilds the engine on the Rust side; it announces the new
// device facts on `looper://engine`, and every view showing them follows.
if (insideTauri()) {
  void listen<EngineInfo>("looper://engine", (e) => set({ info: e.payload }));
  // A reload of the page (or a dev hot reload) while the engine keeps running would otherwise lose
  // the device facts until the next start: the status pill showed "48 kHz" without the buffer.
  api
    .engineInfo()
    .then((info) => {
      if (info && !state.info) set({ info });
    })
    .catch(() => undefined);
}

export function startEngine(config: StartConfig): Promise<boolean> {
  set({ busy: true });
  return api
    .engineStart(config)
    .then((opened) => {
      set({ info: opened });
      note(
        `Engine running: ${opened.host}, ${opened.input_device}, ${opened.sample_rate} Hz, ` +
          `${opened.driver_input_frames ?? opened.buffer_frames} frames, latency ${opened.latency_ms.toFixed(2)} ms.`,
      );
      return true;
    })
    .catch((e) => {
      fail((e as Error).message);
      return false;
    })
    .finally(() => set({ busy: false }));
}

export function stopEngine(): void {
  set({ busy: true });
  api
    .engineStop()
    .then(() => set({ info: null }))
    .catch((e) => fail((e as Error).message))
    .finally(() => set({ busy: false }));
}
