//! The plugin slots of one track or one bus: load, bypass, reorder, remove, open the plugin's own
//! window - and, for a plugin without one (or as a quick overview), a generic list of its
//! parameters with sliders.
//!
//! A minimal, working surface. The final mixer (design/arrangement-mixer.html) builds its own
//! strip on the same `pluginApi` and `usePlugins()`; nothing here has to survive that.
//!
//! Every bypass button and every parameter row carries its `data-midi` address, so both are
//! MIDI-learnable the same way every other control is.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { pluginApi } from "./api";
import { markScanning, onParamChange, setCatalog, setRack, usePlugins } from "./store";
import type { ParamView, SlotView } from "./types";
import "./plugins.css";

interface Props {
  /** `track.1` … `track.8`, `bus.main`, `bus.monitor`. */
  target: string;
  /** Heading; omitted: "Plugins". */
  title?: string;
}

export function PluginSlots({ target, title }: Props) {
  const { rack, catalog, scanning } = usePlugins();
  const [error, setError] = useState<string | null>(null);
  const [choice, setChoice] = useState("");
  const [openParams, setOpenParams] = useState<number | null>(null);
  const slots = useMemo(
    () => rack.slots.filter((s) => s.target === target).sort((a, b) => a.slot - b.slot),
    [rack, target],
  );
  const effects = useMemo(
    () => (catalog?.plugins ?? []).filter((p) => p.status === "ok" && p.kind === "effect"),
    [catalog],
  );

  const run = useCallback(async (work: () => Promise<unknown>) => {
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const add = () =>
    run(async () => {
      if (!choice) return;
      setRack(await pluginApi.load(target, choice));
    });

  const scan = () =>
    run(async () => {
      markScanning();
      setCatalog(await pluginApi.scan(false));
    });

  if (!rack.supported) {
    return <div className="hint">Plugins are available on macOS only for now.</div>;
  }

  return (
    <div className="plg">
      <div className="caps plg-head">
        {title ?? "Plugins"} <span className="dim2">{slots.length}</span>
        {!rack.engine_running && slots.length > 0 && (
          <span className="plg-tag" title="The plugins start with the engine.">
            engine stopped
          </span>
        )}
      </div>
      {slots.length === 0 && <div className="hint">No plugin. Add one below.</div>}
      {slots.map((s, i) => (
        <SlotRow
          key={`${s.slot}-${s.plugin_id}`}
          slot={s}
          first={i === 0}
          last={i === slots.length - 1}
          prevSlot={i > 0 ? slots[i - 1].slot : null}
          nextSlot={i < slots.length - 1 ? slots[i + 1].slot : null}
          paramsOpen={openParams === s.slot}
          toggleParams={() => setOpenParams(openParams === s.slot ? null : s.slot)}
          run={run}
          onNoEditor={() => setOpenParams(s.slot)}
        />
      ))}
      <div className="plg-add">
        <select value={choice} onChange={(e) => setChoice(e.target.value)} disabled={effects.length === 0}>
          <option value="">{effects.length === 0 ? "No plugins found yet" : "Add a plugin…"}</option>
          {effects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name} — {p.vendor} ({p.format === "vst3" ? "VST3" : "AU"})
            </option>
          ))}
        </select>
        <button className="btn s" onClick={add} disabled={!choice || slots.length >= rack.max_slots}>
          Add
        </button>
        <button
          className="btn s ghost"
          onClick={scan}
          disabled={scanning !== null}
          title="Look for new or updated plugins. Each one is tested in a separate process first."
        >
          {scanning !== null ? `Scanning${scanning ? `: ${scanning}` : "…"}` : "Scan"}
        </button>
      </div>
      {error && <div className="plg-error">{error}</div>}
    </div>
  );
}

interface RowProps {
  slot: SlotView;
  first: boolean;
  last: boolean;
  prevSlot: number | null;
  nextSlot: number | null;
  paramsOpen: boolean;
  toggleParams: () => void;
  run: (work: () => Promise<unknown>) => Promise<void>;
  onNoEditor: () => void;
}

