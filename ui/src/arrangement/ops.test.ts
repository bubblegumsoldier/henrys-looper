import { describe, expect, it } from "vitest";
import { henry } from "./fixtures";
import * as ops from "./ops";
import { STATE_BY_KEY, STATE_LOOK, STATES } from "./labels";
import { clipOptions, deriveLayers, inheritedState, resolveSection, sectionSpans, totalBars } from "./resolve";

describe("clip grid", () => {
  it("sets and drops a clip's own note grid without changing its state", () => {
    const doc = henry();
    const cell = { section: 5, track: 0 };
    const name = doc.tracks[0].name;
    const before = inheritedState(doc, 5, name).state;
    const set = ops.setCellGrid(doc, cell, "1/16");
    expect(set.sections[5].tracks.find((t) => t.track === name)).toMatchObject({ track: name, state: before, grid: "1/16" });
    // Picking the shown state again keeps the entry, because it carries the grid.
    const same = ops.setCellState(set, [cell], before);
    expect(same.sections[5].tracks.find((t) => t.track === name)?.grid).toBe("1/16");
    const dropped = ops.setCellGrid(same, cell, undefined);
    expect(dropped.sections[5].tracks.find((t) => t.track === name)?.grid).toBeUndefined();
  });
});

describe("clip options", () => {
  it("sets monitor on a play clip only, and a state change away from play drops it", () => {
    const doc = henry();
    const name = doc.tracks[0].name;
    const find = (d: ReturnType<typeof henry>, s: number) => d.sections[s].tracks.find((t) => t.track === name);
    const played = ops.setCellState(doc, [{ section: 5, track: 0 }], "play");
    const heard = ops.setCellOptions(played, { section: 5, track: 0 }, { monitor: true });
    expect(find(heard, 5)).toMatchObject({ state: "play", monitor: true });
    expect(clipOptions(heard, 5, name).monitor).toBe(true);
    expect(find(ops.setCellState(heard, [{ section: 5, track: 0 }], "record"), 5)?.monitor).toBeUndefined();
    expect(find(ops.setCellOptions(heard, { section: 5, track: 0 }, { monitor: false }), 5)?.monitor).toBeUndefined();
    // On record it is not written at all.
    expect(find(ops.setCellOptions(doc, { section: 0, track: 0 }, { monitor: true }), 0)?.monitor).toBeUndefined();
  });

  it("sets restart and a take length, keeps them through a state change that takes them and drops them otherwise", () => {
    const doc = henry();
    const name = doc.tracks[0].name;
    const find = (d: ReturnType<typeof henry>, s: number) => d.sections[s].tracks.find((t) => t.track === name);
    // Section 0 records the guitar: a take of two bars.
    const short = ops.setCellOptions(doc, { section: 0, track: 0 }, { bars: 2 });
    expect(find(short, 0)).toMatchObject({ state: "record", bars: 2 });
    expect(clipOptions(short, 0, name)).toEqual({ restart: false, bars: 2 });
    // Restart does not go with record: it is not written.
    expect(find(ops.setCellOptions(short, { section: 0, track: 0 }, { restart: true }), 0)?.restart).toBeUndefined();
    // Overdub takes no take length - switching drops it, and the compiler would have refused it.
    expect(find(ops.setCellState(short, [{ section: 0, track: 0 }], "overdub"), 0)?.bars).toBeUndefined();
    expect(find(ops.setCellOptions(short, { section: 0, track: 0 }, { bars: null }), 0)?.bars).toBeUndefined();

    // Section 5 does not state the guitar (it is on stop): restart on a play clip.
    const played = ops.setCellState(doc, [{ section: 5, track: 0 }], "play");
    const restarted = ops.setCellOptions(played, { section: 5, track: 0 }, { restart: true });
    expect(find(restarted, 5)).toMatchObject({ state: "play", restart: true });
    // Picking the same state again keeps it; stop drops it.
    expect(find(ops.setCellState(restarted, [{ section: 5, track: 0 }], "play"), 5)?.restart).toBe(true);
    expect(find(ops.setCellState(restarted, [{ section: 5, track: 0 }], "stop"), 5)?.restart).toBeUndefined();
    expect(find(ops.setCellOptions(restarted, { section: 5, track: 0 }, { restart: false }), 5)?.restart).toBeUndefined();
  });

  it("are inherited with the state through a repeat, and replaced by a clip of its own", () => {
    const doc = henry();
    // Section 3 repeats section 2 (voice_2), which overdubs the voice.
    const name = doc.tracks[1].name;
    const set = ops.setCellOptions(doc, { section: 2, track: 1 }, { restart: true });
    expect(clipOptions(set, 3, name).restart).toBe(true);
    const own = ops.setCellState(set, [{ section: 3, track: 1 }], "play");
    expect(clipOptions(own, 3, name).restart).toBe(false);
  });
});

