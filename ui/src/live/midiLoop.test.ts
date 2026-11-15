import { beforeEach, describe, expect, it, vi } from "vitest";

// `status.ts` hangs a helper on `window` in development; the tests run without a DOM.
vi.hoisted(() => {
  (globalThis as { window?: unknown }).window ??= globalThis;
});

const calls: unknown[][] = [];
vi.mock("../api", () => ({
  api: {
    trackLoopKind: vi.fn(async (...a: unknown[]) => void calls.push(["kind", ...a])),
    trackGrid: vi.fn(async (...a: unknown[]) => void calls.push(["grid", ...a])),
    trackTranspose: vi.fn(async (...a: unknown[]) => void calls.push(["transpose", ...a])),
  },
}));
vi.mock("../setup/config", () => ({ setConfig: vi.fn() }));

import { clearNotes, currentNotes as notesNow } from "../shell/notes";
import { structureSignature } from "../status";
import type { LooperStatus, LooperTrackStatus } from "../types";
import { loopKindBlock, setLoopKind } from "./MidiLoopRows";

function track(part: Partial<LooperTrackStatus>): LooperTrackStatus {
  return {
    index: 2,
    name: "Drums",
    input_channels: [],
    channels: 2,
    pan: 0,
    state: "empty",
    monitor: true,
    playing: false,
    loop_samples: 0,
    layers: [],
    pending_kind: null,
    pending_bars: 0,
    pending_beats: 0,
    midi_loop: false,
    grid: "none",
    transpose: 0,
    ...part,
  } as unknown as LooperTrackStatus;
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("loop type of an instrument track", () => {
  beforeEach(() => {
    calls.length = 0;
    clearNotes();
  });

  it("an empty track (e.g. after Clear all) switches to MIDI", async () => {
    expect(loopKindBlock(track({}))).toBeNull();
    setLoopKind(track({}), "midi");
    await flush();
    expect(calls).toEqual([["kind", 2, "midi"]]);
    expect(notesNow()).toEqual([]);
  });

  it("a track holding a loop says why instead of doing nothing", async () => {
    const full = track({ state: "playing", layers: [{ index: 0, number: 1, muted: false, gain: 1 }] });
    setLoopKind(full, "midi");
    await flush();
    expect(calls).toEqual([]);
    const shown = notesNow();
    expect(shown).toHaveLength(1);
    expect(shown[0].kind).toBe("error");
    expect(shown[0].text).toMatch(/^Clear the track first: "Drums" holds a loop \(1 layer\)\./);
  });

  it("the same kind again sends nothing and says nothing", async () => {
    setLoopKind(track({ midi_loop: true }), "midi");
    await flush();
    expect(calls).toEqual([]);
    expect(notesNow()).toEqual([]);
  });
});

describe("the track drawer follows the instrument settings", () => {
  // The live view and the setup only re-read the tracks when this signature changes. Without the
  // loop type, the grid and the transposition in it, a click reached the engine but the drawer kept
  // showing the old value - the switch seemed to do nothing, the grid "jumped back".
  const status = (t: Partial<LooperTrackStatus>) =>
    ({ running: true, tracks: [track(t)] }) as unknown as LooperStatus;

  it("changes with the loop type", () => {
    expect(structureSignature(status({ midi_loop: true }))).not.toBe(structureSignature(status({ midi_loop: false })));
  });
  it("changes with the note grid", () => {
    expect(structureSignature(status({ grid: "1/16" }))).not.toBe(structureSignature(status({ grid: "none" })));
  });
  it("changes with the transposition", () => {
    expect(structureSignature(status({ transpose: 12 }))).not.toBe(structureSignature(status({ transpose: 0 })));
  });
});
