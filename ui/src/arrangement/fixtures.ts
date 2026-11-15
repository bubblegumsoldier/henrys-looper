//! Test fixtures: the shape of `examples/henry-3-4.rust.yaml` as a document, plus a fake backend.
//! Only imported by tests.

import type { ParseOutcome, RenderOutcome, ScoreDoc } from "../types";
import { resolveDoc } from "./resolve";

export function henry(): ScoreDoc {
  return resolveDoc({
    title: "Henry 3/4",
    bpm: 141,
    time_signature: "3/4",
    tracks: [
      { name: "gitarre", input: 2 },
      { name: "voice", input: 1, comments: { after: "monitor: true is the default" } },
    ],
    midi: [
      { key: "transport.goto.4", note: 40 },
      { key: "transport.goto.6", note: 41 },
      { key: "track.voice.record", note: 36 },
      { key: "track.2.play", note: 37 },
      { key: "next_section", cc: 64 },
    ],
    sections: [
      { id: "gitarre_rec", bars: 8, autorelease: true, tracks: [{ track: "gitarre", state: "record" }, { track: "voice", state: "hear_through" }] },
      { id: "voice_1", bars: 8, autorelease: true, tracks: [{ track: "gitarre", state: "play" }, { track: "voice", state: "record" }] },
      { id: "voice_2", bars: 8, autorelease: true, tracks: [{ track: "gitarre", state: "play" }, { track: "voice", state: "overdub" }] },
      { id: "voice_3", repeat: "voice_2", autorelease: false, quantize: "loop", tracks: [], comments: { before: ["Layer 3."] } },
      { id: "alles", bars: 8, autorelease: false, quantize: "bar", tracks: [{ track: "gitarre", state: "play" }, { track: "voice", state: "play" }] },
      { id: "ende", bars: 1, autorelease: false, tracks: [] },
    ],
  });
}

/** Text is the document as JSON; "invalid" is any text that is not JSON. Duplicate ids are an error. */
export function fakeBackend(log: string[] = []) {
  const check = (doc: ScoreDoc) => {
    const ids = doc.sections.map((s) => s.id);
    const dup = ids.find((id, i) => ids.indexOf(id) !== i);
    return dup
      ? [{ line: 1, column: 1, message: `duplicate ${dup}`, suggestion: null, at: { kind: "section" as const, index: ids.lastIndexOf(dup), id: dup } }]
      : [];
  };
  const strip = (doc: ScoreDoc) => ({ ...doc, sections: doc.sections.map(({ resolved: _r, ...s }) => s) });
  return {
    async parse(yaml: string): Promise<ParseOutcome> {
      log.push("parse");
      try {
        const doc = resolveDoc(JSON.parse(yaml) as ScoreDoc);
        const errors = check(doc);
        return { ok: errors.length === 0, doc: errors.length ? null : doc, score: null, locations: [], errors };
      } catch {
        return { ok: false, doc: null, score: null, locations: [], errors: [{ line: 1, column: 1, message: "not JSON", suggestion: null, at: null }] };
      }
    },
    async render(doc: ScoreDoc): Promise<RenderOutcome> {
      log.push("render");
      const yaml = JSON.stringify(strip(doc));
      const errors = check(doc);
      return { ok: errors.length === 0, yaml, doc: resolveDoc(doc), score: null, locations: [], errors };
    },
  };
}