const keys = (doc: ReturnType<typeof henry>) => doc.midi.map((m) => m.key);
const ids = (doc: ReturnType<typeof henry>) => doc.sections.map((s) => s.id);

describe("resolve", () => {
  it("inherits through repeat and marks the origin", () => {
    const r = resolveSection(henry(), 3);
    expect(r.bars).toEqual({ value: 8, from: { kind: "repeat", index: 2, section: "voice_2" } });
    expect(r.autorelease).toEqual({ value: false, from: { kind: "own" } });
    expect(r.tracks[1]).toEqual({ track: "voice", state: "overdub", from: { kind: "repeat", index: 2, section: "voice_2" } });
  });

  it("falls back to the defaults", () => {
    const r = resolveSection(henry(), 5);
    expect(r.quantize).toEqual({ value: "loop", from: { kind: "default" } });
    expect(r.tracks[0]).toEqual({ track: "gitarre", state: "stop", from: { kind: "default" } });
  });

  it("survives a repeat cycle", () => {
    const doc = henry();
    doc.sections[2].repeat = "voice_3";
    const r = resolveSection(doc, 3);
    expect(r.bars.value).toBe(8);
  });

  it("lays sections out by bars", () => {
    expect(sectionSpans(henry()).map((s) => s.start)).toEqual([0, 8, 16, 24, 32, 40]);
    expect(totalBars(henry())).toBe(41);
  });

  it("derives the layer numbers", () => {
    const layers = deriveLayers(henry());
    // voice: hear, record (1), overdub (2), overdub (3), play (3 layers)
    expect(layers.map((s) => s[1].writes)).toEqual([null, 1, 2, 3, null, null]);
    expect(layers[4][1].layers).toBe(3);
    expect(layers[1][1].stacked).toBe(true);
    // gitarre records once and nothing is stacked on it
    expect(layers[0][0]).toEqual({ layers: 1, writes: 1, stacked: false });
  });
});

describe("cells", () => {
  it("writes an override into a section with a template", () => {
    const doc = ops.setCellState(henry(), [{ section: 3, track: 0 }], "stop");
    expect(doc.sections[3].tracks).toEqual([{ track: "gitarre", state: "stop" }]);
    expect(doc.sections[3].resolved!.tracks[0].from).toEqual({ kind: "own" });
  });

  it("does not write a value equal to the inherited one", () => {
    const doc = ops.setCellState(henry(), [{ section: 3, track: 1 }], "overdub");
    expect(doc.sections[3].tracks).toEqual([]);
    const reset = ops.setCellState(henry(), [{ section: 1, track: 0 }], "stop");
    expect(reset.sections[1].tracks.map((t) => t.track)).toEqual(["voice"]);
  });

  it("sets several cells at once and keeps track order", () => {
    const doc = ops.setCellState(henry(), [{ section: 5, track: 1 }, { section: 5, track: 0 }], "play");
    expect(doc.sections[5].tracks).toEqual([
      { track: "gitarre", state: "play" },
      { track: "voice", state: "play" },
    ]);
  });

  it("resets to the template", () => {
    const changed = ops.setCellState(henry(), [{ section: 3, track: 1 }], "record");
    const back = ops.resetCells(changed, [{ section: 3, track: 1 }]);
    expect(back.sections[3].tracks).toEqual([]);
    expect(inheritedState(back, 3, "voice").state).toBe("overdub");
  });

  it("does not touch the input document", () => {
    const doc = henry();
    const before = JSON.stringify(doc);
    ops.setCellState(doc, [{ section: 0, track: 0 }], "play");
    ops.deleteSection(doc, 2);
    ops.renameTrack(doc, 1, "vox");
    expect(JSON.stringify(doc)).toBe(before);
  });
});

