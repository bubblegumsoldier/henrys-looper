import { useState } from "react";
import type { LogRow, MidiRow } from "../types";

interface Props {
  rows: MidiRow[];
  logs: LogRow[];
  onClear: () => void;
}

async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* fall through */
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

function fmtTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString("de-DE", { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0").slice(0, 1);
}

export function MidiMonitor({ rows, logs, onClear }: Props) {
  const [copied, setCopied] = useState<number | null>(null);

  const copy = async (row: MidiRow) => {
    const ok = await copyText(row.id);
    if (ok) {
      setCopied(row.seq);
      window.setTimeout(() => setCopied((c) => (c === row.seq ? null : c)), 1200);
    }
  };

  return (
    <aside className="midi">
      <div className="panel-title">
        MIDI-Monitor
        <span className="muted">{rows.length} Events</span>
        <button className="btn btn-mini" onClick={onClear} disabled={!rows.length}>
          Leeren
        </button>
      </div>
      <div className="midi-table-wrap">
        <table className="midi-table">
          <thead>
            <tr>
              <th>Device</th>
              <th>Kanal</th>
              <th>Art</th>
              <th>Nr.</th>
              <th>Wert</th>
              <th>ID</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 && (
              <tr>
                <td colSpan={7} className="muted center">
                  Noch keine MIDI-Events.
                </td>
              </tr>
            )}
            {rows.map((r) => (
              <tr key={r.seq} className={`kind-${r.kind}`} title={fmtTime(r.ts)}>
                <td className="ellipsis" title={r.device}>
                  {r.device}
                </td>
                <td className="mono">{r.channel}</td>
                <td className="mono">{r.kind}</td>
                <td className="mono">{r.number}</td>
                <td className="mono">{r.value}</td>
                <td className="mono midi-id">{r.id}</td>
                <td>
                  <button className="btn btn-mini" onClick={() => copy(r)} title="MIDI-ID in die Zwischenablage kopieren">
                    {copied === r.seq ? "kopiert ✓" : "ID kopieren"}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="panel-title log-title">Log</div>
      <div className="log">
        {logs.length === 0 && <div className="muted">Noch keine Log-Einträge.</div>}
        {logs.map((l) => (
          <div key={l.seq} className={`log-row log-${l.level}`}>
            <span className="mono muted">{fmtTime(l.ts)}</span>
            <span className={`log-level`}>{l.level}</span>
            <span className="log-msg">{l.message}</span>
          </div>
        ))}
      </div>
    </aside>
  );
}
