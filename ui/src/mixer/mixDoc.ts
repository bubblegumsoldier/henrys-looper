//! The `mixer:` section of a score as plain data: what an entry consists of, how changed entries
//! are written into a document, and the section without comments. No engine, no store - the
//! wiring is `scoreMix.ts`.

import type { DocMixer, DocMixerTrack, MixerSource, ScoreDoc, TrackMix } from "../types";

/** The fields of a mixer entry, in the order the file writes them. */
export const MIX_FIELDS = [
  "gain",
  "mute",
  "preset",
  "bypass",
  "high_pass",
  "eq",
  "band1",
  "band2",
  "band3",
  "comp",
  "delay",
  "reverb",
] as const satisfies readonly (keyof TrackMix)[];

/** Only the mix fields of an entry (no name, comments, line). */
export function mixOf(entry: TrackMix): TrackMix {
  const out: Record<string, unknown> = {};
  for (const f of MIX_FIELDS) if (entry[f] !== undefined) out[f] = entry[f];
  return out as TrackMix;
}

/** A value with its object keys sorted, so two equal mixes print the same. */
export function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(value).sort()) {
      const v = (value as Record<string, unknown>)[k];
      if (v !== undefined) out[k] = canonical(v);
    }
    return out;
  }
  return value;
}

const same = (a: unknown, b: unknown) => JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));

/** The document's section without comments - what `mixerApi.apply` takes. */
export function sourceOf(mixer: DocMixer | undefined): MixerSource {
  if (!mixer) return {};
  const tracks: Record<string, TrackMix> = {};
  for (const t of mixer.tracks) tracks[t.track] = mixOf(t);
  return { ...(mixer.buses ? { buses: { ...mixer.buses } } : {}), tracks };
}

/**
 * Write what changed into the document. `changes` are the entries the engine reports as changed;
 * `full` the whole mix, used when the document has no section yet. Entries for tracks the score
 * does not have are ignored; an entry that already says the same is left alone (comments and all).
 * Returns the same object when nothing changes.
 */
export function mergeMixer(doc: ScoreDoc, changes: MixerSource, full: MixerSource | null = null): ScoreDoc {
  const names = new Set(doc.tracks.map((t) => t.name));
  const base: DocMixer = doc.mixer
    ? { ...doc.mixer, tracks: doc.mixer.tracks.slice() }
    : { tracks: [], ...(full?.buses ? { buses: { ...full.buses } } : {}) };
  let changed = false;
  const incoming: [string, TrackMix][] = [
    ...(!doc.mixer && full ? Object.entries(full.tracks ?? {}) : []),
    ...Object.entries(changes.tracks ?? {}),
  ];
  for (const [name, mix] of incoming) {
    if (!names.has(name)) continue;
    const at = base.tracks.findIndex((t) => t.track === name);
    if (at >= 0) {
      const old = base.tracks[at];
      if (same(mixOf(old), mixOf(mix))) continue;
      const entry: DocMixerTrack = { track: name, ...mixOf(mix) };
      if (old.comments) entry.comments = old.comments;
      // A field comment stays while its field is still written.
      const kept = Object.entries(old.field_comments ?? {}).filter(([f]) => (mix as Record<string, unknown>)[f] !== undefined);
      if (kept.length > 0) entry.field_comments = Object.fromEntries(kept);
      base.tracks[at] = entry;
    } else {
      base.tracks.push({ track: name, ...mixOf(mix) });
      // In the order of the score's tracks.
      const order = new Map(doc.tracks.map((t, i) => [t.name, i]));
      base.tracks.sort((a, b) => (order.get(a.track) ?? 99) - (order.get(b.track) ?? 99));
    }
    changed = true;
  }
  if (changes.buses && !same(base.buses ?? {}, changes.buses)) {
    base.buses = { ...changes.buses };
    changed = true;
  }
  if (!changed) return doc;
  return { ...doc, mixer: base };
}

/** True when `changes` says nothing. */
export function isEmpty(changes: MixerSource): boolean {
  return !changes.buses && Object.keys(changes.tracks ?? {}).length === 0;
}

/**
 * A score text without its `mixer:` block, for "is the loaded score still the text in the editor":
 * the mix of the open song is kept in step with the engine, so a fader move does not make the
 * loaded score stale.
 */
export function withoutMixer(text: string): string {
  const lines = text.split("\n");
  const out: string[] = [];
  let inside = false;
  for (const line of lines) {
    if (/^mixer\s*:/.test(line)) {
      inside = true;
      continue;
    }
    if (inside && line !== "" && !/^[\s#]/.test(line)) inside = false;
    if (!inside) out.push(line);
  }
  return out.join("\n").trimEnd();
}
