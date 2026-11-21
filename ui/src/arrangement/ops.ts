//! Every edit the arrangement editor can make, as a pure function `ScoreDoc -> ScoreDoc`.
//!
//! Pure on purpose: an edit is then one value in the undo history, it can be tested without a
//! window or an engine, and the render round trip (`scoreApi.render`) only ever sees a finished
//! document. Validation stays the compiler's - an operation may produce a score that does not
//! compile (a duplicate id, a `repeat:` cycle), and the grid then shows the compiler's complaint on
//! the element. What the operations *do* guarantee is that **references move with what they name**:
//!
//! * renaming a section rewrites every `repeat:` that names it;
//! * moving, inserting or deleting a section renumbers the `transport.goto.<n>` bindings (they
//!   count sections from 1, in order), and a binding to a deleted section is dropped;
//! * renaming a track rewrites the section states and `track.<name>.*` bindings; moving or deleting
//!   a track renumbers `track.<n>.*` bindings, and deleting it drops what named it;
//! * deleting a section that others use as their template folds its own values into them, so what
//!   they play does not change.
//!
//! Comments belong to their element and travel with it (the backend's rule); a duplicate gets none,
//! since a copied comment would describe the original.

import type { DocMidiBinding, DocSection, DocSectionTrack, DocTrack, Quantize, ScoreDoc, ScoreState } from "../types";
import { MONITOR_STATES, RESTART_STATES, TAKE_BARS_STATES, inheritedField, inheritedState, resolveDoc } from "./resolve";

/** The engine runs at most this many tracks (`max_tracks`). */
export const MAX_TRACKS = 8;

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** A working copy without the output-only fields, so stale ones never travel back. */
function draft(doc: ScoreDoc): ScoreDoc {
  const copy = clone(doc);
  for (const s of copy.sections) delete s.resolved;
  return copy;
}

/** Recompute `resolved` so the result can be drawn before the backend has answered. */
function done(doc: ScoreDoc): ScoreDoc {
  return resolveDoc(doc);
}

// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

/** `base`, or `base_2`, `base_3`, ... - the first one not in `taken`. */
export function uniqueName(base: string, taken: Iterable<string>): string {
  const set = new Set(taken);
  if (!set.has(base)) return base;
  const stem = base.replace(/_\d+$/, "");
  for (let n = 2; ; n++) {
    const candidate = `${stem}_${n}`;
    if (!set.has(candidate)) return candidate;
  }
}

/**
 * Why a name cannot be used, or null when it can. The compiler would accept more (any string), but
 * a name ends up in MIDI addresses (`track.<name>.record`) where a dot or a space breaks the path.
 */
export function nameProblem(name: string, taken: string[], what: "section" | "track"): string | null {
  const trimmed = name.trim();
  if (trimmed === "") return `A ${what} needs a name.`;
  if (!/^[\p{L}\p{N}_-]+$/u.test(trimmed)) return "Use letters, digits, - and _ only.";
  if (/^\d+$/.test(trimmed) && what === "track") return "A track name cannot be a plain number.";
  if (taken.includes(trimmed)) return `There is already a ${what} called “${trimmed}”.`;
  return null;
}

// ---------------------------------------------------------------------------------------------
// MIDI references
// ---------------------------------------------------------------------------------------------

/**
 * Rewrite the binding keys that point at a section or a track. `goto` maps a 0-based section
 * index, `track` maps a track reference (`{index}` for `track.<n>`, `{name}` for `track.<name>`);
 * returning null drops the binding.
 */
function remapMidi(
  midi: DocMidiBinding[],
  map: {
    goto?: (section: number) => number | null;
    track?: (ref: { index: number } | { name: string }) => string | null;
  },
): DocMidiBinding[] {
  const out: DocMidiBinding[] = [];
  for (const binding of midi) {
    let key: string | null = binding.key;
    const goto = /^transport\.goto\.(\d+)$/.exec(binding.key);
    const track = /^track\.([^.]+)(\..*)?$/.exec(binding.key);
    if (goto && map.goto) {
      const next = map.goto(Number(goto[1]) - 1);
      key = next === null ? null : `transport.goto.${next + 1}`;
    } else if (track && map.track) {
      const ref = /^\d+$/.test(track[1]) ? { index: Number(track[1]) - 1 } : { name: track[1] };
      const next = map.track(ref);
      key = next === null ? null : `track.${next}${track[2] ?? ""}`;
    }
    if (key !== null) out.push(key === binding.key ? binding : { ...binding, key });
  }
  return out;
}