function SlotRow({ slot: s, first, last, prevSlot, nextSlot, paramsOpen, toggleParams, run, onNoEditor }: RowProps) {
  const running = s.active && !s.bypass;
  const title = [
    `${s.vendor} · ${s.format === "vst3" ? "VST3" : "Audio Unit"}`,
    s.latency > 0 ? `latency ${s.latency} samples (made up for on the loop)` : "no latency",
    s.active ? `${s.cpu_us.toFixed(0)} µs per block` : "not running",
  ].join("\n");
  return (
    <div className={`plg-slot${s.bypass ? " bypassed" : ""}`}>
      <div className="plg-row">
        <span className="plg-num">{s.slot + 1}</span>
        <span className={`plg-dot${running ? " on" : ""}`} title={running ? "audio passes" : "idle"} />
        <span className="plg-name" title={title}>
          {s.name}
          <span className="plg-fmt">{s.format === "vst3" ? "VST3" : "AU"}</span>
          {s.latency > 0 && <span className="plg-fmt">{s.latency} smp</span>}
          {s.active && (
            <span className="plg-fmt plg-pk" title="Output peak of this slot">
              {s.peak > 0 ? `${(20 * Math.log10(s.peak)).toFixed(0)} dB` : "−∞ dB"}
            </span>
          )}
        </span>
        <button
          className={`btn s${s.bypass ? " on" : ""}`}
          data-midi={s.bypass_address}
          title="Bypass: the plugin is skipped, the signal passes unchanged"
          onClick={() => run(async () => setRack(await pluginApi.bypass(s.target, s.slot, !s.bypass)))}
        >
          {s.bypass ? "Bypassed" : "Bypass"}
        </button>
        <button
          className={`btn s${s.editor_open ? " on" : ""}`}
          title="Open the plugin's own window"
          onClick={() =>
            run(async () => {
              try {
                setRack(await pluginApi.editor(s.target, s.slot, !s.editor_open));
              } catch (e) {
                onNoEditor();
                throw e;
              }
            })
          }
        >
          Window
        </button>
        <button className={`btn s${paramsOpen ? " on" : ""}`} onClick={toggleParams} title="All parameters">
          Params
        </button>
        <button
          className="btn s sq ghost"
          disabled={first || prevSlot === null}
          title="Move up"
          onClick={() => run(async () => setRack(await pluginApi.move(s.target, s.slot, prevSlot ?? s.slot)))}
        >
          ↑
        </button>
        <button
          className="btn s sq ghost"
          disabled={last || nextSlot === null}
          title="Move down"
          onClick={() => run(async () => setRack(await pluginApi.move(s.target, s.slot, nextSlot ?? s.slot)))}
        >
          ↓
        </button>
        <button
          className="btn s sq danger"
          title="Remove"
          onClick={() => run(async () => setRack(await pluginApi.remove(s.target, s.slot)))}
        >
          ✕
        </button>
      </div>
      {s.error && <div className="plg-error">{s.error}</div>}
      {paramsOpen && <ParamList target={s.target} slot={s.slot} run={run} />}
    </div>
  );
}

/** A dragged slider would otherwise send a command per pixel. */
const PARAM_THROTTLE_MS = 40;

export function ParamList({
  target,
  slot,
  run,
}: {
  target: string;
  slot: number;
  run: (work: () => Promise<unknown>) => Promise<void>;
}) {
  const [params, setParams] = useState<ParamView[] | null>(null);
  const [filter, setFilter] = useState("");
  const pending = useRef(new Map<number, number>());
  const timer = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    pluginApi.params(target, slot).then(
      (p) => alive && setParams(p),
      () => alive && setParams([]),
    );
    const off = onParamChange((t, s, values) => {
      if (t !== target || s !== slot) return;
      setParams((list) =>
        list?.map((p) => {
          const v = values.find((x) => x.id === p.id);
          return v ? { ...p, value: v.value, text: v.text } : p;
        }) ?? list,
      );
    });
    return () => {
      alive = false;
      off();
    };
  }, [target, slot]);

  const flush = useCallback(() => {
    timer.current = null;
    const batch = [...pending.current.entries()];
    pending.current.clear();
    for (const [id, value] of batch) {
      void run(async () => {
        const got = await pluginApi.setParam(target, slot, id, value);
        setParams((list) => list?.map((p) => (p.id === got.id ? { ...p, value: got.value, text: got.text } : p)) ?? list);
      });
    }
  }, [run, target, slot]);

  const change = (id: number, value: number) => {
    setParams((list) => list?.map((p) => (p.id === id ? { ...p, value } : p)) ?? list);
    pending.current.set(id, value);
    if (timer.current === null) timer.current = window.setTimeout(flush, PARAM_THROTTLE_MS);
  };

  if (params === null) return <div className="hint">Reading parameters…</div>;
  if (params.length === 0) return <div className="hint">This plugin shows no parameters.</div>;
  const shown = filter
    ? params.filter((p) => p.name.toLowerCase().includes(filter.toLowerCase()))
    : params;
  return (
    <div className="plg-params">
      {params.length > 12 && (
        <input
          className="plg-filter"
          placeholder={`Filter ${params.length} parameters`}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      )}
      {shown.slice(0, 200).map((p) => (
        <label key={p.id} className="plg-param" data-midi={p.address} title={p.address}>
          <span className="plg-pname">{p.name}</span>
          <input
            type="range"
            min={0}
            max={1}
            step={p.steps > 0 ? 1 / p.steps : 0.001}
            value={p.value}
            disabled={p.read_only}
            onChange={(e) => change(p.id, Number(e.target.value))}
            onDoubleClick={() => change(p.id, p.default)}
          />
          <span className="plg-pval">{p.text}</span>
        </label>
      ))}
    </div>
  );
}