describe("sections", () => {
  it("renames and moves repeat references along", () => {
    const doc = ops.renameSection(henry(), 2, "layer_2");
    expect(doc.sections[3].repeat).toBe("layer_2");
    expect(doc.sections[3].resolved!.bars.value).toBe(8);
  });

  it("renumbers goto bindings when a section moves", () => {
    const doc = ops.moveSection(henry(), 3, 0);
    expect(ids(doc)).toEqual(["voice_3", "gitarre_rec", "voice_1", "voice_2", "alles", "ende"]);
    expect(keys(doc)).toContain("transport.goto.1");
    expect(keys(doc)).toContain("transport.goto.6");
    const back = ops.moveSection(henry(), 0, 5);
    expect(keys(back)).toContain("transport.goto.3"); // voice_3 was 4th, is now 3rd
    expect(keys(back)).toContain("transport.goto.5"); // ende was 6th, is now 5th
  });

  it("inserts and duplicates with shifted gotos", () => {
    const added = ops.addSection(henry(), 1);
    expect(added.sections[1]).toMatchObject({ id: "section_7", bars: 4 });
    expect(keys(added)).toEqual(expect.arrayContaining(["transport.goto.5", "transport.goto.7"]));
    const dup = ops.duplicateSection(henry(), 3);
    expect(ids(dup)[4]).toBe("voice_4");
    expect(dup.sections[4]).toMatchObject({ repeat: "voice_2", autorelease: false, quantize: "loop" });
    expect(dup.sections[4].comments).toBeUndefined();
    expect(keys(dup)).toEqual(expect.arrayContaining(["transport.goto.4", "transport.goto.7"]));
  });

  it("deleting a template folds its values into the sections that use it", () => {
    const before = resolveSection(henry(), 3);
    const doc = ops.deleteSection(henry(), 2);
    const s = doc.sections[2];
    expect(s.id).toBe("voice_3");
    expect(s.repeat).toBeUndefined();
    expect(s.bars).toBe(8);
    expect(s.autorelease).toBe(false); // its own value wins over the template's
    expect(doc.sections[2].resolved!.tracks.map((t) => t.state)).toEqual(before.tracks.map((t) => t.state));
  });

  it("deleting a section drops its goto binding and shifts the rest", () => {
    const doc = ops.deleteSection(henry(), 3);
    expect(keys(doc)).not.toContain("transport.goto.4");
    expect(keys(doc)).toContain("transport.goto.5");
    expect(doc.midi).toHaveLength(4);
  });

  it("patches and removes fields", () => {
    let doc = ops.patchSection(henry(), 3, { bars: 6 });
    expect(doc.sections[3].resolved!.bars).toEqual({ value: 6, from: { kind: "own" } });
    doc = ops.patchSection(doc, 3, { bars: null, repeat: null });
    expect(doc.sections[3].resolved!.bars).toEqual({ value: 4, from: { kind: "default" } });
    expect(ops.patchSection(henry(), 0, { bars: 0 }).sections[0].bars).toBe(1);
  });
});

describe("tracks", () => {
  it("renames states and bindings", () => {
    const doc = ops.renameTrack(henry(), 1, "vox");
    expect(doc.sections[0].tracks[1].track).toBe("vox");
    expect(keys(doc)).toContain("track.vox.record");
    expect(keys(doc)).toContain("track.2.play");
    expect(doc.tracks[1].comments).toEqual({ after: "monitor: true is the default" });
  });

  it("moves and renumbers index bindings", () => {
    const doc = ops.moveTrack(henry(), 1, 0);
    expect(doc.tracks.map((t) => t.name)).toEqual(["voice", "gitarre"]);
    expect(keys(doc)).toContain("track.1.play");
    expect(doc.sections[0].tracks.map((t) => t.track)).toEqual(["voice", "gitarre"]);
  });

  it("deletes a track with its states and bindings", () => {
    const doc = ops.deleteTrack(henry(), 1);
    expect(doc.tracks).toHaveLength(1);
    expect(doc.sections.every((s) => s.tracks.every((t) => t.track === "gitarre"))).toBe(true);
    expect(keys(doc)).not.toContain("track.voice.record");
    expect(keys(doc)).not.toContain("track.2.play");
    expect(keys(doc)).toContain("next_section");
  });

  it("adds a track on a free input, at most eight", () => {
    let doc = ops.addTrack(henry());
    expect(doc.tracks[2]).toEqual({ name: "track_3", input: 3 });
    for (let i = 0; i < 10; i++) doc = ops.addTrack(doc);
    expect(doc.tracks).toHaveLength(ops.MAX_TRACKS);
  });

  it("patches fields and removes them with undefined", () => {
    let doc = ops.patchTrack(henry(), 0, { pan: -0.5, input_right: 3 });
    expect(doc.tracks[0]).toMatchObject({ pan: -0.5, input_right: 3 });
    doc = ops.patchTrack(doc, 0, { input_right: undefined });
    expect(doc.tracks[0].input_right).toBeUndefined();
  });
});

