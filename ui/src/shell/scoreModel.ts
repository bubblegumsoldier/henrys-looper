//! The compiled form of the score the runner holds - what the Live arrangement strip, the position
//! block and the "next" row of each tile read section lengths and target states from.
//!
//! The status event carries the runner's *position* (section, bar, pass, armed change) but not the
//! sections themselves. Until the score model API offers them directly, this store fills itself:
//! when a score appears in the status, the text the editor last held is compiled again and accepted
//! only if title and section count agree with what the runner reports. `setLoadedScore` is the
//! direct way in, for whoever loads a score and already has the compiled result.

import { useEffect, useSyncExternalStore } from "react";
import { scoreApi } from "../api";
import { scoreFile } from "../scoreFile";
import { useStatusSlice } from "../status";
import type { CompiledScore } from "../types";

let loaded: CompiledScore | null = null;
const listeners = new Set<() => void>();

export function setLoadedScore(score: CompiledScore | null): void {
  loaded = score;
  for (const l of listeners) l();
}

export function useLoadedScore(): CompiledScore | null {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => loaded,
  );
}

/** Mount once: keeps the store in step with the runner. */
export function useScoreModelSync(): void {
  const key = useStatusSlice((s) => (s.score ? `${s.score.title}\u0000${s.score.section_count}` : ""));
  useEffect(() => {
    if (key === "") {
      setLoadedScore(null);
      return;
    }
    const [title, count] = key.split("\u0000");
    if (loaded && loaded.title === title && loaded.sections.length === Number(count)) return;
    const text = scoreFile().text;
    if (!text.trim()) return;
    let cancelled = false;
    scoreApi
      .compile(text)
      .then((result) => {
        if (cancelled || !result.ok || !result.score) return;
        if (result.score.title === title && result.score.sections.length === Number(count)) {
          setLoadedScore(result.score);
        }
      })
      .catch(() => {
        /* the strip simply stays minimal */
      });
    return () => {
      cancelled = true;
    };
  }, [key]);
}
