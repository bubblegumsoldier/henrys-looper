// Types mirroring docs/contracts-v0.md

export type TrackState = "record" | "overdub" | "play" | "stop" | "hear_through";

export interface ScoreTrack {
  name: string;
  type: "single" | "group";
  /** single tracks only: 0-based index in the Ableton set. */
  ableton_track?: number | null;
  /** group tracks only: name of the Ableton group track plus the size of its layer pool. */
  group?: string | null;
  layers?: number | null;
  reserve?: number | null;
  monitor?: boolean | null;
}

/** Live occupancy of a group track's layer child tracks (from the runner's state event). */
export interface GroupLayers {
  layers_used: number;
  layers_free: number;
}

/** What ensure_pool() prepared in Ableton when a score with group tracks was loaded. */
export interface PoolGroupResult {
  track: string;
  group: string;
  group_index: number;
  existing_layers: string[];
  created_layers: string[];
  monitor: string | null;
  monitor_created: boolean;
  layer_indices: number[];
  monitor_index: number | null;
  strays: string[];
  warnings: string[];
  message: string;
}

export interface PoolReport {
  changed: boolean;
  groups: PoolGroupResult[];
  messages: string[];
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
  /** Group tracks report how many of their layer child tracks are taken (M4). */
  groups?: Record<string, GroupLayers>;
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

/** POST /api/load additionally reports what was prepared in Ableton (group tracks only). */
export type LoadResult = CompileResult & { state?: StateEvent; pool?: PoolReport };

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
