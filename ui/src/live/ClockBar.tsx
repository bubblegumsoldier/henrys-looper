//! Tempo, time signature and loop length of free looping, right on the Live page - plus "Clear all",
//! which is what unlocks them.
//!
//! One source of truth with the Setup page: a change goes to the engine (`api.setTempo`) and, once
//! the engine took it, into the stored start configuration (`setConfig`), so the next start opens
//! with it. A change that arrives another way (tap tempo by key or MIDI, a tempo knob) is followed
//! into the configuration by `useFollowClock`.
//!
//! The engine only accepts a new grid while every track is empty - a recorded loop is a number of
//! samples, and a different tempo would cut it at the wrong place. So the controls say so and offer
//! the way out next to them instead of silently doing nothing.

import { useEffect, useState } from "react";
import { api } from "../api";
import { confirmAction } from "../dialogs";
import { GLOBAL } from "../midi/addresses";
import { currentConfig, setConfig } from "../setup/config";
import { Icon } from "../shell/Icon";
import { run } from "../shell/notes";
import { T, num } from "../shell/terms";
import { useStatusSlice } from "../status";
import type { Clock } from "./PositionPanel";

const BAR_CHOICES = [1, 2, 4, 8, 16];
const SIGNATURES: [number, number][] = [
  [4, 4],
  [3, 4],
  [6, 8],
  [2, 4],
  [5, 4],
  [7, 8],
];

/** Send a new grid, and keep it for the next start once the engine has it. */
export function applyClock(bpm: number, bpb: number, unit: number, bars: number): void {
  run(async () => {
    await api.setTempo(bpm, bpb, unit, bars);
    setConfig((c) => ({ ...c, bpm, beats_per_bar: bpb, beat_unit: unit, bars }));
  });
}

/** "Clear all", asked first: every loop of every track is gone afterwards. */
export function clearAllAsked(): void {
  void confirmAction(
    "Clear all deletes every layer of every track. The clock goes back to bar 1, and tempo, time signature and loop length can be changed again.",
    T.clearAll,
    T.clearAll,
  ).then((ok) => {
    if (ok) run(() => api.clearAll());
  });
}

/**
 * Keep the stored configuration in step with what the engine runs, however it got there (tap
 * tempo, a MIDI knob). Free looping only: a score brings its own grid and must not overwrite the
 * musician's.
 */
function useFollowClock(clock: Clock, free: boolean): void {
  useEffect(() => {
    if (!free || clock.bpm <= 0 || clock.bpb <= 0) return;
    const c = currentConfig();
    if (c.bpm !== clock.bpm || c.beats_per_bar !== clock.bpb || c.beat_unit !== clock.unit || c.bars !== clock.bars) {
      setConfig({ ...c, bpm: clock.bpm, beats_per_bar: clock.bpb, beat_unit: clock.unit, bars: clock.bars });
    }
  }, [free, clock.bpm, clock.bpb, clock.unit, clock.bars]);
}

