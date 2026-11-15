import { describe, expect, it } from "vitest";
import { deleteTrack, renameTrack } from "../arrangement/ops";
import type { ScoreDoc } from "../types";
import { isEmpty, mergeMixer, pansOf, sourceOf, withoutMixer } from "./mixDoc";

function doc(mixer?: ScoreDoc["mixer"]): ScoreDoc {
  return {
    bpm: 100,
    tracks: [
      { name: "voice", input: 1 },
      { name: "guitar", input: 2 },
    ],
    midi: [],
    sections: [{ id: "a", bars: 4, tracks: [] }],
    ...(mixer ? { mixer } : {}),
  };
}

describe("mergeMixer", () => {
  it("writes the whole mix into a score that has no section yet", () => {
    const full = {
      buses: { main: 0, monitor: -6, click: -12, inputs: 0 },
      tracks: { voice: { gain: 0, preset: "voice" }, guitar: { gain: -3 } },
    };
    const out = mergeMixer(doc(), { tracks: { guitar: { gain: -4 } } }, full);
    expect(out.mixer?.buses).toEqual(full.buses);
    expect(out.mixer?.tracks).toEqual([
      { track: "voice", gain: 0, preset: "voice" },
      { track: "guitar", gain: -4 },
    ]);
  });

  it("rewrites only the entries that changed and keeps their comments", () => {
    const before = doc({
      tracks: [
        { track: "voice", gain: -2, comments: { after: "lead" } },
        { track: "guitar", gain: 0, comp: { threshold: -10 }, field_comments: { comp: { after: "hart" }, gain: { after: "x" } } },
      ],
    });
    const out = mergeMixer(before, { tracks: { guitar: { comp: { threshold: -12 } } } });
    expect(out.mixer?.tracks[0]).toBe(before.mixer?.tracks[0]);
    expect(out.mixer?.tracks[1]).toEqual({
      track: "guitar",
      comp: { threshold: -12 },
      field_comments: { comp: { after: "hart" } },
    });
  });

  it("changes nothing when the engine reports what the score already says", () => {
    const before = doc({ buses: { main: -1 }, tracks: [{ track: "voice", gain: -2, preset: "voice" }] });
    expect(mergeMixer(before, { tracks: { voice: { preset: "voice", gain: -2 } } })).toBe(before);
    expect(mergeMixer(before, { buses: { main: -1 } })).toBe(before);
    // A track the score does not have is none of its business.
    expect(mergeMixer(before, { tracks: { bass: { gain: 3 } } })).toBe(before);
  });

  it("keeps new entries in the order of the score's tracks", () => {
    const before = doc({ tracks: [{ track: "guitar", gain: 0 }] });
    const out = mergeMixer(before, { tracks: { voice: { gain: 1 } } });
    expect(out.mixer?.tracks.map((t) => t.track)).toEqual(["voice", "guitar"]);
  });
});

describe("the section and the rest of the score", () => {
  it("leaves comments and positions out of what the engine is sent", () => {
    const source = sourceOf({
      buses: { click: -12 },
      tracks: [{ track: "voice", gain: -2, comments: { after: "x" }, line: 30 }],
      field_comments: { tracks: { before: ["y"] } },
    });
    expect(source).toEqual({ buses: { click: -12 }, tracks: { voice: { gain: -2 } } });
  });

  it("does not count the mixer block when asking whether the loaded score is stale", () => {
    const base = "bpm: 100\n\ntracks:\n  a: {input: 1}\n\nsections:\n  - id: x\n    bars: 2\n";
    const mixed = `${base}\nmixer:\n  tracks:\n    a: {gain: -3}\n`;
    const remixed = `${base}\nmixer:\n  # louder\n  tracks:\n    a:\n      gain: 2\n      reverb: {mix: 0.2}\n`;
    expect(withoutMixer(mixed)).toBe(withoutMixer(base));
    expect(withoutMixer(remixed)).toBe(withoutMixer(mixed));
    expect(withoutMixer(base.replace("bars: 2", "bars: 3"))).not.toBe(withoutMixer(mixed));
  });

  it("follows a renamed or deleted track", () => {
    const before = doc({ tracks: [{ track: "voice", gain: -2 }, { track: "guitar", mute: true }] });
    const renamed = renameTrack(before, 0, "vocals");
    expect(renamed.mixer?.tracks.map((t) => t.track)).toEqual(["vocals", "guitar"]);
    const deleted = deleteTrack(before, 1);
    expect(deleted.mixer?.tracks).toEqual([{ track: "voice", gain: -2 }]);
  });
});

describe("pan and plugins", () => {
  it("writes a changed pan under tracks, centre as nothing, without creating a mixer section", () => {
    const before = doc();
    const out = mergeMixer(before, { pan: { voice: -0.3, guitar: 0 } });
    expect(out.tracks[0]).toEqual({ name: "voice", input: 1, pan: -0.3 });
    expect(out.tracks[1]).toBe(before.tracks[1]);
    expect(out.mixer).toBeUndefined();
    const back = mergeMixer(out, { pan: { voice: 0 } });
    expect(back.tracks[0]).toEqual({ name: "voice", input: 1 });
    expect(mergeMixer(out, { pan: { voice: -0.3 } })).toBe(out);
    expect(pansOf(out)).toEqual({ voice: -0.3, guitar: 0 });
    expect(isEmpty({ pan: { voice: 1 } })).toBe(false);
  });

  it("keeps instrument and inserts of an entry, and writes them when they change", () => {
    const before = doc({
      tracks: [{ track: "voice", gain: -2, instrument: "au:aumu:dls :appl", inserts: [{ plugin: "au:aufx:dely:appl" }] }],
    });
    // A fader move reports the whole entry, plugins included: nothing is lost.
    const moved = mergeMixer(before, {
      tracks: { voice: { gain: -4, instrument: "au:aumu:dls :appl", inserts: [{ plugin: "au:aufx:dely:appl" }] } },
    });
    expect(moved.mixer?.tracks[0]).toEqual({
      track: "voice",
      gain: -4,
      instrument: "au:aumu:dls :appl",
      inserts: [{ plugin: "au:aufx:dely:appl" }],
    });
    const bypassed = mergeMixer(moved, {
      tracks: { voice: { gain: -4, instrument: "au:aumu:dls :appl", inserts: [{ plugin: "au:aufx:dely:appl", bypass: true }] } },
    });
    expect(bypassed.mixer?.tracks[0].inserts).toEqual([{ plugin: "au:aufx:dely:appl", bypass: true }]);
    expect(sourceOf(bypassed.mixer).tracks?.voice.instrument).toBe("au:aumu:dls :appl");
  });
});
