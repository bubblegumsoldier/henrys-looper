//! Tempo, time signature, click level and monitor level while the engine runs - a deliberately
//! plain row. The values come from the status, so a change by MIDI or by a loaded score shows up
//! here as well.

import { useEffect, useState } from "react";
import { api } from "../api";
import { useStatusSlice } from "../status";

interface Props {
  /** Every command goes through the caller, so a refusal ends up in its notes. */
  run: (work: () => Promise<unknown>) => void;
}

export function SessionControls({ run }: Props) {
  const bpm = useStatusSlice((s) => s.bpm);
  const signature = useStatusSlice((s) => `${s.beats_per_bar}/${s.beat_unit}`);
  const clickGain = useStatusSlice((s) => s.click_gain);
  const monitorGain = useStatusSlice((s) => s.monitor_gain);
  const empty = useStatusSlice((s) => s.tracks.every((t) => t.state === "empty") && s.score === null);

  const [bpmText, setBpmText] = useState(String(bpm));
  const [sigText, setSigText] = useState(signature);
  useEffect(() => setBpmText(String(Math.round(bpm * 100) / 100)), [bpm]);
  useEffect(() => setSigText(signature), [signature]);

  const applyTempo = () => {
    const match = /^\s*(\d{1,2})\s*\/\s*(\d{1,2})\s*$/.exec(sigText);
    const value = Number(bpmText.replace(",", "."));
    if (!match || !Number.isFinite(value)) {
      run(() => Promise.reject(new Error("Tempo: enter BPM as a number and the time signature as e.g. 3/4.")));
      return;
    }
    run(() => api.setTempo(value, Number(match[1]), Number(match[2])));
  };

  return (
    <section className="session-controls">
      <label title={empty ? "Tempo and time signature" : "Only while every track is empty and no score is loaded"}>
        Tempo
        <input
          className="session-bpm"
          value={bpmText}
          onChange={(e) => setBpmText(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && applyTempo()}
          inputMode="decimal"
        />
        BPM
        <input
          className="session-sig"
          value={sigText}
          onChange={(e) => setSigText(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && applyTempo()}
        />
        <button className="btn" onClick={applyTempo} disabled={!empty}>
          Set
        </button>
      </label>
      <Level label="Click level" value={clickGain} onChange={(gain) => run(() => api.clickGain(gain))} />
      <Level label="Monitor level" value={monitorGain} onChange={(gain) => run(() => api.monitorGain(gain))} />
    </section>
  );
}

function Level({ label, value, onChange }: { label: string; value: number; onChange: (gain: number) => void }) {
  return (
    <label className="session-level" title={`${label}: linear 0 to 4, 1 = unity`}>
      {label}
      <input type="range" min={0} max={2} step={0.05} value={value} onChange={(e) => onChange(Number(e.target.value))} />
      <span className="mono">{value.toFixed(2)}</span>
    </label>
  );
}
