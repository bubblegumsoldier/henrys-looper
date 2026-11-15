//! Which device of each column is open. One per column (an accordion), remembered per machine like
//! the mixer's layout - a workspace preference, not part of a song. A compact strip row sets it, so
//! clicking "Reverb" on a strip opens the channel panel with the reverb unfolded.

import { useSyncExternalStore } from "react";
import type { DeviceId, Owner } from "./model";

const KEY = "looper.devices.open";

/** `null` in the map: the user closed every device of that column on purpose. */
type OpenMap = Record<Owner, DeviceId | null>;

function load(): OpenMap {
  try {
    return (JSON.parse(localStorage.getItem(KEY) ?? "null") as OpenMap | null) ?? {};
  } catch {
    return {};
  }
}

let state: OpenMap = load();
const listeners = new Set<() => void>();

export function openDevice(owner: Owner, id: DeviceId | null): void {
  if (state[owner] === id) return;
  state = { ...state, [owner]: id };
  try {
    localStorage.setItem(KEY, JSON.stringify(state));
  } catch {
    /* a preference, not worth an error */
  }
  for (const l of listeners) l();
}

/** The open device of a column: what was opened last, `undefined` when nothing ever was. */
export function useOpenDevice(owner: Owner): DeviceId | null | undefined {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state[owner],
  );
}
