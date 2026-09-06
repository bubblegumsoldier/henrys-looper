// Types mirroring docs/contracts-v0.md

export type TrackState = "record" | "overdub" | "play" | "stop" | "hear_through";

export interface ScoreTrack {
  name: string;
  ableton_track: number;
  type: "single" | "group";
}

export interface ScoreSection {
  id: string;
  index: number;
  bars: number;
  autorelease: boolean;
  quantize: "bar" | "loop";
  tracks: Record<string, TrackState>;
  source_line: number | null;
}

export interface Score {
  title: string;
  bpm: number;
  beats_per_bar: number;
  tracks: ScoreTrack[];
  midi: Record<string, { id: string }>;
  sections: ScoreSection[];
}

export interface ScoreErr {
  line: number | null;
  column: number | null;
  message: string;
  suggestion: string | null;
}

export interface StateEvent {
  type: "state";
  running: boolean;
  section_index: number | null;
  section_id: string | null;
  bars_total: number | null;
  bar: number;
  beat: number;
  pending: "next" | null;
  tracks: Record<string, TrackState>;
  connected: boolean;
  engine: "sim" | "ableton" | string;
  /** Set by the real runner while waiting for the first bar boundary (not in the v0 contract). */
  countin?: boolean;
}

/** True while the transport runs but the first section has not started yet. */
export function isCountIn(s: StateEvent): boolean {
  return s.running && (s.countin === true || s.section_index === null || s.section_index < 0);
}

export interface BeatEvent {
  type: "beat";
  song_beat: number;
  bar: number;
  beat: number;
}

export interface MidiEvent {
  type: "midi";
  device: string;
  channel: number;
  kind: "note_on" | "note_off" | "cc" | string;
  number: number;
  value: number;
  id: string;
}

export interface LogEvent {
  type: "log";
  level: "info" | "warn" | "error" | string;
  message: string;
}

export type LooperEvent = StateEvent | BeatEvent | MidiEvent | LogEvent;

export interface MidiRow extends MidiEvent {
  seq: number;
  ts: number;
}

export interface LogRow extends LogEvent {
  seq: number;
  ts: number;
}

export type CompileResult = { ok: true; score: Score } | { ok: false; errors: ScoreErr[] };

export const IDLE_STATE: StateEvent = {
  type: "state",
  running: false,
  section_index: null,
  section_id: null,
  bars_total: null,
  bar: 0,
  beat: 0,
  pending: null,
  tracks: {},
  connected: false,
  engine: "sim",
};