/** The index permutation of moving one element from `from` to `to`, as old -> new. */
function moveIndex(i: number, from: number, to: number): number {
  if (i === from) return to;
  if (from < to && i > from && i <= to) return i - 1;
  if (from > to && i >= to && i < from) return i + 1;
  return i;
}

function move<T>(list: T[], from: number, to: number): T[] {
  const copy = list.slice();
  const [item] = copy.splice(from, 1);
  copy.splice(to, 0, item);
  return copy;
}

// ---------------------------------------------------------------------------------------------
// Score head
// ---------------------------------------------------------------------------------------------

export function setTitle(doc: ScoreDoc, title: string): ScoreDoc {
  const d = draft(doc);
  if (title.trim() === "") delete d.title;
  else d.title = title;
  return done(d);
}

export function setBpm(doc: ScoreDoc, bpm: number): ScoreDoc {
  const d = draft(doc);
  d.bpm = bpm;
  return done(d);
}

/** Writes `time_signature` and drops the old `beats_per_bar` alias, which it supersedes. */
export function setTimeSignature(doc: ScoreDoc, signature: string): ScoreDoc {
  const d = draft(doc);
  d.time_signature = signature;
  delete d.beats_per_bar;
  return done(d);
}

/** `count_in:` - bars of click before the first section, 1 to 8. Always written, also when it is
 *  the default: a value somebody set on purpose stays visible in the file. */
export function setCountIn(doc: ScoreDoc, bars: number): ScoreDoc {
  const d = draft(doc);
  d.count_in = Math.max(1, Math.min(8, Math.round(bars) || 1));
  return done(d);
}

// ---------------------------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------------------------

export interface Cell {
  section: number;
  track: number;
}

/**
 * Set what a track does in a section. A value equal to what the cell would inherit anyway (from
 * its template, or the default `stop`) is not written: the cell then follows its template again,
 * which is what a user who picks the inherited state means.
 */
export function setCellState(doc: ScoreDoc, cells: Cell[], state: ScoreState): ScoreDoc {
  const d = draft(doc);
  for (const cell of cells) {
    const track = d.tracks[cell.track]?.name;
    const section = d.sections[cell.section];
    if (track === undefined || !section) continue;
    const inherited = inheritedState(d, cell.section, track);
    const at = section.tracks.findIndex((t) => t.track === track);
    // Clip options that do not go with the new state go - the compiler would refuse them.
    const kept = at >= 0 ? fitOptions({ ...section.tracks[at], state }) : undefined;
    if (inherited.state === state && !(kept && (kept.grid || kept.restart || kept.monitor || kept.bars !== undefined))) {
      if (at >= 0) section.tracks.splice(at, 1);
    } else if (kept) {
      section.tracks[at] = kept;
    } else {
      section.tracks.push({ track, state });
    }
  }
  // Keep the written order the score's track order, so the text reads like the grid.
  for (const cell of cells) sortStates(d, cell.section);
  return done(d);
}

/**
 * The note grid of one clip (`keys: {state: play, grid: 1/16}`): while the section runs, the
 * track's MIDI layers play on it instead of the track's own grid. `undefined` follows the track.
 * A clip without a state of its own gets the state it shows now, so nothing else changes.
 */
export function setCellGrid(doc: ScoreDoc, cell: Cell, grid: string | undefined): ScoreDoc {
  const d = draft(doc);
  const track = d.tracks[cell.track]?.name;
  const section = d.sections[cell.section];
  if (track === undefined || !section) return doc;
  const at = section.tracks.findIndex((t) => t.track === track);
  if (at >= 0) {
    const entry = { ...section.tracks[at] };
    if (grid === undefined) delete entry.grid;
    else entry.grid = grid;
    section.tracks[at] = entry;
  } else if (grid !== undefined) {
    section.tracks.push({ track, state: inheritedState(d, cell.section, track).state, grid });
    sortStates(d, cell.section);
  }
  return done(d);
}