describe("head and names", () => {
  it("time signature replaces the old alias", () => {
    const doc = henry();
    doc.beats_per_bar = 3;
    const out = ops.setTimeSignature(doc, "4/4");
    expect(out.time_signature).toBe("4/4");
    expect(out.beats_per_bar).toBeUndefined();
    expect(ops.setTitle(henry(), "").title).toBeUndefined();
    expect(ops.setBpm(henry(), 90).bpm).toBe(90);
  });

  it("checks names", () => {
    expect(ops.nameProblem("voice", ["voice"], "track")).toMatch(/already/);
    expect(ops.nameProblem("a b", [], "section")).toMatch(/letters/);
    expect(ops.nameProblem("3", [], "track")).toMatch(/number/);
    expect(ops.nameProblem("vox_2", [], "track")).toBeNull();
    expect(ops.uniqueName("voice_3", ["voice_3", "voice_4"])).toBe("voice_2");
  });
});

describe("instrument tracks", () => {
  it("turns a track into an instrument track and back, dropping what only the other kind has", () => {
    const doc = henry();
    doc.tracks[1].latency = 300;
    const inst = ops.setTrackInstrument(doc, 1, true);
    expect(inst.tracks[1]).toMatchObject({ name: "voice", instrument: true, input: 0 });
    expect(inst.tracks[1].latency).toBeUndefined();
    const named = ops.setTrackInstrumentPlugin(inst, "voice", "au:aumu:dls :appl");
    expect(named.mixer?.tracks).toEqual([{ track: "voice", instrument: "au:aumu:dls :appl" }]);
    const midi = ops.patchTrack(named, 1, { midi: { device: "Testkeys", channel: 2 } });
    const back = ops.setTrackInstrument(midi, 1, false);
    expect(back.tracks[1].instrument).toBeUndefined();
    expect(back.tracks[1].midi).toBeUndefined();
    expect(back.tracks[1].input).toBe(1);
    // An audio track cannot name an instrument: the entry loses it.
    expect(back.mixer?.tracks).toEqual([{ track: "voice" }]);
    expect(ops.setTrackInstrument(back, 1, false)).toBe(back);
  });
});

describe("stack", () => {
  it("is a pickable state with its own key and the stack symbol in overdub orange", () => {
    expect(STATES).toContain("stack");
    expect(STATE_BY_KEY.a).toBe("stack");
    // K is the click, in every view.
    expect(STATE_BY_KEY.k).toBeUndefined();
    expect(STATE_LOOK.stack).toMatchObject({ cls: "dub", icon: "ic-layers", word: "Stack", key: "A" });
    expect(STATE_LOOK.overdub.icon).not.toBe(STATE_LOOK.stack.icon);
  });

  it("adds at least one layer and leaves the count open for the sections after it", () => {
    const doc = ops.setCellState(henry(), [{ section: 2, track: 1 }], "stack");
    const layers = deriveLayers(doc);
    expect(layers[2][1]).toMatchObject({ writes: 2, layers: 2, open: true });
    // voice_3 repeats voice_2 and therefore stacks too; "alles" plays three or more.
    expect(layers[4][1]).toMatchObject({ layers: 3, open: true });
    expect(layers[1][1].stacked).toBe(true);
  });
});
