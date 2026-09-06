import { isCountIn, type Score, type StateEvent } from "../types";
import type { WsStatus } from "../useWebSocket";

interface Props {
  state: StateEvent;
  score: Score | null;
  wsStatus: WsStatus;
  engine: string;
  engineBusy: boolean;
  onEngine: (name: string) => void;
  onStart: () => void;
  onNext: () => void;
  onStopAll: () => void;
}

export function Transport({ state, score, wsStatus, engine, engineBusy, onEngine, onStart, onNext, onStopAll }: Props) {
  const total = score?.sections.length ?? 0;
  const countIn = isCountIn(state);
  const secNo = state.section_index !== null && state.section_index >= 0 ? state.section_index + 1 : null;

  let position: string;
  if (!state.running) position = "Bereit";
  else if (countIn) position = "Einzähler";
  else position = `Sektion ${secNo}/${total} · Takt ${state.bar}/${state.bars_total ?? "?"}`;

  const beatLabel = state.running && state.beat > 0 ? String(state.beat) : "–";

  return (
    <header className="transport">
      <div className="transport-left">
        <div className="brand">Henrys Looper</div>
        <label className="engine">
          <span className={`dot ${state.connected ? "dot-ok" : "dot-off"}`} title={state.connected ? "Engine verbunden" : "Engine nicht verbunden"} />
          <select value={engine} disabled={engineBusy || state.running} onChange={(e) => onEngine(e.target.value)}>
            <option value="sim">sim</option>
            <option value="ableton">ableton</option>
          </select>
        </label>
        <span className={`ws ws-${wsStatus}`} title="WebSocket-Verbindung zum Backend">
          <span className={`dot ${wsStatus === "open" ? "dot-ok" : wsStatus === "connecting" ? "dot-warn" : "dot-err"}`} />
          {wsStatus === "open" ? "verbunden" : wsStatus === "connecting" ? "verbinde…" : "getrennt"}
        </span>
      </div>

      <div className="transport-buttons">
        <button className="btn btn-start" onClick={onStart} disabled={state.running || !score}>
          Start
        </button>
        <button className="btn btn-next" onClick={onNext} disabled={!state.running || countIn} title="Leertaste oder N">
          Next Section
          {state.pending === "next" && <span className="armed-tag">armiert</span>}
        </button>
        <button className="btn btn-stop" onClick={onStopAll} disabled={!state.running}>
          Stop All
        </button>
      </div>

      <div className={`position ${countIn ? "position-countin" : ""} ${state.pending ? "position-pending" : ""}`}>
        <div className="position-text">
          {position}
          {state.running && state.section_id && !countIn && <span className="position-id">{state.section_id}</span>}
        </div>
        <div className="beat-big" key={`${state.bar}-${state.beat}`}>
          {beatLabel}
        </div>
      </div>
    </header>
  );
}
