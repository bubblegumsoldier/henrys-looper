import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "./api";
import { Editor } from "./components/Editor";
import { Transport } from "./components/Transport";
import { Blocks } from "./components/Blocks";
import { MidiMonitor } from "./components/MidiMonitor";
import { useWebSocket } from "./useWebSocket";
import { IDLE_STATE, type LogRow, type LooperEvent, type MidiRow, type Score, type ScoreErr, type StateEvent } from "./types";

const LS_KEY = "looper.yaml";
const MAX_ROWS = 200;

// Monotonic row id; seeded from the clock so it stays unique across Vite HMR module reloads.
let seq = Date.now();

export default function App() {
  const [yamlText, setYamlText] = useState<string>(() => {
    try {
      return localStorage.getItem(LS_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [editorValue, setEditorValue] = useState<string>(yamlText); // value pushed INTO the editor
  const [compiled, setCompiled] = useState<Score | null>(null);
  const [loadedJson, setLoadedJson] = useState<string>("");
  const [errors, setErrors] = useState<ScoreErr[]>([]);
  const [state, setState] = useState<StateEvent>(IDLE_STATE);
  const [engine, setEngine] = useState<string>("sim");
  const [engineBusy, setEngineBusy] = useState(false);
  const [midiRows, setMidiRows] = useState<MidiRow[]>([]);
  const [logs, setLogs] = useState<LogRow[]>([]);
  const [banner, setBanner] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const bannerTimer = useRef<number | undefined>(undefined);

  const pushLog = useCallback((level: string, message: string) => {
    setLogs((l) => [{ type: "log" as const, level, message, seq: ++seq, ts: Date.now() }, ...l].slice(0, MAX_ROWS));
  }, []);

  const showError = useCallback(
    (msg: string, alreadyLogged = false) => {
      setBanner(msg);
      // Backend errors (HTTP 4xx/5xx) already arrive as `log` events over the WebSocket.
      if (!alreadyLogged) pushLog("error", msg);
      if (bannerTimer.current) window.clearTimeout(bannerTimer.current);
      bannerTimer.current = window.setTimeout(() => setBanner(null), 8000);
    },
    [pushLog],
  );

  // --- WebSocket events ---------------------------------------------------
  const onEvent = useCallback(
    (ev: LooperEvent) => {
      switch (ev.type) {
        case "state":
          setState(ev);
          if (ev.engine) setEngine(ev.engine);
          break;
        case "beat":
          setState((s) => (s.running ? { ...s, bar: ev.bar, beat: ev.beat } : s));
          break;
        case "midi":
          setMidiRows((rows) => [{ ...ev, seq: ++seq, ts: Date.now() }, ...rows].slice(0, MAX_ROWS));
          break;
        case "log":
          setLogs((l) => [{ ...ev, seq: ++seq, ts: Date.now() }, ...l].slice(0, MAX_ROWS));
          break;
      }
    },
    [],
  );
  const wsStatus = useWebSocket(onEvent);

  // --- persist editor text ------------------------------------------------
  useEffect(() => {
    try {
      localStorage.setItem(LS_KEY, yamlText);
    } catch {
      /* ignore quota errors */
    }
  }, [yamlText]);

  // --- actions ------------------------------------------------------------
  const compile = useCallback(
    async (text: string, quiet = false): Promise<Score | null> => {
      try {
        const res = await api.compile(text);
        if (res.ok) {
          setErrors([]);
          setCompiled(res.score);
          if (!quiet) pushLog("info", `Kompiliert: ${res.score.sections.length} Sektionen, ${res.score.tracks.length} Tracks.`);
          return res.score;
        }
        setErrors(res.errors);
        if (!quiet) pushLog("warn", `Kompilieren: ${res.errors.length} Fehler.`);
        return null;
      } catch (e) {
        showError((e as Error).message);
        return null;
      }
    },
    [pushLog, showError],
  );

  const load = useCallback(async () => {
    setBusy(true);
    try {
      const res = await api.load(yamlText);
      if (res.ok) {
        setErrors([]);
        setCompiled(res.score);
        setLoadedJson(JSON.stringify(res.score));
        if (res.state) setState(res.state);
      } else {
        setErrors(res.errors);
        pushLog("warn", `Laden abgebrochen: ${res.errors.length} Fehler.`);
      }
    } catch (e) {
      showError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }, [yamlText, pushLog, showError]);

  const transport = useCallback(
    async (action: "start" | "stop_all" | "next") => {
      try {
        const res = await api.transport(action);
        if (res.state) setState(res.state);
      } catch (e) {
        showError((e as Error).message);
      }
    },
    [showError],
  );

  const changeEngine = useCallback(
    async (name: string) => {
      setEngineBusy(true);
      try {
        const res = await api.setEngine(name);
        setEngine(res.current);
      } catch (e) {
        const err = e as ApiError;
        showError(err.message, err.status === 503);
        try {
          const cur = await api.engines();
          setEngine(cur.current);
        } catch {
          /* keep current */
        }
      } finally {
        setEngineBusy(false);
      }
    },
    [showError],
  );

  // --- initial load (re-run whenever the WebSocket (re)connects and we have no score yet) ---
  const booted = useRef(false);
  const yamlRef = useRef(yamlText);
  yamlRef.current = yamlText;
  useEffect(() => {
    if (booted.current || wsStatus !== "open") return;
    let cancelled = false;
    (async () => {
      try {
        const [srv, st, eng] = await Promise.all([api.score(), api.state(), api.engines()]);
        if (cancelled) return;
        booted.current = true;
        setState(st);
        setEngine(eng.current);
        if (srv.score) setLoadedJson(JSON.stringify(srv.score));
        const current = yamlRef.current;
        const text = current.trim() ? current : srv.yaml;
        if (text !== current) {
          setYamlText(text);
          setEditorValue(text);
        }
        if (srv.stub) pushLog("warn", "Backend läuft gegen den Stub (kein echtes looper-Paket).");
        await compile(text, true);
      } catch (e) {
        if (!cancelled) showError((e as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [wsStatus, compile, pushLog, showError]);

  // --- keyboard shortcuts (outside editor / inputs) -----------------------
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (!t) return;
      const tag = t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t.isContentEditable || t.closest(".cm-editor")) return;
      if (e.key === " " || e.key.toLowerCase() === "n") {
        e.preventDefault();
        if (state.running) void transport("next");
      } else if (e.key === "Escape") {
        if (state.running) void transport("stop_all");
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [state.running, transport]);

  const stale = !!compiled && loadedJson !== "" && JSON.stringify(compiled) !== loadedJson;

  return (
    <div className="app">
      <Transport
        state={state}
        score={compiled}
        wsStatus={wsStatus}
        engine={engine}
        engineBusy={engineBusy}
        onEngine={changeEngine}
        onStart={() => transport("start")}
        onNext={() => transport("next")}
        onStopAll={() => transport("stop_all")}
      />
      {banner && (
        <div className="banner" role="alert" onClick={() => setBanner(null)}>
          {banner}
        </div>
      )}
      <main className="main">
        <section className="editor">
          <div className="panel-title">
            Partitur (YAML)
            <span className="muted">Strg+Enter kompiliert · Strg+S lädt</span>
            <div className="spacer" />
            <button className="btn" onClick={() => compile(yamlText)} disabled={busy}>
              Kompilieren
            </button>
            <button className="btn btn-primary" onClick={load} disabled={busy || state.running} title={state.running ? "Nur im Stillstand" : "Kompilieren und in den Runner laden"}>
              Laden
            </button>
          </div>
          <Editor value={editorValue} onChange={setYamlText} errors={errors} onCompile={() => compile(yamlText)} onLoad={load} />
          <div className={`errors ${errors.length ? "" : "errors-empty"}`}>
            {errors.length === 0 ? (
              <span className="ok">✓ Keine Fehler</span>
            ) : (
              <ul>
                {errors.map((e, i) => (
                  <li key={i}>
                    <span className="err-line">{e.line ? `Zeile ${e.line}${e.column ? `:${e.column}` : ""}` : "—"}</span>
                    <span className="err-msg">{e.message}</span>
                    {e.suggestion && <span className="err-sugg">{e.suggestion}</span>}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </section>
        <Blocks score={compiled} state={state} stale={stale} />
        <MidiMonitor rows={midiRows} logs={logs} onClear={() => setMidiRows([])} />
      </main>
    </div>
  );
}
