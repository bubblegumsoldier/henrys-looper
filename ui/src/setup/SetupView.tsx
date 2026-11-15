//! Setup (design/setup.html): a calm page in the same window. Navigation on the left, cards on the
//! right.
//!
//! It is reachable at any time from the gear. While the engine is stopped every field edits the
//! start configuration; while it runs, the device, rate, buffer and track list are locked (changing
//! them means reopening the device) and the page shows what the engine really opened - but
//! everything the engine can change on the fly stays live here: bus outputs and volumes, each
//! track's monitoring, sends and latency trim, tempo, grid and click.

import { applyClock } from "../live/ClockBar";
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { api } from "../api";
import { MidiSetup } from "../midi/MidiSetup";
import { PluginsPage } from "../plugins/PluginsPage";
import { RecordingPage } from "../recording/RecordingPage";
import { busGainAddress, GLOBAL, quantizeAddress, trackAddress } from "../midi/addresses";
import { useMidiView } from "../midi/store";
import { midiApi } from "../api";
import { instrumentApi, instrumentOf, noteName, setInstruments, useInstruments } from "../plugins/instruments";
import { InstrumentPicker } from "../mixer/InstrumentSlot";
import { IS_MAC } from "../platform";
import { currentStatus, structureSignature, useStatusSlice } from "../status";
import { Icon } from "../shell/Icon";
import { fail, run } from "../shell/notes";
import { startEngine, stopEngine, useEngine } from "../shell/engine";
import { T, gainDb, num } from "../shell/terms";
import { HMeter } from "../live/HMeter";
import { GRID_CHOICES, TransposeControl } from "../live/MidiLoopRows";
import { TapTempo } from "../live/TapTempo";
import {
  BUS_CHOICES,
  DEFAULT_LATENCY,
  DEFAULT_LATENCY_MEASURED,
  SAMPLE_RATES,
  bufferRange,
  covers,
  maxChannels,
  maxOutChannels,
  ms,
  setConfig,
  useConfig,
} from "./config";
import type {
  AppInfo,
  BusId,
  BusStatus,
  DeviceInfo,
  DeviceReport,
  LooperStatus,
  LooperTrackStatus,
  OverdubMode,
  Quantize,
  StartBusOut,
  StartConfig,
  StartInstrument,
  StartTrack,
} from "../types";

export type SetupPage = "audio" | "tracks" | "tempo" | "midi" | "plugins" | "recording" | "general";

const BUFFERS = [32, 64, 128, 256, 512, 1024];

interface Props {
  info: AppInfo | null;
  page: SetupPage;
  onPage: (page: SetupPage) => void;
  /** Called after the engine started from this page, so the frame can switch to Live. */
  onStarted: () => void;
}

// ---------------------------------------------------------------------------------------------
// Small controls in the design's vocabulary
// ---------------------------------------------------------------------------------------------

