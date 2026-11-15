//! The mixer and the song it belongs to.
//!
//! ```text
//!   Fader / Taste / MIDI ──► engine ──mixer_rev──► take() ──► docStore.edit(mixer:) ──► ⌘S
//!   Load / Undo / Redo / Tippen im mixer: ──────────────────► apply() ──► engine
//! ```
//!
//! **With a score loaded** (the runner holds one, and it is the document open in the arrangement),
//! the score's `mixer:` section *is* the mix: a fader moved anywhere - mouse, keyboard, MIDI - is
//! written into it a moment later, which marks the file as changed, and ⌘S saves it with the song.
//! The engine reports every human change by moving `mixer_rev`; `mixerApi.take` then says which
//! tracks changed, and only those entries are rewritten (comments stay where they are). The first
//! change in a score without the section writes the whole mix, so a saved song is complete.
//!
//! **Undo and redo** are the arrangement's own (`docStore.ts`): a burst of fader moves is one step
//! (the write waits for the fader to rest), and undoing it puts the engine back as well - the
//! section of the open song and the sound always agree. The same holds for typing in the text
//! view: once the edited `mixer:` parses, the engine follows. What never moves the engine is
//! *another* document: opening a different file only shows its mix; "Load" applies it.
//!
//! **Without a score** (the free mode) there is no file to write into. The host keeps the tracks'
//! mix in `mixer.json` by name; the bus levels go into the setup (the start configuration), which
//! is what the engine starts with.

import { mixerApi } from "../api";
import { docStore, startDoc } from "../arrangement/useDoc";
import { setConfig } from "../setup/config";
import { currentStatus, subscribe } from "../status";
import type { LooperStatus, MixerBusLevels, MixerTake, ScoreDoc } from "../types";
import { canonical, isEmpty, mergeMixer, pansOf, sourceOf } from "./mixDoc";

export { withoutMixer } from "./mixDoc";

/** How long the mix has to rest before it is written - one undo step per gesture, not per pixel. */
const SETTLE_MS = 350;

// ---------------------------------------------------------------------------------------------
// Keeping engine and document in step
// ---------------------------------------------------------------------------------------------

/** The document (by `docId`) the runner was given; null after a reload of the window. */
let liveDoc: number | null = null;
/** The section as the engine has it, as far as this side knows - to tell an edit from an echo. */
let synced = "";
let lastRev = 0;
let timer: ReturnType<typeof setTimeout> | null = null;
let started = false;

function sameNames(doc: ScoreDoc, status: LooperStatus): boolean {
  return (
    doc.tracks.length === status.tracks.length && doc.tracks.every((t) => status.tracks.some((s) => s.name === t.name))
  );
}

/** Whether the open document is the score the engine plays. */
export function isLive(doc: ScoreDoc | null, docId: number, status: LooperStatus): doc is ScoreDoc {
  if (!doc || !status.running || status.score === null) return false;
  if (liveDoc !== null) return liveDoc === docId && sameNames(doc, status);
  // After a reload of the window nobody knows which document was loaded; the names have to do.
  return sameNames(doc, status);
}

/** Called by "Load": this document is now the one playing, and the host has applied its mix. */
export function markLoaded(): void {
  const s = docStore.get();
  liveDoc = s.docId;
  synced = keyOf(s.doc);
}

/** Changes that could not be written yet; `take` has already forgotten them. */
let pending: MixerTake = {};

function mergeSources(a: MixerTake, b: MixerTake): MixerTake {
  return {
    ...(a.buses || b.buses ? { buses: { ...(a.buses ?? {}), ...(b.buses ?? {}) } as MixerBusLevels } : {}),
    tracks: { ...(a.tracks ?? {}), ...(b.tracks ?? {}) },
    ...(a.pan || b.pan ? { pan: { ...(a.pan ?? {}), ...(b.pan ?? {}) } } : {}),
  };
}

/** The part of a song the engine follows: its `mixer:` section and the tracks' pans. */
const keyOf = (doc: ScoreDoc | null | undefined) =>
  JSON.stringify(canonical({ mixer: sourceOf(doc?.mixer), pan: pansOf(doc ?? null) }));

async function flush(): Promise<void> {
  timer = null;
  let changes: MixerTake;
  try {
    changes = mergeSources(pending, await mixerApi.take());
  } catch {
    return; // no engine (any more)
  }
  pending = {};
  if (isEmpty(changes)) return;
  const status = currentStatus();

  if (status.score === null) {
    // Free mode: the tracks are the host's business (mixer.json); the bus levels belong to the
    // setup the engine is started with.
    if (changes.buses) {
      const main = status.buses.find((b) => b.id === "main");
      const monitor = status.buses.find((b) => b.id === "monitor");
      setConfig((c) => ({
        ...c,
        bus_gain_main: main?.gain ?? c.bus_gain_main,
        bus_gain_monitor: monitor?.gain ?? c.bus_gain_monitor,
        click_gain: status.click_gain,
        monitor_gain: status.monitor_gain,
      }));
    }
    return;
  }

  const state = docStore.get();
  if (!isLive(state.doc, state.docId, status)) return;
  if (!state.inSync) {
    // Typing is being parsed right now: try again once it has settled.
    pending = changes;
    schedule();
    return;
  }
  let full: Awaited<ReturnType<typeof mixerApi.state>> | null = null;
  const mixerChanged = !!changes.buses || Object.keys(changes.tracks ?? {}).length > 0;
  if (!state.doc.mixer && mixerChanged) {
    try {
      full = await mixerApi.state();
    } catch {
      return;
    }
  }
  const doc = docStore.get().doc;
  if (!doc) return;
  const next = mergeMixer(doc, changes, full);
  if (next === doc) return;
  const before = synced;
  // Set first: the store announces the edit synchronously, and that is no edit to send back.
  synced = keyOf(next);
  if (!docStore.edit(() => next)) {
    synced = before;
    pending = changes;
    schedule();
  }
}

function schedule(): void {
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => void flush(), SETTLE_MS);
}

/** The document changed (undo, redo, typing): the engine follows its section - only this song's. */
function onDoc(): void {
  const s = docStore.get();
  if (!s.inSync || !s.doc) return;
  const status = currentStatus();
  if (!isLive(s.doc, s.docId, status)) return;
  const key = keyOf(s.doc);
  if (key === synced) return;
  synced = key;
  // Undoing the very first write of the section: the engine keeps its mix (the pans still follow).
  const source = s.doc.mixer ? sourceOf(s.doc.mixer) : {};
  void mixerApi.apply(source, pansOf(s.doc)).catch(() => undefined);
}

/** Start keeping the mix and the open song in step. Idempotent. */
export function startScoreMix(): void {
  if (started) return;
  started = true;
  startDoc();
  let lastDocId = docStore.get().docId;
  docStore.subscribe((s) => {
    if (s.docId !== lastDocId) {
      // Another document: it is not playing until "Load" says so.
      lastDocId = s.docId;
      synced = keyOf(s.doc);
      return;
    }
    onDoc();
  });
  subscribe((status) => {
    if (!status.running) {
      lastRev = 0;
      return;
    }
    if (status.mixer_rev === lastRev) return;
    // A new engine counts from zero again.
    const fresh = status.mixer_rev < lastRev;
    lastRev = status.mixer_rev;
    if (fresh && status.mixer_rev === 0) return;
    schedule();
  });
}
