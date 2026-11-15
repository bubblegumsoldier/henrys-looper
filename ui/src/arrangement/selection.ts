//! What is selected in the arrangement. One value, so the grid, the inspector and the keyboard can
//! never disagree about it.

import type { ScoreDoc } from "../types";
import type { Cell } from "./ops";

export type Selection =
  | { kind: "score" }
  | { kind: "section"; index: number }
  | { kind: "track"; index: number }
  /** `anchor` is where a shift-extension starts, `focus` where the arrow keys move from. */
  | { kind: "cells"; cells: Cell[]; anchor: Cell; focus: Cell };

export const NONE: Selection = { kind: "score" };

export function cellKey(c: Cell): string {
  return `${c.section}:${c.track}`;
}

/** Every cell in the rectangle between two corners. */
export function rect(a: Cell, b: Cell): Cell[] {
  const out: Cell[] = [];
  for (let s = Math.min(a.section, b.section); s <= Math.max(a.section, b.section); s++) {
    for (let t = Math.min(a.track, b.track); t <= Math.max(a.track, b.track); t++) out.push({ section: s, track: t });
  }
  return out;
}

export function single(c: Cell): Selection {
  return { kind: "cells", cells: [c], anchor: c, focus: c };
}

/** Click with modifiers: plain replaces, shift extends a rectangle, ⌘/Ctrl toggles one cell. */
export function clickCell(sel: Selection, c: Cell, shift: boolean, toggle: boolean): Selection {
  if (sel.kind === "cells" && shift) return { kind: "cells", cells: rect(sel.anchor, c), anchor: sel.anchor, focus: c };
  if (sel.kind === "cells" && toggle) {
    const has = sel.cells.some((x) => cellKey(x) === cellKey(c));
    const cells = has ? sel.cells.filter((x) => cellKey(x) !== cellKey(c)) : [...sel.cells, c];
    return cells.length === 0 ? NONE : { kind: "cells", cells, anchor: c, focus: c };
  }
  return single(c);
}

/** Arrow keys. Shift extends from the anchor. */
export function moveFocus(sel: Selection, ds: number, dt: number, shift: boolean, doc: ScoreDoc): Selection {
  const clamp = (c: Cell): Cell => ({
    section: Math.max(0, Math.min(doc.sections.length - 1, c.section)),
    track: Math.max(0, Math.min(doc.tracks.length - 1, c.track)),
  });
  if (doc.sections.length === 0 || doc.tracks.length === 0) return sel;
  if (sel.kind === "section") {
    if (dt !== 0) return single(clamp({ section: sel.index, track: dt > 0 ? 0 : doc.tracks.length - 1 }));
    return { kind: "section", index: Math.max(0, Math.min(doc.sections.length - 1, sel.index + ds)) };
  }
  if (sel.kind === "track") {
    if (ds !== 0) return single(clamp({ section: ds > 0 ? 0 : doc.sections.length - 1, track: sel.index }));
    return { kind: "track", index: Math.max(0, Math.min(doc.tracks.length - 1, sel.index + dt)) };
  }
  if (sel.kind !== "cells") return single({ section: 0, track: 0 });
  const focus = clamp({ section: sel.focus.section + ds, track: sel.focus.track + dt });
  if (shift) return { kind: "cells", cells: rect(sel.anchor, focus), anchor: sel.anchor, focus };
  return single(focus);
}

/** Sections touched by the selection, for the column highlight. */
export function selectedSections(sel: Selection): Set<number> {
  if (sel.kind === "section") return new Set([sel.index]);
  if (sel.kind === "cells") return new Set(sel.cells.map((c) => c.section));
  return new Set();
}

/** Drop what no longer exists after an edit (a deleted section or track). */
export function prune(sel: Selection, doc: ScoreDoc | null): Selection {
  if (!doc) return sel;
  if (sel.kind === "section") return sel.index < doc.sections.length ? sel : NONE;
  if (sel.kind === "track") return sel.index < doc.tracks.length ? sel : NONE;
  if (sel.kind === "cells") {
    const ok = (c: Cell) => c.section < doc.sections.length && c.track < doc.tracks.length;
    const cells = sel.cells.filter(ok);
    if (cells.length === 0) return NONE;
    if (cells.length === sel.cells.length && ok(sel.anchor) && ok(sel.focus)) return sel;
    return { kind: "cells", cells, anchor: cells[0], focus: cells[cells.length - 1] };
  }
  return sel;
}
