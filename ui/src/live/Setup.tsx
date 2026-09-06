//! What has to be settled before a single sample can be recorded: which device, how big a buffer,
//! how fast, how long a loop, how much latency to take out - and which tracks exist at all.
//!
//! The defaults are the values phase 0 measured on this machine (ASIO, Focusrite, 48 kHz, 128
//! frames, 827 samples). They are also what the Rust side would fall back to, repeated here so the
//! musician sees the numbers instead of guessing them.

import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type {
  AppInfo,
  ConfigInfo,
  DeviceInfo,
  DeviceReport,
  Quantize,
  StartConfig,
  StartTrack,
} from "../types";
import { TapTempo } from "./TapTempo";

const STORE_KEY = "looper.setup";

const SAMPLE_RATES = [44100, 48000, 88200, 96000];

export const DEFAULT_SETUP: StartConfig = {
  host: "asio",
  device: null,
  sample_rate: 48000,
  buffer_frames: 128,
  input_channels: null,
  output_channels: null,
  force_buffer: false,
  tracks: [
    { name: "stimme", input_channel: 1, input_channel_right: null, pan: 0 },
    { name: "gitarre", input_channel: 2, input_channel_right: null, pan: 0 },
  ],
  bpm: 100,
  beats_per_bar: 4,
  beat_unit: 4,
  bars: 8,
  quantize: "loop",
  latency_samples: 827,
  monitor_gain: 1,
  click_gain: 1,
  click: true,
  monitor: false,
};

function loadStored(): StartConfig {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (!raw) return DEFAULT_SETUP;
    const parsed = JSON.parse(raw) as Partial<StartConfig>;
    // Merge, so a field added later still has its default - including per track, which is what
    // keeps a setup stored before the stereo rebuild loadable.
    const tracks: StartTrack[] = parsed.tracks?.length
      ? parsed.tracks.map((entry) => {
          // A setup stored before the stereo rebuild has neither of the last two fields.
          const stored = entry as Partial<StartTrack>;
          return {
            name: stored.name ?? "track",
            input_channel: stored.input_channel ?? 1,
            input_channel_right: stored.input_channel_right ?? null,
            pan: stored.pan ?? 0,
          };
        })
      : DEFAULT_SETUP.tracks;
    return { ...DEFAULT_SETUP, ...parsed, tracks };
  } catch {
    return DEFAULT_SETUP;
  }
}

function store(config: StartConfig): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(config));
  } catch {
    /* a full or blocked storage is not worth an error */
  }
}

/** Tightest buffer range every configuration of this device agrees on. */
function bufferRange(device: DeviceInfo | null): { min: number; max: number } | null {
  if (!device) return null;
  const configs = [...device.input_configs, ...device.output_configs].filter(
    (c) => c.min_buffer_frames !== null && c.max_buffer_frames !== null,
  );
  if (configs.length === 0) return null;
  const min = Math.max(...configs.map((c) => c.min_buffer_frames as number));
  const max = Math.min(...configs.map((c) => c.max_buffer_frames as number));
  return max >= min ? { min, max } : null;
}

function covers(configs: ConfigInfo[], rate: number): boolean {
  return configs.some((c) => c.min_sample_rate <= rate && rate <= c.max_sample_rate);
}

function maxChannels(device: DeviceInfo | null): number {
  if (!device || device.input_configs.length === 0) return 8;
  return Math.max(...device.input_configs.map((c) => c.channels));
}

// ---------------------------------------------------------------------------------------------

