//! The app's one document store, wired to the backend and to the score file, plus the hooks the
//! components read it and the runner's state with.

import { useEffect, useState } from "react";
import { scoreApi } from "../api";
import { scoreFile, subscribeScoreFile, updateScoreFile } from "../scoreFile";
import { useStatusSlice } from "../status";
import type { LooperStatus, Phase } from "../types";
import { DocStore, type DocState } from "./docStore";

/** The text the store itself last handed to the file layer, so its own echo is not a "replace". */
let echo: string | null = null;

export const docStore = new DocStore(
  { parse: scoreApi.parse, render: scoreApi.render },
  {
    onText: (text) => {
      echo = text;
      updateScoreFile({ text });
    },
  },
);

// The file layer is the authority for which text is open. A text that arrives there from anywhere
// but this store (open, new, the live view) replaces the document and its history.
let started = false;
function start() {
  if (started) return;
  started = true;
  void docStore.replace(scoreFile().text);
  subscribeScoreFile((file) => {
    if (file.text === echo || file.text === docStore.get().text) return;
    void docStore.replace(file.text);
  });
}

export function useDoc(): DocState {
  start();
  const [state, setState] = useState(docStore.get());
  useEffect(() => {
    setState(docStore.get());
    return docStore.subscribe(setState);
  }, []);
  return state;
}

// ---------------------------------------------------------------------------------------------
// The runner, as the grid draws it
// ---------------------------------------------------------------------------------------------

export interface Run {
  /** A score sits in the runner. */
  loaded: boolean;
  phase: Phase | "none";
  /** Count-in or running: the arrangement is locked. */
  playing: boolean;
  section: number | null;
  bar: number;
  beat: number;
  pass: number;
  /** Section the armed change leads to; null for none or for the end. */
  armedSection: number | null;
  armed: boolean;
  armedBars: number;
  armedBeats: number;
  armedText: string;
  countIn: boolean;
  engine: boolean;
  sampleRate: number;
}

const NO_RUN: Run = {
  loaded: false,
  phase: "none",
  playing: false,
  section: null,
  bar: 0,
  beat: 0,
  pass: 0,
  armedSection: null,
  armed: false,
  armedBars: 0,
  armedBeats: 0,
  armedText: "",
  countIn: false,
  engine: false,
  sampleRate: 0,
};

function pickRun(s: LooperStatus): Run {
  const score = s.score;
  if (!score) return { ...NO_RUN, engine: s.running, sampleRate: s.sample_rate };
  return {
    loaded: true,
    phase: score.phase,
    playing: score.phase === "count_in" || score.phase === "running",
    section: score.section,
    bar: score.bar,
    beat: score.beat,
    pass: score.pass,
    armedSection: score.armed?.section ?? null,
    armed: score.armed !== null,
    armedBars: score.armed?.bars ?? 0,
    armedBeats: score.armed?.beats ?? 0,
    armedText: score.armed?.text ?? "",
    countIn: score.armed?.count_in ?? false,
    engine: s.running,
    sampleRate: s.sample_rate,
  };
}

function sameRun(a: Run, b: Run): boolean {
  return (Object.keys(a) as (keyof Run)[]).every((k) => a[k] === b[k]);
}

export function useRun(): Run {
  return useStatusSlice(pickRun, sameRun);
}

/** What the engine says about each track right now, by name: state and layer count. */
export function useEngineTracks(): Record<string, { state: string; layers: number; channels: number }> {
  const sig = useStatusSlice((s) => s.tracks.map((t) => `${t.name}\u0001${t.state}\u0001${t.layers.length}\u0001${t.channels}`).join("\u0002"));
  const out: Record<string, { state: string; layers: number; channels: number }> = {};
  if (sig === "") return out;
  for (const row of sig.split("\u0002")) {
    const [name, state, layers, channels] = row.split("\u0001");
    out[name] = { state, layers: Number(layers), channels: Number(channels) };
  }
  return out;
}
