//! Messages to the musician: the engine's confirmations, the runner's sentences and every refusal.
//!
//! One store for the whole window, so a refusal is shown wherever the press happened - in Live, in
//! Setup or from a MIDI pad - and `run()` is the one way a view sends a command: a rejected promise
//! always ends up on screen, never in the console.

import { useSyncExternalStore } from "react";

export interface Note {
  text: string;
  kind: "engine" | "error";
  seq: number;
  at: number;
}

let notes: Note[] = [];
let seq = 0;
const listeners = new Set<() => void>();

function announce(): void {
  for (const l of listeners) l();
}

export function note(text: string, kind: Note["kind"] = "engine"): void {
  // The engine repeats its newest sentence in every status event; one copy is enough.
  if (notes[0]?.text === text && notes[0]?.kind === kind) return;
  notes = [{ text, kind, seq: ++seq, at: Date.now() }, ...notes].slice(0, 8);
  announce();
}

export function fail(text: string): void {
  note(text, "error");
}

export function clearNotes(): void {
  notes = [];
  announce();
}

export function useNotes(): Note[] {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => notes,
  );
}

/** Send a command; a refusal becomes a note. */
export function run(work: () => Promise<unknown>): void {
  void work().catch((e) => fail((e as Error).message));
}

/** Send a runner command, whose answer is a sentence worth showing. */
export function say(work: () => Promise<string>): void {
  work()
    .then((text) => note(text, "engine"))
    .catch((e) => fail((e as Error).message));
}