/** Drop the clip options the entry's state does not take (`restart` / `bars`). */
function fitOptions(entry: DocSectionTrack): DocSectionTrack {
  const out = { ...entry };
  if (!RESTART_STATES.includes(out.state)) delete out.restart;
  if (!TAKE_BARS_STATES.includes(out.state)) delete out.bars;
  if (!MONITOR_STATES.includes(out.state)) delete out.monitor;
  return out;
}

/**
 * The clip options of one clip: `restart` (the loop starts from its top at the section start)
 * and `bars` (a take shorter than the section). `false` / `undefined` remove the option. A clip
 * without a state of its own gets the state it shows now, like [`setCellGrid`].
 */
export function setCellOptions(doc: ScoreDoc, cell: Cell, patch: { restart?: boolean; bars?: number | null; monitor?: boolean }): ScoreDoc {
  const d = draft(doc);
  const track = d.tracks[cell.track]?.name;
  const section = d.sections[cell.section];
  if (track === undefined || !section) return doc;
  let at = section.tracks.findIndex((t) => t.track === track);
  if (at < 0) {
    section.tracks.push({ track, state: inheritedState(d, cell.section, track).state });
    sortStates(d, cell.section);
    at = section.tracks.findIndex((t) => t.track === track);
  }
  const entry = { ...section.tracks[at] };
  if (patch.monitor !== undefined) {
    if (patch.monitor) entry.monitor = true;
    else delete entry.monitor;
  }
  if (patch.restart !== undefined) {
    if (patch.restart) entry.restart = true;
    else delete entry.restart;
  }
  if (patch.bars !== undefined) {
    if (patch.bars === null) delete entry.bars;
    else entry.bars = patch.bars;
  }
  section.tracks[at] = fitOptions(entry);
  return done(d);
}

/** "Reset to template": drop the cells' own states. */
export function resetCells(doc: ScoreDoc, cells: Cell[]): ScoreDoc {
  const d = draft(doc);
  for (const cell of cells) {
    const track = d.tracks[cell.track]?.name;
    const section = d.sections[cell.section];
    if (track === undefined || !section) continue;
    section.tracks = section.tracks.filter((t) => t.track !== track);
  }
  return done(d);
}

function sortStates(d: ScoreDoc, section: number) {
  const s = d.sections[section];
  if (!s) return;
  const order = new Map(d.tracks.map((t, i) => [t.name, i]));
  s.tracks = s.tracks.slice().sort((a, b) => (order.get(a.track) ?? 99) - (order.get(b.track) ?? 99));
}

// ---------------------------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------------------------

export type SectionPatch = {
  bars?: number | null;
  autorelease?: boolean | null;
  quantize?: Quantize | null;
  /** null removes the template. */
  repeat?: string | null;
};

/**
 * Change section fields. `null` removes a field (it is then inherited or the default); a value
 * equal to the inherited one is still written, because choosing it in the inspector is a decision
 * ("this section has 8 bars, whatever the template says").
 */
export function patchSection(doc: ScoreDoc, index: number, patch: SectionPatch): ScoreDoc {
  const d = draft(doc);
  const s = d.sections[index];
  if (!s) return doc;
  for (const key of ["bars", "autorelease", "quantize", "repeat"] as const) {
    if (!(key in patch)) continue;
    const value = patch[key];
    if (value === null || value === undefined) delete s[key];
    else (s as unknown as Record<string, unknown>)[key] = value;
  }
  if (s.bars !== undefined) s.bars = Math.max(1, Math.round(s.bars));
  return done(d);
}

/** A free id for a new section: `section_1`, `section_2`, ... */
function freshSectionId(d: ScoreDoc, base = "section"): string {
  const taken = d.sections.map((s) => s.id);
  if (base !== "section") return uniqueName(base, taken);
  for (let n = d.sections.length + 1; ; n++) {
    if (!taken.includes(`section_${n}`)) return `section_${n}`;
  }
}

/** Insert an empty section of `bars` bars at `at` (default: the end). Returns the new document. */
export function addSection(doc: ScoreDoc, at = doc.sections.length, bars = 4): ScoreDoc {
  const d = draft(doc);
  const section: DocSection = { id: freshSectionId(d), bars, autorelease: true, tracks: [] };
  d.sections.splice(at, 0, section);
  d.midi = remapMidi(d.midi, { goto: (i) => (i >= at ? i + 1 : i) });
  return done(d);
}

