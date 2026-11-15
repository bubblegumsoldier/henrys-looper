import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DocStore } from "./docStore";
import { fakeBackend, henry } from "./fixtures";
import * as ops from "./ops";

const text = (doc = henry()) =>
  JSON.stringify({ ...doc, sections: doc.sections.map(({ resolved: _r, ...s }) => s) });

describe("DocStore", () => {
  let saved: string[];
  let store: DocStore;

  beforeEach(async () => {
    vi.useFakeTimers();
    saved = [];
    store = new DocStore(fakeBackend(), { onText: (t) => saved.push(t), parseDelayMs: 100 });
    await store.replace(text());
  });
  afterEach(() => vi.useRealTimers());

  it("starts in sync with nothing to undo", () => {
    const s = store.get();
    expect(s.inSync).toBe(true);
    expect(s.doc!.sections).toHaveLength(6);
    expect(s.canUndo).toBe(false);
  });

  it("a grid edit renders new text and can be undone and redone", async () => {
    expect(store.edit((d) => ops.renameSection(d, 2, "layer_2"))).toBe(true);
    // Drawn at once, before the render is back.
    expect(store.get().doc!.sections[3].repeat).toBe("layer_2");
    await store.settled();
    expect(store.get().text).toContain("layer_2");
    expect(saved[saved.length - 1]).toBe(store.get().text);
    expect(store.get().canUndo).toBe(true);

    store.undo();
    expect(store.get().text).not.toContain("layer_2");
    expect(store.get().doc!.sections[2].id).toBe("voice_2");
    expect(saved[saved.length - 1]).toBe(store.get().text);
    store.redo();
    expect(store.get().doc!.sections[2].id).toBe("layer_2");
  });

  it("a mixer write is one undo step and the document stays the same document", async () => {
    const id = store.get().docId;
    store.edit((d) => ({ ...d, mixer: { tracks: [{ track: d.tracks[0].name, gain: -3 }] } }));
    await store.settled();
    expect(store.get().doc!.mixer?.tracks[0].gain).toBe(-3);
    store.undo();
    expect(store.get().doc!.mixer).toBeUndefined();
    expect(store.get().docId).toBe(id);
    await store.replace(text());
    expect(store.get().docId).toBe(id + 1);
  });

  it("edits made in a row are rendered in order and each is one undo step", async () => {
    store.edit((d) => ops.addSection(d));
    store.edit((d) => ops.addSection(d));
    store.edit((d) => ops.deleteSection(d, 0));
    await store.settled();
    expect(store.get().doc!.sections.map((s) => s.id)).toEqual([
      "voice_1", "voice_2", "voice_3", "alles", "ende", "section_7", "section_8",
    ]);
    store.undo();
    store.undo();
    expect(store.get().doc!.sections).toHaveLength(7);
    store.undo();
    expect(store.get().doc!.sections).toHaveLength(6);
    expect(store.get().canUndo).toBe(false);
  });

  it("an edit after undo drops the redo branch", async () => {
    store.edit((d) => ops.setBpm(d, 100));
    await store.settled();
    store.undo();
    store.edit((d) => ops.setBpm(d, 120));
    await store.settled();
    expect(store.get().canRedo).toBe(false);
    expect(store.get().doc!.bpm).toBe(120);
  });

  it("an edit the compiler rejects stays editable and carries the error", async () => {
    store.edit((d) => ops.renameSection(d, 1, "gitarre_rec"));
    await store.settled();
    const s = store.get();
    expect(s.inSync).toBe(true);
    expect(s.errors[0].at).toMatchObject({ kind: "section", index: 1 });
  });

  it("typing is parsed after a pause; broken text keeps the last good model read-only", async () => {
    const good = store.get().doc;
    store.setText("{ broken");
    expect(store.get().text).toBe("{ broken");
    expect(saved[saved.length - 1]).toBe("{ broken");
    expect(store.edit((d) => ops.setBpm(d, 1))).toBe(false); // not parsed yet
    await vi.advanceTimersByTimeAsync(150);
    const s = store.get();
    expect(s.inSync).toBe(false);
    expect(s.doc).toBe(good);
    expect(s.errors).toHaveLength(1);
    expect(store.edit((d) => ops.setBpm(d, 1))).toBe(false);

    store.undo();
    expect(store.get().inSync).toBe(true);
    expect(store.get().text).toBe(text());
  });

  it("valid typing becomes the model and one undo step", async () => {
    const doc = henry();
    doc.bpm = 99;
    store.setText(text(doc));
    await vi.advanceTimersByTimeAsync(150);
    expect(store.get().doc!.bpm).toBe(99);
    store.undo();
    expect(store.get().doc!.bpm).toBe(141);
  });

  it("undo during unparsed typing drops just that typing", async () => {
    store.edit((d) => ops.setBpm(d, 100));
    await store.settled();
    const after = store.get().text;
    store.setText(after + " ");
    store.undo();
    expect(store.get().text).toBe(after);
    await vi.advanceTimersByTimeAsync(150);
    expect(store.get().text).toBe(after); // the cancelled parse does not come back
    store.undo();
    expect(store.get().doc!.bpm).toBe(141);
  });

  it("replace starts a new history", async () => {
    store.edit((d) => ops.setBpm(d, 100));
    await store.settled();
    await store.replace(text());
    expect(store.get().canUndo).toBe(false);
    expect(store.get().doc!.bpm).toBe(141);
  });

  it("a backend failure leaves the document as it was", async () => {
    const failing = new DocStore(
      { ...fakeBackend(), render: () => Promise.reject(new Error("no window")) },
      { parseDelayMs: 10 },
    );
    await failing.replace(text());
    failing.edit((d) => ops.setBpm(d, 1));
    await failing.settled();
    expect(failing.get().failure).toBe("no window");
    expect(failing.get().doc!.bpm).toBe(141);
  });
});
