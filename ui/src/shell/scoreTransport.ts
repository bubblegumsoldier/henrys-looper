//! The score's transport as the new frame uses it: one flattened view of the runner, the three
//! commands, and the keys the CLI has (Space or N = start/release, Shift+S = stop all).
//!
//! Without a score Space is the Start/Stop of free looping instead (`freeTransport`); N and
//! Shift+S do nothing there. With a score loaded Space keeps its meaning exactly as before.
//!
//! The keys are installed by the frame only while the Arrangement placeholder is *not* showing:
//! that one still embeds the score view with its own transport, which listens to the same keys, and
//! one press must never reach the runner twice.

import { useEffect } from "react";
import { api, scoreApi } from "../api";
import { useStatusSlice } from "../status";
import { run, say } from "./notes";
import { confirmAction } from "../dialogs";
import type { LooperStatus, Phase } from "../types";

export interface ScoreView {
  loaded: boolean;
  phase: Phase | "none";
  section: number | null;
  sectionId: string;
  sectionCount: number;
  bar: number;
  barsTotal: number;
  beat: number;
  pass: number;
  title: string;
  armed: {
    label: string;
    section: number | null;
    bars: number;
    beats: number;
    countIn: boolean;
    text: string;
  } | null;
}

const NONE: ScoreView = {
  loaded: false,
  phase: "none",
  section: null,
  sectionId: "",
  sectionCount: 0,
  bar: 0,
  barsTotal: 0,
  beat: 0,
  pass: 0,
  title: "",
  armed: null,
};

function pick(s: LooperStatus): ScoreView {
  const score = s.score;
  if (!score) return NONE;
  const a = score.armed;
  return {
    loaded: true,
    phase: score.phase,
    section: score.section,
    sectionId: score.section_id,
    sectionCount: score.section_count,
    bar: score.bar,
    barsTotal: score.bars_total,
    beat: score.beat,
    pass: score.pass,
    title: score.title,
    armed: a
      ? { label: a.label, section: a.section, bars: a.bars, beats: a.beats, countIn: a.count_in, text: a.text }
      : null,
  };
}

function same(a: ScoreView, b: ScoreView): boolean {
  if (
    a.loaded !== b.loaded ||
    a.phase !== b.phase ||
    a.section !== b.section ||
    a.sectionId !== b.sectionId ||
    a.sectionCount !== b.sectionCount ||
    a.bar !== b.bar ||
    a.barsTotal !== b.barsTotal ||
    a.beat !== b.beat ||
    a.pass !== b.pass ||
    a.title !== b.title
  )
    return false;
  if (a.armed === null || b.armed === null) return a.armed === b.armed;
  return (
    a.armed.label === b.armed.label &&
    a.armed.section === b.armed.section &&
    a.armed.bars === b.armed.bars &&
    a.armed.beats === b.armed.beats &&
    a.armed.countIn === b.armed.countIn &&
    a.armed.text === b.armed.text
  );
}

export function useScore(): ScoreView {
  return useStatusSlice(pick, same);
}

/** True while the score owns the transport: started and not over. */
export function scoreRuns(v: ScoreView): boolean {
  return v.phase === "count_in" || v.phase === "running";
}

export function scoreCanStart(v: ScoreView): boolean {
  return v.loaded && v.phase === "idle";
}

/** Start (`true`), stop (`false`) or toggle (`null`) the clock of free looping. */
export const freeTransport = (on: boolean | null) => run(() => api.transport(on));

/** What the free-looping keys need to know: is there an engine, and does its clock run. */
function pickFree(s: LooperStatus): number {
  return (s.running ? 1 : 0) + (s.transport ? 2 : 0);
}

export const scoreStart = () => say(() => scoreApi.start());
export const scoreNext = () => say(() => scoreApi.next());
export const scoreStopAll = () => say(() => scoreApi.stopAll());
/** Back before the first section - clears every track, so it asks first. */
export const scoreRewind = () =>
  void confirmAction(
    "Back to start clears ALL tracks - every recorded loop is gone - and returns to the first section.",
    "Back to start",
    "Clear tracks & rewind",
  ).then((yes) => {
    if (yes) say(() => scoreApi.rewind());
  });

function typing(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  if (!element || !element.tagName) return false;
  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    element.isContentEditable ||
    element.closest(".cm-editor") !== null
  );
}

export function useScoreKeys(enabled: boolean): void {
  const view = useScore();
  const runs = scoreRuns(view);
  const canStart = scoreCanStart(view);
  const loaded = view.loaded;
  const free = useStatusSlice(pickFree);
  const engineOn = (free & 1) !== 0;
  const ticking = (free & 2) !== 0;
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
      if (typing(e.target)) return;
      const key = e.key.toLowerCase();
      if (!loaded) {
        // Free looping: Space starts and stops the clock. Nothing else is the score's business.
        if (key === " " && !e.shiftKey && engineOn) {
          e.preventDefault();
          freeTransport(!ticking);
        }
        return;
      }
      if ((key === " " || key === "n") && !e.shiftKey) {
        e.preventDefault();
        if (runs) scoreNext();
        else if (canStart) scoreStart();
        return;
      }
      if (key === "s" && e.shiftKey && runs) {
        e.preventDefault();
        scoreStopAll();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, runs, canStart, loaded, engineOn, ticking]);
}