function NumberField({
  label,
  value,
  onChange,
  min,
  max,
  step,
  hint,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
  min?: number;
  max?: number;
  step?: number;
  hint?: string;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => {
    // Only pull the value in when it really differs - otherwise a half-typed "12." is destroyed.
    if (text.trim() !== "" && Number(text) !== value) setText(String(value));
  }, [value, text]);

  return (
    <label className="field">
      <span className="field-label">{label}</span>
      <input
        type="number"
        value={text}
        min={min}
        max={max}
        step={step}
        onChange={(e) => {
          setText(e.target.value);
          const n = Number(e.target.value);
          if (e.target.value.trim() !== "" && Number.isFinite(n)) onChange(n);
        }}
      />
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

interface Props {
  info: AppInfo | null;
  busy: boolean;
  onStart: (config: StartConfig) => void;
  onError: (message: string) => void;
}

export function Setup({ info, busy, onStart, onError }: Props) {
  const [config, setConfig] = useState<StartConfig>(loadStored);
  const [report, setReport] = useState<DeviceReport | null>(null);
  const [scanning, setScanning] = useState(false);

  const patch = useCallback((part: Partial<StartConfig>) => {
    setConfig((c) => {
      const next = { ...c, ...part };
      store(next);
      return next;
    });
  }, []);

  // One scan on mount, further ones only when asked for. No retry loop: if the device survey
  // fails, the message stays on screen until someone presses the button again.
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
        // Prefer the interface this project was measured on, else the host's default input.
        const preferred =
          host.devices.find((d) => d.name.toLowerCase().includes("focusrite")) ??
          host.devices.find((d) => d.name === host.default_input) ??
          host.devices.find((d) => d.input) ??
          null;
        const next = { ...c, host: host.name, device: preferred ? preferred.name : null };
        store(next);
        return next;
      });
    } catch (e) {
      onError((e as Error).message);
    } finally {
      setScanning(false);
    }
  }, [onError]);

  useEffect(() => {
    void scan();
  }, [scan]);

  const host = useMemo(
    () => report?.hosts.find((h) => h.name === config.host) ?? null,
    [report, config.host],
  );
  const device = useMemo(
    () => host?.devices.find((d) => d.name === config.device) ?? null,
    [host, config.device],
  );
  const range = useMemo(() => bufferRange(device), [device]);
  const channels = maxChannels(device);

  const rates = useMemo(() => {
    if (!device) return SAMPLE_RATES;
    const usable = SAMPLE_RATES.filter(
      (r) => covers(device.input_configs, r) || covers(device.output_configs, r),
    );
    return usable.length > 0 ? usable : SAMPLE_RATES;
  }, [device]);

  const asioMissing = report ? !report.asio_built || !report.hosts.some((h) => h.name === "asio") : false;
  const bufferOff = range !== null && (config.buffer_frames < range.min || config.buffer_frames > range.max);

  const setTrack = (index: number, part: Partial<StartTrack>) =>
    patch({ tracks: config.tracks.map((t, i) => (i === index ? { ...t, ...part } : t)) });

  const maxTracks = info?.max_tracks ?? 8;
  const trackProblem =
    config.tracks.length === 0
      ? "Mindestens ein Track."
      : config.tracks.some((t) => t.name.trim() === "")
        ? "Jeder Track braucht einen Namen."
        : config.tracks.some((t) => t.input_channel < 1 || (t.input_channel_right ?? 1) < 1)
          ? "Eingangskanäle sind eins-basiert."
          : config.tracks.some((t) => t.input_channel_right === t.input_channel)
            ? "Ein Stereo-Track braucht zwei verschiedene Eingänge."
            : null;

  /**
   * Switch one track between mono and stereo. Going stereo proposes the next input up, which is
   * how a stereo return is wired nine times out of ten; going mono simply drops the second one.
   */
  const setChannels = (index: number, count: number) => {
    const track = config.tracks[index];
    if (count < 2) {
      setTrack(index, { input_channel_right: null });
      return;
    }
    const proposal = track.input_channel < channels ? track.input_channel + 1 : 1;
    setTrack(index, { input_channel_right: track.input_channel_right ?? proposal });
  };

  return (
    <div className="setup">
      <div className="setup-cols">
        <section className="panel">
          <div className="panel-title">
            Gerät
            <div className="spacer" />
            <button className="btn btn-mini" onClick={() => void scan()} disabled={scanning}>
              {scanning ? "suche…" : "neu suchen"}
            </button>
          </div>
          <div className="panel-body">
            {asioMissing && (
              <p className="setup-warn">
                Kein ASIO in diesem Build oder auf diesem Rechner. Ohne ASIO ist die Latenz nicht
                brauchbar (siehe docs/architektur.md, Abschnitt 2).
              </p>
            )}
            {host?.error && <p className="setup-warn">{host.error}</p>}

            <label className="field">
              <span className="field-label">Host</span>
              <select value={config.host} onChange={(e) => patch({ host: e.target.value, device: null })}>
                {(report?.hosts ?? [{ name: config.host }]).map((h) => (
                  <option key={h.name} value={h.name}>
                    {h.name}
                  </option>
                ))}
              </select>
            </label>

            <label className="field field-wide">
              <span className="field-label">Gerät</span>
              <select
                value={config.device ?? ""}
                onChange={(e) => patch({ device: e.target.value === "" ? null : e.target.value })}
              >
                <option value="">(Standard des Hosts)</option>
                {(host?.devices ?? []).map((d) => (
                  <option key={d.name} value={d.name}>
                    {d.name}
                  </option>
                ))}
              </select>
              {device?.note && <span className="field-hint">{device.note}</span>}
            </label>

            <label className="field">
              <span className="field-label">Samplerate</span>
              <select value={config.sample_rate} onChange={(e) => patch({ sample_rate: Number(e.target.value) })}>
                {rates.map((r) => (
                  <option key={r} value={r}>
                    {r} Hz
                  </option>
                ))}
              </select>
            </label>

            <NumberField
              label="Puffer (Frames)"
              value={config.buffer_frames}
              onChange={(v) => patch({ buffer_frames: v })}
              min={range?.min}
              max={range?.max}
              step={16}
              hint={range ? `Gerät meldet ${range.min} bis ${range.max}` : "Gerät meldet keine Grenzen"}
            />
            {bufferOff && (
              <p className="setup-warn">
                {config.buffer_frames} Frames liegen ausserhalb dessen, was das Gerät meldet. Der
                Treiber wird das vermutlich ablehnen.
              </p>
            )}

            <NumberField
              label="Latenzkompensation (Samples)"
              value={config.latency_samples}
              onChange={(v) => patch({ latency_samples: Math.max(0, Math.round(v)) })}
              min={0}
              step={1}
              hint={`≈ ${(config.latency_samples / (config.sample_rate || 48000) * 1000).toFixed(2)} ms · nach jedem Wechsel von Gerät, Rate oder Puffer neu messen`}
            />
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">Takt und Loop</div>
          <div className="panel-body">
            <NumberField label="Tempo (BPM)" value={config.bpm} onChange={(v) => patch({ bpm: v })} min={20} max={300} step={0.5} />
            <TapTempo onTempo={(v) => patch({ bpm: v })} />
            <div className="field-row">
              <NumberField label="Schläge/Takt" value={config.beats_per_bar} onChange={(v) => patch({ beats_per_bar: Math.max(1, Math.round(v)) })} min={1} max={16} step={1} />
              <NumberField label="Notenwert" value={config.beat_unit} onChange={(v) => patch({ beat_unit: Math.max(1, Math.round(v)) })} min={1} max={16} step={1} />
            </div>
            <NumberField
              label="Loop-Länge (Takte)"
              value={config.bars}
              onChange={(v) => patch({ bars: Math.max(1, Math.round(v)) })}
              min={1}
              max={64}
              step={1}
              hint={`${((config.bars * config.beats_per_bar * 60) / (config.bpm || 100)).toFixed(2)} s bei ${config.bpm} BPM`}
            />
            <label className="field field-wide">
              <span className="field-label">Aufnahme startet</span>
              <select
                value={config.quantize}
                onChange={(e) => patch({ quantize: e.target.value as Quantize })}
              >
                <option value="loop">am nächsten Loop-Anfang</option>
                <option value="bar">an der nächsten Taktgrenze</option>
              </select>
              <span className="field-hint">
                {config.quantize === "loop"
                  ? `Einmal früh drücken reicht: R oder O irgendwo im Loop rüstet die Aufnahme für den Anfang des nächsten Loops scharf. Bis dahin steht auf der Karte, wie viele Takte noch fehlen — Zeit, zur Gitarre zu kommen.`
                  : `R oder O beginnen schon im nächsten Takt. Dann muss man bis Takt ${config.bars} warten und den Einsatz hetzen — dafür lässt sich mitten im Loop anfangen.`}
              </span>
            </label>
            <label className="field field-check">
              <input type="checkbox" checked={config.click} onChange={(e) => patch({ click: e.target.checked })} />
              <span>Klick beim Start an</span>
            </label>
            <label className="field field-check">
              <input type="checkbox" checked={config.monitor} onChange={(e) => patch({ monitor: e.target.checked })} />
              <span>Mithören beim Start an</span>
            </label>
          </div>
        </section>

        <section className="panel">
          <div className="panel-title">
            Tracks
            <div className="spacer" />
            <button
              className="btn btn-mini"
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
                    },
                  ],
                })
              }
            >
              + Track
            </button>
          </div>
          <div className="panel-body">
            <ul className="setup-tracks">
              {config.tracks.map((t, i) => (
                <li key={i}>
                  <span className="live-track-key mono">{i + 1}</span>
                  <input
                    className="setup-track-name"
                    value={t.name}
                    placeholder="Name"
                    onChange={(e) => setTrack(i, { name: e.target.value })}
                  />
                  <label className="setup-track-kind">
                    <span>Quelle</span>
                    <select
                      value={t.input_channel_right === null ? 1 : 2}
                      onChange={(e) => setChannels(i, Number(e.target.value))}
                      title="Ein Eingang nimmt mono auf, ein Paar stereo"
                    >
                      <option value={1}>mono</option>
                      <option value={2}>stereo</option>
                    </select>
                  </label>
                  <label className="setup-track-in">
                    <span>{t.input_channel_right === null ? "Eingang" : "links"}</span>
                    <input
                      type="number"
                      min={1}
                      max={channels}
                      value={t.input_channel}
                      onChange={(e) => setTrack(i, { input_channel: Math.max(1, Math.round(Number(e.target.value) || 1)) })}
                    />
                  </label>
                  {t.input_channel_right !== null && (
                    <label className="setup-track-in">
                      <span>rechts</span>
                      <input
                        type="number"
                        min={1}
                        max={channels}
                        value={t.input_channel_right}
                        onChange={(e) =>
                          setTrack(i, {
                            input_channel_right: Math.max(1, Math.round(Number(e.target.value) || 1)),
                          })
                        }
                      />
                    </label>
                  )}
                  <button
                    className="btn btn-mini btn-danger"
                    onClick={() => patch({ tracks: config.tracks.filter((_, j) => j !== i) })}
                    title="Track entfernen"
                  >
                    ✕
                  </button>
                </li>
              ))}
            </ul>
            <p className="field-hint">
              Höchstens {maxTracks} Tracks. Das Gerät bietet {channels} Eingangskanäle. Ein Mikrofon
              oder eine Gitarre nimmt mono auf — das halbiert den Speicher und klingt keinen Deut
              anders. Ein Klavier oder eine Fläche aus einer Sample-Bibliothek gehört auf ein
              Eingangspaar; die beiden müssen nicht nebeneinander liegen. Panorama und Lautstärke
              werden später auf der Track-Karte gesetzt.
            </p>
          </div>
        </section>
      </div>

      <div className="setup-start">
        {trackProblem && <span className="setup-warn">{trackProblem}</span>}
        <button
          className="btn btn-primary btn-start"
          disabled={busy || trackProblem !== null}
          onClick={() => onStart(config)}
        >
          {busy ? "starte…" : "Engine starten"}
        </button>
      </div>
    </div>
  );
}