function BpmField({ bpm, disabled, onBpm }: { bpm: number; disabled: boolean; onBpm: (v: number) => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState("");
  const commit = () => {
    setEditing(false);
    const v = Number(text.replace(",", "."));
    if (Number.isFinite(v) && v >= 20 && v <= 300 && v !== bpm) onBpm(Math.round(v * 10) / 10);
  };
  if (editing) {
    return (
      <input
        className="cb-bpm-in"
        autoFocus
        inputMode="decimal"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
      />
    );
  }
  return (
    <button
      className="cb-bpm"
      data-midi={GLOBAL.tempo}
      disabled={disabled}
      onClick={() => {
        setText(String(bpm));
        setEditing(true);
      }}
      title="Click to type a tempo · MIDI Learn: map a knob to it"
    >
      <b>{num(bpm)}</b> BPM
    </button>
  );
}

export function ClockBar({ clock, free }: { clock: Clock; free: boolean }) {
  // Locked while anything is recorded, recording or armed - the engine's own rule.
  const locked = useStatusSlice((s) => s.tracks.some((t) => t.state !== "empty"));
  const [otherBars, setOtherBars] = useState(false);
  const [otherSig, setOtherSig] = useState(false);
  useFollowClock(clock, free);

  if (!free) return null;
  const { bpm, bpb, unit, bars } = clock;
  const set = (p: Partial<{ bpm: number; bpb: number; unit: number; bars: number }>) =>
    applyClock(p.bpm ?? bpm, p.bpb ?? bpb, p.unit ?? unit, p.bars ?? bars);
  const step = (e: React.MouseEvent, dir: number) => {
    const next = Math.min(300, Math.max(20, Math.round(bpm) + dir * (e.shiftKey ? 5 : 1)));
    if (next !== bpm) set({ bpm: next });
  };
  const sigKey = `${bpb}/${unit}`;
  const knownSig = SIGNATURES.some(([a, b]) => `${a}/${b}` === sigKey);
  const lockTitle = locked ? "Clear all loops to change the tempo" : undefined;

  return (
    <div className={`clockbar${locked ? " locked" : ""}`}>
      <div className="cb-group" title={lockTitle}>
        <button className="cb-btn" disabled={locked} onClick={(e) => step(e, -1)} title="−1 BPM · Shift −5" aria-label="Slower">
          <Icon name="minus" />
        </button>
        <BpmField bpm={bpm} disabled={locked} onBpm={(v) => set({ bpm: v })} />
        <button className="cb-btn" disabled={locked} onClick={(e) => step(e, 1)} title="+1 BPM · Shift +5" aria-label="Faster">
          <Icon name="plus" />
        </button>
        <button
          className="cb-btn tap"
          data-midi={GLOBAL.tapTempo}
          disabled={locked}
          onClick={() => run(() => api.tapTempo())}
          title="Tap tempo · key T. From the third tap on the tempo follows your taps; the click keeps its place."
        >
          {T.tap}
        </button>
      </div>

      <div className="cb-group" title={lockTitle}>
        {otherSig || !knownSig ? (
          <span className="cb-free">
            <input
              className="cb-num"
              type="number"
              min={1}
              max={16}
              disabled={locked}
              defaultValue={bpb}
              onBlur={(e) => {
                const v = Math.round(Number(e.target.value));
                if (v >= 1 && v <= 16 && v !== bpb) set({ bpb: v });
              }}
              aria-label="Beats per bar"
            />
            /
            <select className="cb-sel" disabled={locked} value={unit} onChange={(e) => set({ unit: Number(e.target.value) })} aria-label="Beat unit">
              {[2, 4, 8, 16].map((u) => (
                <option key={u} value={u}>
                  {u}
                </option>
              ))}
            </select>
            <button className="cb-btn s" onClick={() => setOtherSig(false)} title="Back to the list" aria-label="Close">
              <Icon name="x" />
            </button>
          </span>
        ) : (
          <select
            className="cb-sel"
            disabled={locked}
            value={sigKey}
            onChange={(e) => {
              if (e.target.value === "other") return setOtherSig(true);
              const [a, b] = e.target.value.split("/").map(Number);
              set({ bpb: a, unit: b });
            }}
            aria-label={T.timeSig}
          >
            {SIGNATURES.map(([a, b]) => (
              <option key={`${a}/${b}`} value={`${a}/${b}`}>
                {a}/{b}
              </option>
            ))}
            <option value="other">other…</option>
          </select>
        )}

        {otherBars || !BAR_CHOICES.includes(bars) ? (
          <span className="cb-free">
            <input
              className="cb-num"
              type="number"
              min={1}
              max={64}
              disabled={locked}
              defaultValue={bars}
              onBlur={(e) => {
                const v = Math.round(Number(e.target.value));
                if (v >= 1 && v <= 64 && v !== bars) set({ bars: v });
                setOtherBars(false);
              }}
              aria-label="Loop length in bars"
            />
            {T.bars}
          </span>
        ) : (
          <select
            className="cb-sel"
            disabled={locked}
            value={bars}
            onChange={(e) => {
              if (e.target.value === "other") return setOtherBars(true);
              set({ bars: Number(e.target.value) });
            }}
            aria-label="Loop length"
          >
            {BAR_CHOICES.map((b) => (
              <option key={b} value={b}>
                {b} {b === 1 ? "bar" : T.bars}
              </option>
            ))}
            <option value="other">other…</option>
          </select>
        )}
      </div>

      <button className="cb-btn danger clear" data-midi={GLOBAL.clearAll} onClick={clearAllAsked} title="Delete every layer of every track · Shift+Backspace">
        <Icon name="trash" />
        {T.clearAll}
      </button>
      {locked && <div className="cb-note">{T.tempoLocked}</div>}
    </div>
  );
}