/** Copy a section right after itself, with every field and state it states itself. */
export function duplicateSection(doc: ScoreDoc, index: number): ScoreDoc {
  const d = draft(doc);
  const source = d.sections[index];
  if (!source) return doc;
  const copy: DocSection = {
    id: uniqueName(source.id, d.sections.map((s) => s.id)),
    tracks: source.tracks.map((t) => ({ track: t.track, state: t.state })),
  };
  for (const key of ["repeat", "bars", "autorelease", "quantize"] as const) {
    if (source[key] !== undefined) (copy as unknown as Record<string, unknown>)[key] = source[key];
  }
  d.sections.splice(index + 1, 0, copy);
  d.midi = remapMidi(d.midi, { goto: (i) => (i > index ? i + 1 : i) });
  return done(d);
}

export function moveSection(doc: ScoreDoc, from: number, to: number): ScoreDoc {
  if (from === to || to < 0 || to >= doc.sections.length || from < 0 || from >= doc.sections.length) return doc;
  const d = draft(doc);
  d.sections = move(d.sections, from, to);
  d.midi = remapMidi(d.midi, { goto: (i) => moveIndex(i, from, to) });
  return done(d);
}

/**
 * Delete a section. Sections that used it as their template inherit from its template instead and
 * take over what it stated itself, so they play exactly as before. A jump binding to it is dropped.
 */
export function deleteSection(doc: ScoreDoc, index: number): ScoreDoc {
  const d = draft(doc);
  const gone = d.sections[index];
  if (!gone) return doc;
  for (const s of d.sections) {
    if (s === gone || s.repeat !== gone.id) continue;
    for (const key of ["bars", "autorelease", "quantize"] as const) {
      if (s[key] === undefined && gone[key] !== undefined) (s as unknown as Record<string, unknown>)[key] = gone[key];
    }
    for (const state of gone.tracks) {
      if (!s.tracks.some((t) => t.track === state.track)) {
        // The clip options travel with the state, as the compiler lets a repeat inherit them.
        const { restart, bars, monitor } = state;
        s.tracks.push({
          track: state.track,
          state: state.state,
          ...(restart ? { restart } : {}),
          ...(bars !== undefined ? { bars } : {}),
          ...(monitor ? { monitor } : {}),
        });
      }
    }
    if (gone.repeat !== undefined && gone.repeat !== s.id) s.repeat = gone.repeat;
    else delete s.repeat;
  }
  d.sections.splice(index, 1);
  d.midi = remapMidi(d.midi, { goto: (i) => (i === index ? null : i > index ? i - 1 : i) });
  d.sections.forEach((_, i) => sortStates(d, i));
  return done(d);
}

/** Rename a section and every `repeat:` that names it. */
export function renameSection(doc: ScoreDoc, index: number, id: string): ScoreDoc {
  const d = draft(doc);
  const s = d.sections[index];
  if (!s || s.id === id) return doc;
  const old = s.id;
  s.id = id;
  for (const other of d.sections) if (other !== s && other.repeat === old) other.repeat = id;
  return done(d);
}

// ---------------------------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------------------------

/** Append a mono track on the first input no other track uses. */
export function addTrack(doc: ScoreDoc, name?: string): ScoreDoc {
  if (doc.tracks.length >= MAX_TRACKS) return doc;
  const d = draft(doc);
  const used = new Set(d.tracks.flatMap((t) => (t.input_right ? [t.input, t.input_right] : [t.input])));
  let input = 1;
  while (used.has(input)) input++;
  const track: DocTrack = { name: uniqueName(name ?? `track_${d.tracks.length + 1}`, d.tracks.map((t) => t.name)), input };
  d.tracks.push(track);
  return done(d);
}

export type TrackPatch = Partial<Omit<DocTrack, "name" | "comments" | "line">>;

/** Change track fields. A key mapped to `undefined` removes the field (the default applies). */
export function patchTrack(doc: ScoreDoc, index: number, patch: TrackPatch): ScoreDoc {
  const d = draft(doc);
  const t = d.tracks[index];
  if (!t) return doc;
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined || value === null) delete (t as unknown as Record<string, unknown>)[key];
    else (t as unknown as Record<string, unknown>)[key] = value;
  }
  return done(d);
}

/**
 * Make a track an instrument track (`input: instrument`) or an audio track again (`input: <n>`).
 * What only the one kind has goes: an audio track's measured latency and stereo pair, an
 * instrument track's `midi:`, reserved `loop:` and its instrument in `mixer:`.
 */