function Seg<V extends string | number>({
  value,
  options,
  onChange,
  disabled,
  midi,
  small,
}: {
  value: V;
  options: { value: V; label: ReactNode; title?: string }[];
  onChange: (v: V) => void;
  disabled?: boolean;
  /** `data-midi` per option, when choosing one is an engine action. */
  midi?: (v: V) => string | undefined;
  small?: boolean;
}) {
  return (
    <div className={`seg${disabled ? " off" : ""}`} style={small ? ({ "--ctl-h": "22px" } as CSSProperties) : undefined}>
      {options.map((o) => (
        <button
          key={String(o.value)}
          className={o.value === value ? "on" : ""}
          disabled={disabled}
          data-midi={midi?.(o.value)}
          title={o.title}
          onClick={() => o.value !== value && onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function Select<V extends string | number>({
  value,
  options,
  onChange,
  disabled,
  width,
  title,
}: {
  value: V;
  options: { value: V; label: string }[];
  onChange: (v: V) => void;
  disabled?: boolean;
  width?: number;
  title?: string;
}) {
  return (
    <select
      className="field sel"
      style={width ? { width } : undefined}
      value={String(value)}
      disabled={disabled}
      title={title}
      onChange={(e) => {
        const raw = e.target.value;
        const hit = options.find((o) => String(o.value) === raw);
        if (hit) onChange(hit.value);
      }}
    >
      {options.map((o) => (
        <option key={String(o.value)} value={String(o.value)}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

/** A number field that keeps a half-typed value and reports on every valid change. */
function NumberInput({
  value,
  onChange,
  min,
  max,
  step,
  width = 80,
  disabled,
  placeholder,
  title,
  commitOnBlur,
}: {
  value: number | null;
  onChange: (v: number | null) => void;
  min?: number;
  max?: number;
  step?: number;
  width?: number;
  disabled?: boolean;
  placeholder?: string;
  title?: string;
  /** Report only on blur/Enter (for values the running engine applies at once). */
  commitOnBlur?: boolean;
}) {
  const [text, setText] = useState(value === null ? "" : String(value));
  const editing = useRef(false);
  useEffect(() => {
    if (editing.current) return;
    setText(value === null ? "" : String(value));
  }, [value]);
  const report = (raw: string) => {
    if (raw.trim() === "") return onChange(null);
    const n = Number(raw);
    if (Number.isFinite(n)) onChange(n);
  };
  return (
    <input
      className="field num-in"
      type="number"
      style={{ width }}
      value={text}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      placeholder={placeholder}
      title={title}
      onFocus={() => {
        editing.current = true;
      }}
      onChange={(e) => {
        setText(e.target.value);
        if (!commitOnBlur) report(e.target.value);
      }}
      onBlur={() => {
        editing.current = false;
        if (commitOnBlur) report(text);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
    />
  );
}

function Toggle({ on, onChange, disabled, midi, title }: { on: boolean; onChange: (on: boolean) => void; disabled?: boolean; midi?: string; title?: string }) {
  return (
    <button
      className={`toggle${on ? " on" : ""}`}
      role="switch"
      aria-checked={on}
      disabled={disabled}
      data-midi={midi}
      title={title}
      onClick={() => onChange(!on)}
    />
  );
}

/** `1–2` or `3` for a bus start channel and width. */
function outLabel(channel: number, width: number): string {
  return width >= 2 ? `Output ${channel}–${channel + 1}` : `Output ${channel}`;
}

function outChoices(outs: number, width: number): { value: number; label: string }[] {
  const list: { value: number; label: string }[] = [];
  if (width >= 2) for (let c = 1; c + 1 <= Math.max(outs, 2); c += 2) list.push({ value: c, label: outLabel(c, 2) });
  else for (let c = 1; c <= Math.max(outs, 1); c += 1) list.push({ value: c, label: outLabel(c, 1) });
  return list;
}

/** "main+monitor" etc. from two booleans, and back. */
function busValue(main: boolean, monitor: boolean): string {
  return main && monitor ? "main+monitor" : main ? "main" : monitor ? "monitor" : "none";
}

// ---------------------------------------------------------------------------------------------

export function SetupView({ info, page, onPage, onStarted }: Props) {
  const config = useConfig();
  const engine = useEngine();
  const running = useStatusSlice((s) => s.running);
  const midi = useMidiView();
  const [report, setReport] = useState<DeviceReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const tracksCard = useRef<HTMLElement | null>(null);

  const patch = useCallback((part: Partial<StartConfig>) => setConfig((c) => ({ ...c, ...part })), []);

  const scan = useCallback(async () => {
    setScanning(true);
    try {
      const found = await api.listDevices();
      setReport(found);
      setConfig((c) => {
        const host = found.hosts.find((h) => h.name === c.host) ?? found.hosts[0];
        if (!host) return c;
        const known = host.devices.some((d) => d.name === c.device);
        if (host.name === c.host && (known || c.device === null)) return c;
        // Prefer the interface this project was measured on, else the default input if it can also
        // play (one device, one clock), else any duplex device, else the default input.
        const duplex = (d: DeviceInfo) => d.input && d.output;
        const preferred =
          host.devices.find((d) => /focusrite|scarlett/i.test(d.name) && duplex(d)) ??
          host.devices.find((d) => d.name === host.default_input && duplex(d)) ??
          host.devices.find(duplex) ??
          host.devices.find((d) => d.name === host.default_input) ??
          host.devices.find((d) => d.input) ??
          null;
        return { ...c, host: host.name, device: preferred ? preferred.name : null };
      });
    } catch (e) {
      fail((e as Error).message);
    } finally {
      setScanning(false);
    }
  }, []);

  useEffect(() => {
    // The survey opens nothing, but a running engine already owns the device; ask only when idle.
    if (!running && report === null) void scan();
  }, [scan, running, report]);

  useEffect(() => {
    if (page === "tracks") tracksCard.current?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, [page]);

  const host = useMemo(() => report?.hosts.find((h) => h.name === config.host) ?? null, [report, config.host]);
  // With no device chosen the engine takes the host's default input; describe that one.
  const device = useMemo(
    () => host?.devices.find((d) => d.name === (config.device ?? host.default_input)) ?? null,
    [host, config.device],
  );

  const start = async () => {
    if (await startEngine(config)) onStarted();
  };

  const nav: { id: SetupPage; icon: string; label: string; badge?: string }[] = [
    { id: "audio", icon: "speaker", label: "Audio" },
    { id: "tracks", icon: "input", label: "Tracks & Inputs", badge: String(config.tracks.length) },
    { id: "tempo", icon: "metro", label: "Tempo & Loop" },
    { id: "midi", icon: "midi", label: "MIDI", badge: midi.connected ? "connected" : "—" },
    { id: "plugins", icon: "gear", label: "Plugins" },
    { id: "recording", icon: "rec", label: T.recording },
    { id: "general", icon: "gear", label: "General" },
  ];

  return (
    <div className="setup">
      <nav className="snav">
        <div className="caps" style={{ padding: "4px 10px 8px" }}>
          {T.setup}
        </div>
        {nav.map((n) => (
          <a
            key={n.id}
            href="#"
            className={page === n.id || (page === "tracks" && n.id === "tracks") ? "on" : ""}
            onClick={(e) => {
              e.preventDefault();
              onPage(n.id);
            }}
          >
            <Icon name={n.icon} />
            {n.label}
            {n.badge && <span className="badge">{n.badge}</span>}
          </a>
        ))}
        <div className="grow" />
        <div className="hint" style={{ padding: "0 10px" }}>
          Changes to device, sample rate or buffer need the engine stopped; they apply on the next start.
          {running && " The engine is running, so they are locked now."}
        </div>
      </nav>

      <main className="spage">
        {(page === "audio" || page === "tracks") && (
          <>
            <div>
              <h1>Audio</h1>
              <p className="lead">Interface, clock and where the two buses go.</p>
            </div>
            <DeviceCard
              config={config}
              report={report}
              device={device}
              scanning={scanning}
              running={running}
              busy={engine.busy}
              onScan={() => void scan()}
              onStart={() => void start()}
              patch={patch}
            />
            <BusCard config={config} device={device} running={running} patch={patch} />
            <section className="card" ref={tracksCard}>
              <TracksCard config={config} device={device} running={running} info={info} patch={patch} />
            </section>
          </>
        )}
        {page === "tempo" && <TempoPage config={config} running={running} patch={patch} />}
        {page === "midi" && <MidiSetup />}
        {page === "plugins" && <PluginsPage />}
        {page === "recording" && <RecordingPage />}
        {page === "general" && <GeneralPage info={info} />}
      </main>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Audio device
// ---------------------------------------------------------------------------------------------

function DeviceCard({
  config,
  report,
  device,
  scanning,
  running,
  busy,
  onScan,
  onStart,
  patch,
}: {
  config: StartConfig;
  report: DeviceReport | null;
  device: DeviceInfo | null;
  scanning: boolean;
  running: boolean;
  busy: boolean;
  onScan: () => void;
  onStart: () => void;
  patch: (p: Partial<StartConfig>) => void;
}) {
  const engine = useEngine().info;
  const counters = useStatusSlice(
    (s) => `${s.xruns}|${s.fifo_overruns}|${s.fifo_underruns}|${s.latency_frames}|${s.sample_rate}`,
  );
  const [xruns, overruns, underruns, latencyFrames, rate] = counters.split("|").map(Number);
  const host = report?.hosts.find((h) => h.name === config.host) ?? null;
  const range = bufferRange(device);
  const rates = device
    ? SAMPLE_RATES.filter((r) => covers(device.input_configs, r) || covers(device.output_configs, r))
    : SAMPLE_RATES;
  const buffers = Array.from(new Set([...BUFFERS.filter((b) => !range || (b >= range.min && b <= range.max)), config.buffer_frames])).sort(
    (a, b) => a - b,
  );
  const asioMissing = !IS_MAC && report ? !report.asio_built || !report.hosts.some((h) => h.name === "asio") : false;
  const bufferOff = range !== null && (config.buffer_frames < range.min || config.buffer_frames > range.max);
  const ins = device ? maxChannels(device) : null;
  const outs = device ? maxOutChannels(device) : null;
  const estimate = !DEFAULT_LATENCY_MEASURED && config.latency_frames === DEFAULT_LATENCY;

  return (
    <section className="card">
      <div className="card-h">
        <h3>Audio device</h3>
        <div className="grow" />
        {running ? (
          <>
            <span className="pill">
              <i className="led" />
              {T.engineRunning}
            </span>
            <button className="btn danger" disabled={busy} onClick={stopEngine}>
              <Icon name="power" />
              {T.stopEngine}
            </button>
          </>
        ) : (
          <>
            <span className="pill off">
              <i className="led" />
              {T.engineStopped}
            </span>
            <button className="btn accent" disabled={busy || config.tracks.length === 0} onClick={onStart}>
              <Icon name="power" />
              {busy ? "Starting…" : T.startEngine}
            </button>
          </>
        )}
      </div>
      <div className="card-b">
        {asioMissing && (
          <p className="warnline">
            <Icon name="warn" /> No ASIO in this build or on this machine. Without ASIO the latency is not usable.
          </p>
        )}
        {host?.error && (
          <p className="warnline">
            <Icon name="warn" /> {host.error}
          </p>
        )}
        <div className="srow">
          <label>Driver</label>
          <div>
            {running && engine ? (
              <span className="field ro" style={{ width: 220 }}>{engine.host}</span>
            ) : (
              <Select
                width={220}
                value={config.host}
                options={(report?.hosts ?? [{ name: config.host }]).map((h) => ({ value: h.name, label: h.name === "coreaudio" ? "CoreAudio" : h.name.toUpperCase() }))}
                onChange={(v) => patch({ host: v, device: null })}
              />
            )}
          </div>
        </div>
        <div className="srow">
          <label>Device</label>
          <div>
            {running && engine ? (
              <span className="field ro" style={{ width: 320 }}>{engine.input_device}</span>
            ) : (
              <Select
                width={320}
                value={config.device ?? ""}
                options={[
                  { value: "", label: host?.default_input ? `Default (${host.default_input})` : "(host default)" },
                  ...(host?.devices ?? []).map((d) => ({ value: d.name, label: d.name })),
                ]}
                onChange={(v) => patch({ device: v === "" ? null : v })}
              />
            )}
            <span className="dim">
              {running && engine
                ? `${engine.input_channels} inputs · ${engine.output_channels} outputs`
                : ins !== null
                  ? `${ins} inputs · ${outs} outputs`
                  : ""}
            </span>
            {!running && (
              <button className="btn s" onClick={onScan} disabled={scanning}>
                <Icon name="search" />
                {scanning ? "Scanning…" : "Rescan"}
              </button>
            )}
          </div>
        </div>
        {device?.note && !running && <div className="hint">{device.note}</div>}
        <div className="srow">
          <label>Sample rate</label>
          <div>
            <Seg
              value={running && engine ? engine.sample_rate : config.sample_rate}
              disabled={running}
              options={(running && engine && !rates.includes(engine.sample_rate) ? [...rates, engine.sample_rate] : rates).map((r) => ({
                value: r,
                label: `${num(r / 1000)} kHz`,
              }))}
              onChange={(v) => patch({ sample_rate: v })}
            />
          </div>
        </div>
        <div className="srow">
          <label>Buffer</label>
          <div>
            <Seg
              value={running && engine ? engine.buffer_frames : config.buffer_frames}
              disabled={running}
              options={buffers.map((b) => ({ value: b, label: String(b) }))}
              onChange={(v) => patch({ buffer_frames: v })}
            />
            <span className="dim">samples</span>
            {range && !running && (
              <span className="dim2">
                device reports {range.min}–{range.max}
              </span>
            )}
          </div>
        </div>
        {bufferOff && !running && (
          <p className="warnline">
            <Icon name="warn" /> {config.buffer_frames} frames is outside what the device reports; the driver will
            probably refuse it.
          </p>
        )}
        <div className="srow">
          <label>Latency</label>
          <div className="lat">
            {running && engine ? (
              <>
                <span>
                  Round trip <b>{engine.latency_ms.toFixed(1)} ms</b>
                </span>
                <span>
                  Compensation <b>{latencyFrames} frames</b> ({ms(latencyFrames, rate)})
                </span>
                <span>
                  Dropouts since start{" "}
                  <b style={{ color: xruns > 0 ? "var(--warn)" : "var(--ok)" }}>{xruns}</b>
                </span>
                {underruns > 0 && (
                  <span>
                    FIFO empty <b style={{ color: "var(--warn)" }}>{underruns}</b>
                  </span>
                )}
                {overruns > 0 && (
                  <span>
                    Input lost <b style={{ color: "var(--err)" }}>{overruns}</b>
                  </span>
                )}
              </>
            ) : (
              <>
                <span>Compensation</span>
                <NumberInput
                  value={config.latency_frames}
                  min={0}
                  step={1}
                  onChange={(v) => v !== null && patch({ latency_frames: Math.max(0, Math.round(v)) })}
                />
                <span>
                  frames ≈ <b>{ms(config.latency_frames, config.sample_rate)}</b>
                </span>
                <span className={estimate ? "warn-tx" : ""}>
                  {estimate
                    ? "estimate from CoreAudio figures, not measured - calibrate before the first serious take"
                    : "default for every track without its own value; measure again after changing device, rate or buffer"}
                </span>
              </>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------------------------
// Output buses
// ---------------------------------------------------------------------------------------------

function busSig(s: LooperStatus): string {
  return s.buses.map((b) => `${b.id}:${b.channel}:${b.width}:${b.gain}`).join("|") + `~${s.output_channels}~${s.bus_note ?? ""}~${s.buses_collapsed}`;
}

const busPeaks = (id: BusId) => (s: LooperStatus) => s.buses.find((b) => b.id === id)?.peaks ?? [0, 0];

function GainSlider({ value, onChange, disabled, midi }: { value: number; onChange: (v: number) => void; disabled?: boolean; midi?: string }) {
  const [local, setLocal] = useState(value);
  const dragging = useRef(false);
  useEffect(() => {
    if (!dragging.current) setLocal(value);
  }, [value]);
  return (
    <span className="gain" data-midi={midi}>
      {/* The engine allows up to 4; a mouse throw stops at 2 (+6 dB) so the useful half stays fine. */}
      <input
        className="range"
        type="range"
        min={0}
        max={2}
        step={0.01}
        value={local}
        disabled={disabled}
        onPointerDown={() => {
          dragging.current = true;
        }}
        onPointerUp={() => {
          dragging.current = false;
        }}
        onChange={(e) => {
          const v = Number(e.target.value);
          setLocal(v);
          onChange(v);
        }}
      />
      <span className="num val">{gainDb(local)}</span>
    </span>
  );
}

function BusCard({
  config,
  device,
  running,
  patch,
}: {
  config: StartConfig;
  device: DeviceInfo | null;
  running: boolean;
  patch: (p: Partial<StartConfig>) => void;
}) {
  useStatusSlice(busSig);
  const status = currentStatus();
  const outs = running ? status.output_channels : maxOutChannels(device);
  const defaultMain: StartBusOut = { channel: 1, width: 2 };
  const defaultMonitor: StartBusOut = outs >= 4 ? { channel: 3, width: 2 } : { channel: 1, width: 2 };

  type Row = { id: BusId; label: string; icon: string; channel: number; width: number; gain: number; click: boolean; deaf: boolean };
  const rows: Row[] = running
    ? status.buses.map((b: BusStatus) => ({
        id: b.id,
        label: b.id === "main" ? "Main · Room" : "Monitor · Headphones",
        icon: b.id === "main" ? "speaker" : "phones",
        channel: b.channel,
        width: b.width,
        gain: b.gain,
        click: b.has_click,
        deaf: status.buses_collapsed && b.id === "monitor",
      }))
    : (["main", "monitor"] as BusId[]).map((id) => {
        const out = (id === "main" ? config.bus_out_main : config.bus_out_monitor) ?? (id === "main" ? defaultMain : defaultMonitor);
        return {
          id,
          label: id === "main" ? "Main · Room" : "Monitor · Headphones",
          icon: id === "main" ? "speaker" : "phones",
          channel: out.channel,
          width: out.width,
          gain: (id === "main" ? config.bus_gain_main : config.bus_gain_monitor) ?? 1,
          click: id === "monitor",
          deaf: false,
        };
      });
  const collapsed = rows.length === 2 && rows[0].channel === rows[1].channel && rows[0].width === rows[1].width;

  const setOut = (row: Row, channel: number, width: number) => {
    if (running) run(() => api.busOutput(row.id, channel, width));
    else patch(row.id === "main" ? { bus_out_main: { channel, width } } : { bus_out_monitor: { channel, width } });
  };
  const setGain = (row: Row, gain: number) => {
    if (running) run(() => api.busGain(row.id, gain));
    else patch(row.id === "main" ? { bus_gain_main: gain } : { bus_gain_monitor: gain });
  };

  return (
    <section className="card">
      <div className="card-h">
        <h3>Output buses</h3>
        <span className="dim">The click is only ever on Monitor.</span>
      </div>
      <table className="tt">
        <thead>
          <tr>
            <th style={{ width: 240 }}>Bus</th>
            <th>Output</th>
            <th>Width</th>
            <th>Volume</th>
            <th style={{ width: 220 }}>Level</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.id}>
              <td>
                <span className="tn2">
                  <Icon name={row.icon} size={15} />
                  {row.label}
                  {row.click && (
                    <span className="chip" style={{ marginLeft: 4 }}>
                      <Icon name="metro" />
                      {T.click}
                    </span>
                  )}
                </span>
              </td>
              <td>
                <Select
                  width={150}
                  value={row.channel}
                  options={outChoices(outs, row.width)}
                  onChange={(c) => setOut(row, c, row.width)}
                  title="Which device outputs this bus leaves on. A single channel folds the bus to mono."
                />
              </td>
              <td>
                <Seg
                  small
                  value={row.width >= 2 ? 2 : 1}
                  options={[
                    { value: 1, label: T.mono },
                    { value: 2, label: T.stereo },
                  ]}
                  onChange={(w) => setOut(row, w === 2 && row.channel + 1 > Math.max(outs, 2) ? 1 : row.channel, w)}
                />
              </td>
              <td>
                <GainSlider
                  value={row.gain}
                  disabled={row.deaf}
                  midi={running ? busGainAddress(row.id) : undefined}
                  onChange={(g) => setGain(row, g)}
                />
              </td>
              <td>
                {running ? <HMeter pick={busPeaks(row.id)} channels={2} style={{ width: 200 }} /> : <span className="dim2">—</span>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {(collapsed || (running && status.bus_note)) && (
        <div className="card-b">
          <p className="warnline">
            <Icon name="warn" />
            {running && status.bus_note
              ? status.bus_note
              : `Both buses leave on the same output. The device reports ${outs} output channels; separate buses need four. There is then one mix - and the click goes to the room as well. Stopgap with two outputs: Main on output 1, Monitor on output 2, split with a Y cable.`}
          </p>
        </div>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------------------------
// Tracks and inputs
// ---------------------------------------------------------------------------------------------

function TracksCard({
  config,
  device,
  running,
  info,
  patch,
}: {
  config: StartConfig;
  device: DeviceInfo | null;
  running: boolean;
  info: AppInfo | null;
  patch: (p: Partial<StartConfig>) => void;
}) {
  const sig = useStatusSlice(structureSignature);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const live = useMemo(() => currentStatus().tracks, [sig]);
  const channels = maxChannels(device);
  const maxTracks = info?.max_tracks ?? 8;
  const setTrack = (index: number, part: Partial<StartTrack>) =>
    patch({ tracks: config.tracks.map((t, i) => (i === index ? { ...t, ...part } : t)) });
  const inputs = Array.from({ length: Math.max(channels, 2) }, (_, i) => ({ value: i + 1, label: `Input ${i + 1}` }));

  const problem =
    config.tracks.length === 0
      ? "At least one track."
      : config.tracks.some((t) => t.name.trim() === "")
        ? "Every track needs a name."
        : config.tracks.some((t) => !t.instrument && t.input_channel_right === t.input_channel)
          ? "A stereo track needs two different inputs."
          : null;

  return (
    <>
      <div className="card-h">
        <h3>Tracks and inputs</h3>
        <span className="dim">
          {running ? live.length : config.tracks.length} of {maxTracks}
          {running ? " · as the engine opened them" : " · applies on the next start"}
        </span>
        <div className="grow" />
        {problem && !running && <span className="warn-tx">{problem}</span>}
        {!running && (
          <button
            className="btn s"
            disabled={config.tracks.length >= maxTracks}
            onClick={() =>
              patch({
                tracks: [
                  ...config.tracks,
                  {
                    name: `track ${config.tracks.length + 1}`,
                    input_channel: Math.min(config.tracks.length + 1, channels),
                    input_channel_right: null,
                    pan: 0,
                    latency_frames: null,
                    latency_trim: 0,
                    bus: "main+monitor",
                    monitor_bus: "monitor",
                  },
                ],
              })
            }
          >
            <Icon name="plus" />
            Track
          </button>
        )}
      </div>
      <table className="tt">
        <thead>
          <tr>
            <th>Track</th>
            <th>Input</th>
            <th>Channels</th>
            {running && <th>{T.monitor}</th>}
            <th>Loop to</th>
            <th>Monitor to</th>
            <th>{T.latency}</th>
            <th>{T.trim}</th>
            {!running && <th />}
          </tr>
        </thead>
        <tbody>
          {running
            ? live.map((t) => <LiveTrackRow key={t.index} t={t} />)
            : config.tracks.map((t, i) => (
                <tr key={i}>
                  <td>
                    <span className="tn2" style={{ "--c": `var(--trk-${(i % 8) + 1})` } as CSSProperties}>
                      <i />
                      <input
                        className="field name-in"
                        value={t.name}
                        placeholder="Name"
                        onChange={(e) => setTrack(i, { name: e.target.value })}
                      />
                    </span>
                  </td>
                  <td>
                    <span className="row" style={{ gap: 4 }}>
                      <Select
                        width={110}
                        value={t.instrument ? 0 : t.input_channel}
                        options={[...inputs, { value: 0, label: T.instrument }]}
                        title="An input of the interface, or an instrument plugin played over MIDI"
                        onChange={(v) =>
                          setTrack(
                            i,
                            v === 0
                              ? { instrument: t.instrument ?? { device: null, channel: null, monitor: true } }
                              : { input_channel: v, instrument: null },
                          )
                        }
                      />
                      {t.instrument ? (
                        <InstrumentInputFields
                          track={t.name}
                          value={t.instrument}
                          onChange={(instrument) => setTrack(i, { instrument })}
                        />
                      ) : (
                        t.input_channel_right !== null && (
                          <Select
                            width={110}
                            value={t.input_channel_right}
                            options={inputs}
                            onChange={(v) => setTrack(i, { input_channel_right: v })}
                          />
                        )
                      )}
                    </span>
                  </td>
                  <td>
                    <Seg
                      small
                      disabled={!!t.instrument}
                      value={t.input_channel_right === null && !t.instrument ? 1 : 2}
                      options={[
                        { value: 1, label: T.mono, title: "One input, recorded mono" },
                        { value: 2, label: T.stereo, title: "A pair of inputs, recorded stereo" },
                      ]}
                      onChange={(n) =>
                        setTrack(i, {
                          input_channel_right:
                            n < 2 ? null : (t.input_channel_right ?? (t.input_channel < channels ? t.input_channel + 1 : 1)),
                        })
                      }
                    />
                  </td>
                  <td>
                    <Select width={150} value={t.bus} options={BUS_CHOICES} onChange={(v) => setTrack(i, { bus: v })} />
                  </td>
                  <td>
                    <Select width={130} value={t.monitor_bus} options={BUS_CHOICES} onChange={(v) => setTrack(i, { monitor_bus: v })} />
                  </td>
                  <td>
                    <span className="row" style={{ gap: 6 }}>
                      <NumberInput
                        width={76}
                        value={t.latency_frames}
                        min={0}
                        placeholder={String(config.latency_frames)}
                        title="Measured latency of this input in frames. Empty follows the global default."
                        onChange={(v) => setTrack(i, { latency_frames: v === null ? null : Math.max(0, Math.round(v)) })}
                      />
                      <span className="dim">
                        {ms((t.latency_frames ?? config.latency_frames) + t.latency_trim, config.sample_rate)}
                        {t.latency_frames === null ? " default" : " measured"}
                      </span>
                    </span>
                  </td>
                  <td>
                    <span className="field-unit">
                      <NumberInput
                        width={70}
                        value={t.latency_trim}
                        step={1}
                        title="Manual surcharge in frames for what no measurement sees (a plugin host's buffer). Survives calibration."
                        onChange={(v) => setTrack(i, { latency_trim: Math.round(v ?? 0) })}
                      />
                      <span className="unit dim">fr</span>
                    </span>
                  </td>
                  <td>
                    <button
                      className="btn s sq ghost danger"
                      title="Remove track"
                      onClick={() => patch({ tracks: config.tracks.filter((_, j) => j !== i) })}
                    >
                      <Icon name="x" />
                    </button>
                  </td>
                </tr>
              ))}
        </tbody>
      </table>
      <div className="card-b">
        <div className="hint">
          A microphone or a guitar records <b>mono</b> - half the memory, no audible difference. A keyboard or a
          pad from a sample library belongs on an input <b>pair</b>; the two need not be neighbours.{" "}
          <b>{T.instrument}</b> plays an instrument plugin from a MIDI keyboard instead: pick the plugin, the
          device and the channel; it is always heard and recorded stereo, and what it plays is looped as sound.{" "}
          <b>Latency</b> is the measured path of this input in frames (empty: the global default of{" "}
          {config.latency_frames}); <b>Trim</b> is added on top for what no measurement sees and survives the
          next calibration. Pan and volume are set per track in Live (track details).
        </div>
      </div>
    </>
  );
}

/** The MIDI side of an instrument track in the setup: plugin, device, channel. */
function InstrumentInputFields({
  track,
  value,
  onChange,
}: {
  track: string;
  value: StartInstrument;
  onChange: (v: StartInstrument) => void;
}) {
  const [ports, setPorts] = useState<string[]>([]);
  useEffect(() => {
    midiApi.ports().then((list) => setPorts(list.map((p) => p.name)), () => setPorts([]));
  }, []);
  const devices = [
    { value: "", label: T.allDevices },
    ...ports.map((p) => ({ value: p, label: p })),
    ...(value.device && !ports.includes(value.device) ? [{ value: value.device, label: `${value.device} (not connected)` }] : []),
  ];
  const channels = [{ value: 0, label: T.allChannels }, ...Array.from({ length: 16 }, (_, c) => ({ value: c + 1, label: `${T.channelShort} ${c + 1}` }))];
  return (
    <>
      <InstrumentChoice track={track} />
      <Select
        width={150}
        value={value.device ?? ""}
        options={devices}
        title="Which MIDI keyboard plays this track"
        onChange={(v) => onChange({ ...value, device: v === "" ? null : v })}
      />
      <Select
        width={100}
        value={value.channel ?? 0}
        options={channels}
        title="Which MIDI channel plays this track. Notes on other channels stay with the MIDI bindings."
        onChange={(v) => onChange({ ...value, channel: v === 0 ? null : v })}
      />
      <Select
        width={90}
        value={value.loop === "midi" ? "midi" : "audio"}
        options={[
          { value: "audio", label: `${T.loopKind}: ${T.loopAudio}` },
          { value: "midi", label: `${T.loopKind}: ${T.loopMidi}` },
        ]}
        title="What a take stores: the instrument's sound (Audio) or the notes, played back through the instrument (MIDI)"
        onChange={(v) => onChange({ ...value, loop: v === "midi" ? "midi" : null })}
      />
      <TransposeControl
        value={value.transpose ?? 0}
        onChange={(v) => onChange({ ...value, transpose: v === 0 ? null : v })}
      />
      {value.loop === "midi" && (
        <Select
          width={90}
          value={value.grid ?? "none"}
          options={GRID_CHOICES.map((g) => ({ value: g.value, label: g.value === "none" ? `${T.noteGrid}: ${T.gridNone}` : g.label }))}
          title="Note grid of the MIDI loop. Acts on what is recorded too and never changes the recording."
          onChange={(v) => onChange({ ...value, grid: v === "none" ? null : v })}
        />
      )}
    </>
  );
}

/** The instrument plugin of a track, by name: a button that opens the picker. */
function InstrumentChoice({ track }: { track: string }) {
  const { rack } = useInstruments();
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const inst = instrumentOf(rack, track);
  return (
    <>
      <button
        className="btn s"
        style={{ width: 150, justifyContent: "flex-start", overflow: "hidden" }}
        title={inst ? `${inst.name} · ${inst.vendor} - click to choose another` : T.instrumentPick}
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
      >
        <Icon name="plug" />
        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{inst ? inst.name : T.instrumentNone}</span>
      </button>
      {inst && (
        <button
          className="btn s sq ghost"
          title="Open the instrument's window"
          disabled={!inst.has_editor}
          onClick={() => run(async () => setInstruments(await instrumentApi.editor(track, !inst.editor_open)))}
        >
          <Icon name="sliders" />
        </button>
      )}
      {anchor && <InstrumentPicker track={track} anchor={anchor} onClose={() => setAnchor(null)} />}
    </>
  );
}

/** A running instrument track: plugin and how it is played, read only except for the plugin. */
function LiveInstrumentCell({ t }: { t: LooperTrackStatus }) {
  const config = useConfig();
  const input = config.tracks.find((c) => c.name === t.name)?.instrument ?? null;
  const how = input
    ? `${input.device ?? T.allDevices} · ${input.channel ? `${T.channelShort} ${input.channel}` : T.allChannels}${
        input.note_low != null || input.note_high != null ? ` · ${noteName(input.note_low ?? 0)}–${noteName(input.note_high ?? 127)}` : ""
      }`
    : T.midiIn;
  return (
    <span className="row" style={{ gap: 4 }}>
      <InstrumentChoice track={t.name} />
      <span className="dim" title="Device and channel apply on the next start">{how}</span>
    </span>
  );
}

function LiveTrackRow({ t }: { t: LooperTrackStatus }) {
  const i = t.index;
  const setBus = (source: "loop" | "monitor", value: string) => {
    const wantMain = value.includes("main");
    const wantMonitor = value.includes("monitor");
    const hasMain = source === "loop" ? t.bus_main : t.monitor_bus_main;
    const hasMonitor = source === "loop" ? t.bus_monitor : t.monitor_bus_monitor;
    if (wantMain !== hasMain) run(() => api.trackBus(i, source, "main", wantMain));
    if (wantMonitor !== hasMonitor) run(() => api.trackBus(i, source, "monitor", wantMonitor));
  };
  return (
    <tr>
      <td>
        <span className="tn2" style={{ "--c": `var(--trk-${(i % 8) + 1})` } as CSSProperties}>
          <i />
          {t.name}
        </span>
      </td>
      <td>
        {t.instrument ? (
          <LiveInstrumentCell t={t} />
        ) : (
          <span className="field ro" style={{ width: 120 }}>
            Input {t.input_channels.join("–")}
          </span>
        )}
      </td>
      <td>
        <Seg small disabled value={t.channels} options={[{ value: 1, label: T.mono }, { value: 2, label: T.stereo }]} onChange={() => undefined} />
      </td>
      <td>
        <Toggle on={t.monitor} midi={trackAddress(i, "monitor")} onChange={(on) => run(() => api.trackMonitor(i, on))} />
      </td>
      <td>
        <Select width={150} value={busValue(t.bus_main, t.bus_monitor)} options={BUS_CHOICES} onChange={(v) => setBus("loop", v)} />
      </td>
      <td>
        <Select width={130} value={busValue(t.monitor_bus_main, t.monitor_bus_monitor)} options={BUS_CHOICES} onChange={(v) => setBus("monitor", v)} />
      </td>
      <td>
        <span className="num">{t.latency_ms.toFixed(1)} ms</span> <span className="dim">{t.latency_inherited ? "default" : "measured"}</span>
      </td>
      <td>
        <span className="field-unit" data-midi={trackAddress(i, "latency_trim")}>
          <NumberInput
            width={70}
            commitOnBlur
            value={t.latency_trim}
            step={1}
            onChange={(v) => run(() => api.trackLatency(i, t.latency_measured, Math.round(v ?? 0)))}
          />
          <span className="unit dim">fr</span>
        </span>
      </td>
    </tr>
  );
}

// ---------------------------------------------------------------------------------------------
// Tempo and loop
// ---------------------------------------------------------------------------------------------

function TempoPage({ config, running, patch }: { config: StartConfig; running: boolean; patch: (p: Partial<StartConfig>) => void }) {
  const live = useStatusSlice(
    (s) =>
      `${s.bpm}|${s.beats_per_bar}|${s.beat_unit}|${s.bars}|${s.quantize}|${s.click}|${s.loop_seconds}|${s.click_gain}|${s.monitor_gain}`,
  );
  const [bpm, bpb, unit, bars, quantize, click, loopSeconds, clickGain, monitorGain] = live.split("|");
  const q = (running ? quantize : config.quantize) as Quantize;
  const odLive = useStatusSlice((s) => s.overdub_mode);
  const od: OverdubMode = running ? odLive : (config.overdub_mode ?? "latch");
  const seconds = running ? Number(loopSeconds) : (config.bars * config.beats_per_bar * 60) / (config.bpm || 100);

  return (
    <>
      <div>
        <h1>Tempo &amp; Loop</h1>
        <p className="lead">
          {running
            ? "The running engine's clock - the same values the Live page shows. Tempo, time signature and loop length change at once while every track is empty (and no score is loaded), and are kept for the next start."
            : "What the engine starts with. A loaded score sets tempo, time signature and loop length itself."}
        </p>
      </div>
      <section className="card">
        <div className="card-h">
          <h3>Clock</h3>
        </div>
        <div className="card-b">
          <div className="srow">
            <label>{T.tempo}</label>
            <div>
              {running ? (
                <span data-midi={GLOBAL.tempo} className="midi-host">
                  <NumberInput
                    width={90}
                    commitOnBlur
                    value={Number(bpm)}
                    min={20}
                    max={300}
                    step={0.5}
                    onChange={(v) => v !== null && applyClock(v, Number(bpb), Number(unit), Number(bars))}
                  />
                </span>
              ) : (
                <NumberInput width={90} value={config.bpm} min={20} max={300} step={0.5} onChange={(v) => v !== null && patch({ bpm: v })} />
              )}
              <span className="dim">BPM</span>
              {!running && <TapTempo onTempo={(v) => patch({ bpm: v })} />}
              {running && (
                <button className="btn" data-midi={GLOBAL.tapTempo} onClick={() => run(() => api.tapTempo())} title="Tap tempo · key T on the Live page">
                  Tap tempo
                </button>
              )}
            </div>
          </div>
          <div className="srow">
            <label>Time signature</label>
            <div>
              <NumberInput
                width={60}
                commitOnBlur={running}
                value={running ? Number(bpb) : config.beats_per_bar}
                min={1}
                max={16}
                onChange={(v) =>
                  v !== null &&
                  (running
                    ? applyClock(Number(bpm), Math.max(1, Math.round(v)), Number(unit), Number(bars))
                    : patch({ beats_per_bar: Math.max(1, Math.round(v)) }))
                }
              />
              <span className="dim">/</span>
              <NumberInput
                width={60}
                commitOnBlur={running}
                value={running ? Number(unit) : config.beat_unit}
                min={1}
                max={16}
                onChange={(v) =>
                  v !== null &&
                  (running
                    ? applyClock(Number(bpm), Number(bpb), Math.max(1, Math.round(v)), Number(bars))
                    : patch({ beat_unit: Math.max(1, Math.round(v)) }))
                }
              />
            </div>
          </div>
          <div className="srow">
            <label>Loop length</label>
            <div>
              <NumberInput
                width={60}
                commitOnBlur={running}
                value={running ? Number(bars) : config.bars}
                min={1}
                max={64}
                onChange={(v) =>
                  v !== null &&
                  (running
                    ? applyClock(Number(bpm), Number(bpb), Number(unit), Math.max(1, Math.round(v)))
                    : patch({ bars: Math.max(1, Math.round(v)) }))
                }
              />
              <span className="dim">
                {T.bars} · {seconds.toFixed(2)} s
              </span>
            </div>
          </div>
          <div className="srow">
            <label>Takes start</label>
            <div>
              <Seg
                value={q}
                options={[
                  { value: "loop", label: <><Icon name="loop" /> at the next loop start</> },
                  { value: "bar", label: <><Icon name="bar" /> at the next bar</> },
                ]}
                midi={running ? (v) => quantizeAddress(v) : undefined}
                onChange={(v) => (running ? run(() => api.setQuantize(v)) : patch({ quantize: v }))}
              />
            </div>
          </div>
          <div className="srow">
            <label>{T.overdub}</label>
            <div>
              <Seg
                value={od}
                options={[
                  { value: "latch", label: <><Icon name="dub" /> {T.overdubLatch}</> },
                  { value: "once", label: <><Icon name="dub" /> {T.overdubOnce}</> },
                ]}
                onChange={(v) => {
                  patch({ overdub_mode: v });
                  if (running) run(() => api.setOverdubMode(v));
                }}
              />
            </div>
          </div>
          <div className="hint">
            {od === "latch"
              ? "O keeps overdubbing: every pass of the loop becomes a layer of its own until O is pressed again (it ends at the end of that pass). O during the first recording runs it straight on into overdub. A pass with nothing played is dropped."
              : "O records exactly one pass of the loop as one layer, then plays."}
          </div>
          <div className="hint">
            {q === "loop"
              ? "Pressing early is enough: R or O anywhere in the loop arms the take for the start of the next loop. The tile counts down the bars - time to get to the guitar."
              : `R or O start at the next bar. You have to wait for bar ${running ? bars : config.bars} and hurry - but you can start mid-loop.`}
          </div>
        </div>
      </section>
      <section className="card">
        <div className="card-h">
          <h3>{running ? "Now" : "At start"}</h3>
        </div>
        <div className="card-b">
          <div className="srow">
            <label>{T.click}</label>
            <div>
              <Toggle
                on={running ? click === "true" : config.click}
                midi={running ? GLOBAL.click : undefined}
                onChange={(on) => (running ? run(() => api.setClick(on)) : patch({ click: on }))}
              />
              <span className="dim">on the Monitor bus only</span>
            </div>
          </div>
          {running && (
            <>
              <div className="srow">
                <label>Click level</label>
                <div>
                  <GainSlider value={Number(clickGain)} onChange={(g) => run(() => api.clickGain(g))} />
                </div>
              </div>
              <div className="srow">
                <label>Monitoring level</label>
                <div>
                  <GainSlider value={Number(monitorGain)} onChange={(g) => run(() => api.monitorGain(g))} />
                  <span className="dim">every monitored input, output side only</span>
                </div>
              </div>
            </>
          )}
          {!running && (
            <div className="srow">
              <label>{T.monitor}</label>
              <div>
                <Toggle on={config.monitor} onChange={(on) => patch({ monitor: on })} />
                <span className="dim">every track starts with monitoring on</span>
              </div>
            </div>
          )}
        </div>
      </section>
    </>
  );
}

// ---------------------------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------------------------

function GeneralPage({ info }: { info: AppInfo | null }) {
  const keys: [string, string][] = [
    ["1 – 8", "select a track"],
    ["R · O · P · S · M", "record · overdub · play · stop · monitor (selected track)"],
    ["O (continuous)", "on: every pass a new layer · again: ends at the end of the pass · during R: overdub next"],
    ["Q", "mute / unmute the selected track (at once, the loop keeps its place)"],
    ["U", "undo on the selected track: the running take, else its newest layer"],
    ["Z · Backspace", "undo the last take of any track"],
    ["C", "clear the selected track (asks first)"],
    ["I", "track details (pan, sends, latency, effects, layers)"],
    ["K", "click on/off"],
    ["T", "tap tempo (free looping, while every track is empty)"],
    ["Shift+Backspace", "clear all (asks first)"],
    ["F", "effect chain in/out"],
    ["X n · V n · D n", "toggle effect n · load preset n · delay note n"],
    ["Space / N", "start the score · Release"],
    ["Shift+S", "stop all (end the run)"],
    [`${IS_MAC ? "⌘" : "Ctrl"}+Enter · ${IS_MAC ? "⌘" : "Ctrl"}+S`, "compile · load the score (Arrangement)"],
    [`+ · − · ${IS_MAC ? "⌘" : "Ctrl"}+scroll · pinch`, `zoom the arrangement grid (+ and − also with ${IS_MAC ? "⌘" : "Ctrl"})`],
    [`${IS_MAC ? "⌘" : "Ctrl"}+0`, "fit the whole score into the arrangement grid"],
    [`${IS_MAC ? "⌘" : "Ctrl"}+click`, "MIDI-learn a control without the switch"],
    ["Esc", "cancel learn / close details"],
  ];
  return (
    <>
      <div>
        <h1>General</h1>
        <p className="lead">About this build, and every key.</p>
      </div>
      <section className="card">
        <div className="card-h">
          <h3>Application</h3>
        </div>
        <div className="card-b">
          <div className="srow">
            <label>Version</label>
            <div className="mono">{info?.version ?? "—"}</div>
          </div>
          <div className="srow">
            <label>Audio</label>
            <div>{IS_MAC ? "CoreAudio" : info?.asio_built ? "ASIO" : "without ASIO"}</div>
          </div>
          <div className="srow">
            <label>Limits</label>
            <div>
              {info ? `${info.max_tracks} tracks · ${info.max_layers} layers per track` : "—"}
            </div>
          </div>
          <div className="srow">
            <label>Log file</label>
            <div className="mono sel-text">{info ? (info.logging ? info.log_path : "logging off") : "—"}</div>
          </div>
        </div>
      </section>
      <section className="card">
        <div className="card-h">
          <h3>Keyboard</h3>
        </div>
        <table className="tt">
          <tbody>
            {keys.map(([k, v]) => (
              <tr key={k}>
                <td style={{ width: 220 }}>
                  <kbd>{k}</kbd>
                </td>
                <td>{v}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </>
  );
}
