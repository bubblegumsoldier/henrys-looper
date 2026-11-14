//! Which file the score text belongs to, and whether it has unsaved changes.
//!
//! Outside React on purpose: the score view is unmounted when the live tab is open, but the window
//! title and the "unsaved changes" question on closing the window must keep working. The text
//! itself is mirrored to `localStorage` as before (`looper.yaml`) - not as the place the score
//! lives, but so a crash or a reload never loses typing that was not saved yet.

import { useEffect, useState } from "react";

export interface ScoreFileState {
  /** Current editor text. */
  text: string;
  /** File the text belongs to; null for an untitled score. */
  path: string | null;
  /** Its file name, for the title. */
  name: string | null;
  /** Text as last opened or saved; null when this text has never been saved anywhere. */
  savedText: string | null;
}

const TEXT_KEY = "looper.yaml";
const FILE_KEY = "looper.file";

function load(): ScoreFileState {
  let text = "";
  let file: Partial<ScoreFileState> = {};
  try {
    text = localStorage.getItem(TEXT_KEY) ?? "";
    file = JSON.parse(localStorage.getItem(FILE_KEY) ?? "{}") as Partial<ScoreFileState>;
  } catch {
    /* private mode or garbage: start empty */
  }
  return {
    text,
    path: file.path ?? null,
    name: file.name ?? null,
    savedText: file.savedText ?? null,
  };
}

let state: ScoreFileState = load();
const listeners = new Set<(s: ScoreFileState) => void>();

export function scoreFile(): ScoreFileState {
  return state;
}

/** Whether closing, opening or "New" would lose something. */
export function isDirty(s: ScoreFileState = state): boolean {
  return s.savedText === null ? s.text.trim() !== "" : s.text !== s.savedText;
}

export function updateScoreFile(change: Partial<ScoreFileState>): void {
  state = { ...state, ...change };
  try {
    localStorage.setItem(TEXT_KEY, state.text);
    localStorage.setItem(
      FILE_KEY,
      JSON.stringify({ path: state.path, name: state.name, savedText: state.savedText }),
    );
  } catch {
    /* quota: the file on disk is what counts */
  }
  for (const l of listeners) l(state);
}

export function subscribeScoreFile(listener: (s: ScoreFileState) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useScoreFile(): ScoreFileState {
  const [value, setValue] = useState(state);
  useEffect(() => {
    setValue(state);
    return subscribeScoreFile(setValue);
  }, []);
  return value;
}
