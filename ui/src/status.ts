//! The status hub: one place that holds the newest `looper://status`, and the hooks that let a
//! component watch exactly the part of it that it draws.
//!
//! **Why this exists.** The engine pushes a full status twenty times a second. Putting that into
//! `useState` would rebuild the whole tree eighty times for every bar of music, most of it to
//! redraw numbers that did not change. So the event never becomes React state as a whole:
//!
//! * `subscribe()` is the raw firehose. Meters and the beat pulse use it and write into the DOM
//!   through a ref - no render at all.
//! * `useStatusSlice(select, equal)` renders only when the selected part actually differs. The
//!   track and layer structure uses a signature string for `select`, so a card is rebuilt when a
//!   state, a name, a layer count or a mute changes, and never because a peak moved.
//!
//! Nothing in here is React-specific except the hooks at the bottom, and nothing allocates per
//! event apart from what a selector asks for.

import { useEffect, useRef, useState } from "react";
import { IDLE_STATUS, type LooperStatus } from "./types";

type Listener = (status: LooperStatus) => void;

const listeners = new Set<Listener>();
let latest: LooperStatus = IDLE_STATUS;

/** The newest status. Safe to read at any time, including during a render. */
export function currentStatus(): LooperStatus {
  return latest;
}

/** Feed a freshly arrived event in. Called by the event bridge, and by nothing else. */
export function pushStatus(status: LooperStatus): void {
  latest = status;
  for (const l of listeners) l(status);
}

// A way to drive the whole live view from the console while `npm run dev` runs, so the stage
// screen can be looked at without a device and without making a sound. Stripped from the build.
if (import.meta.env.DEV) {
  (window as unknown as { pushStatus?: typeof pushStatus }).pushStatus = pushStatus;
}

/** Watch every event. Returns the unsubscribe function. */
export function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------------------------

/**
 * Render only when `select(status)` changes. `equal` defaults to `Object.is`, which is right for
 * numbers, strings and booleans; pass one when the selector builds an object.
 */
export function useStatusSlice<T>(
  select: (status: LooperStatus) => T,
  equal: (a: T, b: T) => boolean = Object.is,
): T {
  const selectRef = useRef(select);
  selectRef.current = select;
  const equalRef = useRef(equal);
  equalRef.current = equal;

  const [value, setValue] = useState<T>(() => select(latest));
  const valueRef = useRef(value);

  useEffect(() => {
    const apply = (status: LooperStatus) => {
      const next = selectRef.current(status);
      if (!equalRef.current(valueRef.current, next)) {
        valueRef.current = next;
        setValue(next);
      }
    };
    // Catch up with whatever arrived between the first render and this effect.
    apply(latest);
    return subscribe(apply);
  }, []);

  return value;
}

/**
 * Run `effect` on every single event without rendering. For meters and the beat pulse, which write
 * a style straight into the DOM. The callback is kept in a ref, so it may be a fresh closure on
 * every render without resubscribing.
 */
export function useStatusEffect(effect: Listener): void {
  const ref = useRef(effect);
  ref.current = effect;
  useEffect(() => {
    const run: Listener = (status) => ref.current(status);
    run(latest);
    return subscribe(run);
  }, []);
}

// ---------------------------------------------------------------------------------------------
// Selectors that need a stable definition, because several components share them
// ---------------------------------------------------------------------------------------------

/**
 * Everything about the tracks that changes the layout: names, channels, states, layer count and
 * every layer's mute and gain. Peaks and positions are deliberately absent - they move constantly
 * and are drawn imperatively.
 */
export function structureSignature(status: LooperStatus): string {
  let sig = status.running ? "1" : "0";
  for (const t of status.tracks) {
    sig += `|${t.index}${t.name}${t.input_channel}${t.state}${t.monitor}${t.playing}${t.loop_samples}`;
    // The count-in belongs in the signature: it only changes on a beat line, so it costs a handful
    // of renders per bar rather than twenty a second - and a countdown that does not count is
    // useless.
    sig += `~${t.pending_kind ?? ""}~${t.pending_bars}~${t.pending_beats}`;
    for (const l of t.layers) sig += `${l.index}${l.muted ? 1 : 0}${l.gain}`;
  }
  return sig;
}