export function setTrackInstrument(doc: ScoreDoc, index: number, instrument: boolean): ScoreDoc {
  const t = doc.tracks[index];
  if (!t || !!t.instrument === instrument) return doc;
  const d = draft(doc);
  const n = d.tracks[index];
  if (instrument) {
    n.instrument = true;
    n.input = 0;
    delete n.input_right;
    delete n.latency;
  } else {
    delete n.instrument;
    delete n.midi;
    delete n.loop;
    const used = new Set(d.tracks.flatMap((x) => (x.instrument ? [] : x.input_right ? [x.input, x.input_right] : [x.input])));
    let input = 1;
    while (used.has(input)) input++;
    n.input = input;
    if (d.mixer) {
      d.mixer.tracks = d.mixer.tracks.map((m) => {
        if (m.track !== n.name || m.instrument === undefined) return m;
        const copy = { ...m };
        delete copy.instrument;
        return copy;
      });
    }
  }
  return done(d);
}

/**
 * The instrument plugin of an instrument track, by id, in its `mixer:` entry (created when the
 * score has none). `undefined` removes the field: the track keeps whatever it plays locally.
 */
export function setTrackInstrumentPlugin(doc: ScoreDoc, track: string, id: string | undefined): ScoreDoc {
  const d = draft(doc);
  const mixer = d.mixer ?? { tracks: [] };
  const at = mixer.tracks.findIndex((m) => m.track === track);
  if (at >= 0) {
    const entry = { ...mixer.tracks[at] };
    if (id === undefined) delete entry.instrument;
    else entry.instrument = id;
    mixer.tracks[at] = entry;
  } else if (id !== undefined) {
    mixer.tracks.push({ track, instrument: id });
    const order = new Map(d.tracks.map((t, i) => [t.name, i]));
    mixer.tracks.sort((a, b) => (order.get(a.track) ?? 99) - (order.get(b.track) ?? 99));
  }
  d.mixer = mixer;
  return done(d);
}

/** Rename a track, its states in every section, its mixer entry and the bindings that name it. */
export function renameTrack(doc: ScoreDoc, index: number, name: string): ScoreDoc {
  const d = draft(doc);
  const t = d.tracks[index];
  if (!t || t.name === name) return doc;
  const old = t.name;
  t.name = name;
  for (const s of d.sections) {
    s.tracks = s.tracks.map((st): DocSectionTrack => (st.track === old ? { ...st, track: name } : st));
  }
  d.midi = remapMidi(d.midi, {
    track: (ref) => ("name" in ref ? (ref.name === old ? name : ref.name) : String(ref.index + 1)),
  });
  if (d.mixer) d.mixer.tracks = d.mixer.tracks.map((m) => (m.track === old ? { ...m, track: name } : m));
  return done(d);
}

export function moveTrack(doc: ScoreDoc, from: number, to: number): ScoreDoc {
  if (from === to || to < 0 || to >= doc.tracks.length || from < 0 || from >= doc.tracks.length) return doc;
  const d = draft(doc);
  d.tracks = move(d.tracks, from, to);
  d.midi = remapMidi(d.midi, {
    track: (ref) => ("name" in ref ? ref.name : String(moveIndex(ref.index, from, to) + 1)),
  });
  d.sections.forEach((_, i) => sortStates(d, i));
  return done(d);
}

/** Delete a track, its states, its mixer entry and every binding that names it. */
export function deleteTrack(doc: ScoreDoc, index: number): ScoreDoc {
  const d = draft(doc);
  const t = d.tracks[index];
  if (!t) return doc;
  d.tracks.splice(index, 1);
  for (const s of d.sections) s.tracks = s.tracks.filter((st) => st.track !== t.name);
  if (d.mixer) d.mixer.tracks = d.mixer.tracks.filter((m) => m.track !== t.name);
  d.midi = remapMidi(d.midi, {
    track: (ref) =>
      "name" in ref
        ? ref.name === t.name
          ? null
          : ref.name
        : ref.index === index
          ? null
          : String((ref.index > index ? ref.index - 1 : ref.index) + 1),
  });
  return done(d);
}

/** For the inspector: the value a section field falls back to when its own is removed. */
export { inheritedField, inheritedState };
