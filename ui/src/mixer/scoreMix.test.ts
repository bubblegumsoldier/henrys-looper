// The order of opening, loading and writing back, against a fake engine and a fake backend: what
// the owner ran into was a file changed on disk whose new fader values never reached the engine.
// Opening must show the file and leave the engine alone, the difference must count as "not
// loaded", the engine's write-back must not overwrite the opened file, and "Load" closes it.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MixerTake, ScoreDoc } from "../types";

const engine = vi.hoisted(() => ({
  take: vi.fn(async (): Promise<MixerTake> => ({})),
  apply: vi.fn(async () => undefined),
  state: vi.fn(async () => ({ tracks: {} })),
  rev: 0,
  listeners: [] as ((s: unknown) => void)[],
}));

// The "YAML" of these tests is the document as JSON: parse and render are each other's inverse.
vi.mock("../api", () => ({
  mixerApi: { take: engine.take, apply: engine.apply, state: engine.state },
  scoreApi: {
    parse: async (text: string) => ({ ok: true, doc: JSON.parse(text), errors: [], score: null }),
    render: async (doc: unknown) => ({ ok: true, yaml: JSON.stringify(doc), doc, errors: [], score: null }),
  },
}));
vi.mock("../setup/config", () => ({ setConfig: () => undefined }));
vi.mock("../status", () => {
  const status = () => ({
    running: true,
    score: { phase: "idle" },
    tracks: [{ name: "bass" }, { name: "mic" }],
    buses: [],
    mixer_rev: engine.rev,
  });
  return {
    currentStatus: status,
    subscribe: (l: (s: unknown) => void) => {
      engine.listeners.push(l);
      return () => undefined;
    },
  };
});

import { docStore } from "../arrangement/useDoc";
import { updateScoreFile } from "../scoreFile";
import { markLoaded, mixIsLoaded, setLoadedTextCheck, startScoreMix } from "./scoreMix";

const song = (bass: number, mic: number, title = "Sketch"): string =>
  JSON.stringify({
    title,
    tracks: [{ name: "bass" }, { name: "mic" }],
    sections: [],
    mixer: { tracks: [{ track: "bass", gain: bass }, { track: "mic", gain: mic }] },
  } as unknown as ScoreDoc);

const gains = () => Object.fromEntries((docStore.get().doc?.mixer?.tracks ?? []).map((t) => [t.track, t.gain]));

async function open(text: string) {
  updateScoreFile({ text, path: "/s/Sketch.yaml", name: "Sketch.yaml", savedText: text });
  await vi.advanceTimersByTimeAsync(1);
}

/** A human moved a fader: the engine counts its revision up and `take` reports the change. */
async function faderMoved(take: MixerTake) {
  engine.take.mockResolvedValueOnce(take);
  engine.rev += 1;
  const status = { running: true, score: { phase: "idle" }, tracks: [{ name: "bass" }, { name: "mic" }], mixer_rev: engine.rev };
  for (const l of engine.listeners) l(status);
  await vi.advanceTimersByTimeAsync(400);
  await docStore.settled();
}

let loaded: string | null = null;

describe("score mix: open, load, write back", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setLoadedTextCheck((text) => loaded !== null && JSON.stringify({ ...JSON.parse(loaded), mixer: 0 }) === JSON.stringify({ ...JSON.parse(text), mixer: 0 }));
    startScoreMix();
  });
  afterEach(() => vi.useRealTimers());

  it("treats a mix changed on disk as not loaded until Load, and never writes the engine over it", async () => {
    // Loaded and playing: bass 0, mic 0.
    loaded = song(0, 0);
    await open(loaded);
    markLoaded();
    expect(mixIsLoaded(docStore.get().doc)).toBe(true);

    // A fader move is written back - that is no difference to what the engine has.
    await faderMoved({ tracks: { mic: { gain: 2 } } });
    expect(gains()).toEqual({ bass: 0, mic: 2 });
    expect(mixIsLoaded(docStore.get().doc)).toBe(true);
    expect(engine.apply).not.toHaveBeenCalled();

    // The file was changed elsewhere and opened again: the file's values are shown, the engine is
    // not moved, and the difference counts - the Load button says "Reload".
    await open(song(-6, 4));
    expect(gains()).toEqual({ bass: -6, mic: 4 });
    expect(engine.apply).not.toHaveBeenCalled();
    expect(mixIsLoaded(docStore.get().doc)).toBe(false);

    // A fader moved now must not be written over the file just opened.
    await faderMoved({ tracks: { bass: { gain: -1 } } });
    expect(gains()).toEqual({ bass: -6, mic: 4 });
    expect(mixIsLoaded(docStore.get().doc)).toBe(false);

    // "Load": the host applied the file's section; from now on they move together again.
    loaded = docStore.get().text;
    markLoaded();
    expect(mixIsLoaded(docStore.get().doc)).toBe(true);
    await faderMoved({ tracks: { bass: { gain: -7 } } });
    expect(gains()).toEqual({ bass: -7, mic: 4 });
    expect(mixIsLoaded(docStore.get().doc)).toBe(true);
  });

  it("keeps writing into the loaded song when the same file is opened again unchanged", async () => {
    loaded = song(1, 1, "Same");
    await open(loaded);
    markLoaded();
    await open(song(1, 1, "Same") + " ");
    expect(mixIsLoaded(docStore.get().doc)).toBe(true);
    await faderMoved({ tracks: { bass: { gain: -2 } } });
    expect(gains()).toEqual({ bass: -2, mic: 1 });
  });
});
