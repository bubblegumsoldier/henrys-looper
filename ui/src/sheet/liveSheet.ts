//! Which sheet the Live view shows.
//!
//! The runner plays the score that was loaded; the sheet is only text beside it. So an edit to the
//! sheet in the Arrangement should show on stage without a Reload (which resets every track): when
//! the open document has the same sections as the loaded score - same ids, bars and autorelease,
//! i.e. the spans mean the same - its sheet is shown, otherwise the loaded score's own.

import { useSyncExternalStore } from "react";
import { docStore } from "../arrangement/useDoc";
import { useLoadedScore } from "../shell/scoreModel";
import type { CompiledScore, CompiledSection, CompiledSheet } from "../types";

/** True when two compiled scores have sections that the sheet maps the same way. */
export function sameSections(a: CompiledScore, b: CompiledScore): boolean {
  if (a.sections.length !== b.sections.length) return false;
  return a.sections.every((s, i) => {
    const t = b.sections[i];
    return s.id === t.id && s.bars === t.bars && s.autorelease === t.autorelease;
  });
}

/** The sheet to show and the sections of the loaded score; both null without a loaded score. */
export function pickSheet(
  loaded: CompiledScore | null,
  open: CompiledScore | null,
): { sheet: CompiledSheet | null; sections: CompiledSection[] | null } {
  if (!loaded) return { sheet: null, sections: null };
  const sheet = open && sameSections(open, loaded) ? (open.sheet ?? null) : (loaded.sheet ?? null);
  return { sheet, sections: loaded.sections };
}

const openScore = () => docStore.get().score;

export function useLiveSheet(): { sheet: CompiledSheet | null; sections: CompiledSection[] | null } {
  const loaded = useLoadedScore();
  const open = useSyncExternalStore((l) => docStore.subscribe(() => l()), openScore, openScore);
  return pickSheet(loaded, open);
}
