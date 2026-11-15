//! Recording every run to disk: the commands (`app/src-tauri/src/recording.rs`) and the state the
//! `looper://recording` event carries.
//!
//! Two facts live here: the recording in progress (`active`, refreshed four times a second while one
//! runs) and the newest finished one (`finished`, kept until it is dismissed), so the title bar can
//! offer "Show in Finder" right after a run.

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface ActiveRecording {
  name: string;
  folder: string;
  seconds: number;
  /** The run is over; only the tail is still being written. */
  stopping: boolean;
  mode: "score" | "free";
  overflows: number;
}

export interface FinishedRecording {
  name: string;
  folder: string;
  kept: boolean;
  reason: string | null;
  seconds: number;
  bytes: number;
  overflows: number;
  silent_frames: number;
  error: string | null;
  text: string;
}

/** What arrives on `looper://recording`. */
export interface RecordingEvent {
  active: ActiveRecording | null;
  finished: FinishedRecording | null;
}

export interface RecordingSettings {
  enabled: boolean;
  dry: boolean;
  folder: string;
  default_folder: boolean;
  free_bytes: number | null;
  low_space: boolean;
}

export interface RecordingEntry {
  name: string;
  path: string;
  title: string;
  started_at: string;
  seconds: number;
  bytes: number;
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
  }
}

export const recordingApi = {
  settings: () => call<RecordingSettings>("recording_settings"),
  /** `folder: null` goes back to the default folder under Documents. */
  set: (enabled: boolean, dry: boolean, folder: string | null) =>
    call<RecordingSettings>("recording_set", { enabled, dry, folder }),
  list: () => call<RecordingEntry[]>("recording_list"),
  /** Without a path: the recordings folder itself. */
  reveal: (path?: string) => call<void>("recording_reveal", { path: path ?? null }),
  delete: (path: string) => call<RecordingEntry[]>("recording_delete", { path }),
  stop: () => call<void>("recording_stop"),
};

// ---------------------------------------------------------------------------------------------

export interface RecordingState {
  active: ActiveRecording | null;
  finished: FinishedRecording | null;
}

let state: RecordingState = { active: null, finished: null };
const listeners = new Set<() => void>();

function announce(): void {
  for (const l of listeners) l();
}

/** Feed one event in. Called by the event bridge. */
export function pushRecording(event: RecordingEvent): void {
  state = {
    active: event.active,
    // A progress event says nothing about the last finished run; keep it until a new one replaces
    // it or it is dismissed.
    finished: event.finished ?? (event.active ? null : state.finished),
  };
  announce();
}

/** The state right now, outside React. */
export function recordingState(): RecordingState {
  return state;
}

export function dismissFinished(): void {
  state = { ...state, finished: null };
  announce();
}

export function useRecording(): RecordingState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}

/** "1:05", "12:00", "1:02:03". */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** "120 MB", "1.4 GB". */
export function bytes(n: number): string {
  const mb = n / 1_048_576;
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}
