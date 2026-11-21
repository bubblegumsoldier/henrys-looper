//! The status pill's popover: what the engine opened, every counter that should be zero, the two
//! bus volumes with their meters, the output meters per device channel (the sum on the socket and
//! what the safety limiter took off), the grid, the click, "clear all" and the engine switch.
//!
//! These are the global controls of the old Live header and bus strip. The stage mockup has no
//! place for them, and they are not stage controls - but they must stay one click away (turning
//! the headphones down mid-song) and MIDI-learnable, so here they are.

import { useEffect, useRef } from "react";
import { api } from "../api";
import { busGainAddress, GLOBAL, quantizeAddress } from "../midi/addresses";
import { useStatusEffect, useStatusSlice } from "../status";
import { HMeter } from "../live/HMeter";
import { currentConfig } from "../setup/config";
import { Icon } from "./Icon";
import { run } from "./notes";
import { engineWords, startEngine, stopEngine, useEngine } from "./engine";
import { T, gainDb } from "./terms";
import { budgetText, loadAdvice, percentText } from "./loadModel";
import { IDLE_LOAD, type BusId, type LooperStatus } from "../types";

interface Counters {
  running: boolean;
  xruns: number;
  underruns: number;
  overruns: number;
  others: number;
  ignored: number;
  maxMs: number;
  quantize: "bar" | "loop";
  click: boolean;
  buses: string;
  /** `channel:buses` per device output channel - what the output rows are built from. */
  outputs: string;
}

function pick(s: LooperStatus): Counters {
  return {
    running: s.running,
    xruns: s.xruns,
    underruns: s.fifo_underruns,
    overruns: s.fifo_overruns,
    others: s.other_errors,
    ignored: s.ignored_commands,
    maxMs: Math.round(s.max_callback_ms * 10) / 10,
    quantize: s.quantize,
    click: s.click,
    buses: s.buses.map((b) => `${b.id}:${b.gain}:${b.channel_label}`).join("|") + (s.buses_collapsed ? "!" : ""),
    outputs: (s.outputs ?? []).map((o) => `${o.channel}:${o.buses}`).join("|"),
  };
}

function same(a: Counters, b: Counters): boolean {
  return (
    a.running === b.running &&
    a.xruns === b.xruns &&
    a.underruns === b.underruns &&
    a.overruns === b.overruns &&
    a.others === b.others &&
    a.ignored === b.ignored &&
    a.maxMs === b.maxMs &&
    a.quantize === b.quantize &&
    a.click === b.click &&
    a.buses === b.buses &&
    a.outputs === b.outputs
  );
}

/** Click volume (monitor bus only) or the volume of every monitored input - linear, 1 = unity. */
function LevelRow({
  label,
  icon,
  pick,
  set,
}: {
  label: string;
  icon: string;
  pick: (s: LooperStatus) => number;
  set: (gain: number) => Promise<unknown>;
}) {
  const value = useStatusSlice(pick);
  return (
    <div className="busrow">
      <span className="nm">
        <Icon name={icon} />
        {label}
      </span>
      <span className="gain">
        <input
          className="range"
          type="range"
          min={0}
          max={2}
          step={0.05}
          value={value}
          onChange={(e) => run(() => set(Number(e.target.value)))}
        />
        <span className="num val">{gainDb(value)}</span>
      </span>
      <span />
    </div>
  );
}

const peaks = (id: BusId) => (s: LooperStatus) => s.buses.find((b) => b.id === id)?.peaks ?? [0, 0];
const outPeak = (channel: number) => (s: LooperStatus) => [s.outputs?.find((o) => o.channel === channel)?.peak ?? 0];

/** How long the limiter readout keeps its deepest value, so a short reduction can be read. */
const REDUCTION_HOLD_MS = 1500;

/**
 * What the safety limiter is taking off one output, e.g. "Limiter −2.1 dB", held for a moment;
 * empty while it does nothing. Written straight into the DOM, like a meter readout.
 */
function Reduction({ channel }: { channel: number }) {
  const el = useRef<HTMLSpanElement | null>(null);
  const held = useRef({ db: 0, at: -Infinity, text: "" });
  useStatusEffect((s) => {
    const now = performance.now();
    const db = Math.min(0, s.outputs?.find((o) => o.channel === channel)?.reduction_db ?? 0);
    const h = held.current;
    if (db < -0.05 && (db <= h.db || now - h.at > REDUCTION_HOLD_MS)) {
      h.db = db;
      h.at = now;
    } else if (now - h.at > REDUCTION_HOLD_MS) {
      h.db = 0;
    }
    const text = h.db < -0.05 ? `${T.limiting} −${Math.abs(h.db).toFixed(1)} dB` : "";
    if (text !== h.text && el.current) {
      el.current.textContent = text;
      h.text = text;
    }
  });
  return <span className="num warn-text" ref={el} />;
}

/**
 * The audio load in words and numbers - the details behind the title bar's load meter. Renders
 * when a rounded number changes, i.e. a few times a second at most.
 */
function LoadSection() {
  const sig = useStatusSlice((s) => {
    const l = s.load ?? IDLE_LOAD;
    return [
      Math.round(l.peak_percent),
      Math.round(l.avg_percent),
      Math.round(l.plugin_percent),
      l.overruns,
      l.late_blocks,
      l.late_wakeups,
      l.driver_overloads,
      l.last_overrun ?? "",
      l.last_overrun_kind ?? "",
      Math.round(l.last_overrun_percent),
      l.block_frames,
      l.budget_ms,
      s.sample_rate,
    ].join("|");
  });
  const [peak, avg, plugins, overruns, lateBlocks, lateWakeups, overloads, lastAt, lastKind, lastPct, frames, budget, rate] =
    sig.split("|");
  const load: typeof IDLE_LOAD = {
    ...IDLE_LOAD,
    overruns: Number(overruns),
    budget_ms: Number(budget),
  };
  const advice = loadAdvice(load);
  const bad = Number(overruns) > 0;
  return (
    <div className="pop-s">
      <div className="caps" title="Share of each audio block's time budget (block length / sample rate) the engine needs. Above 100 % the block was late.">
        Audio load
      </div>
      <div className="counters load-counters">
        <span className={Number(peak) >= 100 ? "bad" : Number(peak) >= 70 ? "warn" : ""}>
          peak <b>{percentText(Number(peak))}</b>
        </span>
        <span>
          average <b>{percentText(Number(avg))}</b>
        </span>
        <span title="Instruments and insert plugins, as a share of the budget">
          plugins <b>{percentText(Number(plugins))}</b>
        </span>
        <span className={bad ? "bad" : "ok"} title="Late blocks, late wake-ups and driver overloads; what comes together counts once">
          overruns <b>{overruns}</b>
        </span>
        <span title="Blocks that took longer than their budget · callbacks the system woke too late · overloads CoreAudio reported">
          late blocks <b>{lateBlocks}</b> · late wake-ups <b>{lateWakeups}</b> · driver <b>{overloads}</b>
        </span>
      </div>
      <div className="mono dim load-facts">
        {Number(frames) > 0 ? `${frames} frames at ${Number(rate) / 1000} kHz = ${budgetText(load)} per block` : "Measuring…"}
        {lastAt && ` · last overrun ${lastAt} (${lastKind}${Number(lastPct) > 0 ? `, ${percentText(Number(lastPct))}` : ""})`}
      </div>
      {advice && <div className="warn-text load-advice">{advice}</div>}
    </div>
  );
}

export function EnginePanel({ onClose, onSetup }: { onClose: () => void; onSetup: () => void }) {
  const c = useStatusSlice(pick, same);
  const { info, busy, view } = useEngine();
  const box = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const onDown = (e: PointerEvent) => {
      const t = e.target as HTMLElement;
      if (box.current && !box.current.contains(t) && !t.closest(".status-pill, .load-meter")) onClose();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onDown);
    };
  }, [onClose]);

  const buses = c.buses
    ? c.buses
        .replace(/!$/, "")
        .split("|")
        .filter(Boolean)
        .map((entry) => {
          const [id, gain, label] = entry.split(":");
          return { id: id as BusId, gain: Number(gain), label };
        })
    : [];
  const collapsed = c.buses.endsWith("!");
  const outputs = c.outputs
    ? c.outputs.split("|").map((entry) => {
        const [channel, on] = entry.split(":");
        return { channel: Number(channel), on };
      })
    : [];
  const other = c.quantize === "loop" ? "bar" : "loop";

  return (
    <div className="popover engine-pop" ref={box} role="dialog" aria-label={T.engine}>
      <div className="pop-h">
        <span className={`pill${c.running ? "" : " off"}`}>
          <i className="led" />
          {c.running ? T.engineRunning : engineWords(view)}
        </span>
        <div className="grow" />
        <button className="btn s" onClick={onSetup}>
          <Icon name="gear" />
          {T.setup}
        </button>
      </div>

      {c.running && info && (
        <div className="pop-s mono dim facts">
          {info.host} · {info.input_device} · {info.input_format}/{info.output_format} ·{" "}
          {info.driver_input_frames ?? info.buffer_frames} frames · {info.latency_ms.toFixed(2)} ms · up to{" "}
          {info.max_layers} layers per track · {info.memory_mb.toFixed(0)} MB
        </div>
      )}

      {c.running && (
        <>
          <div className="pop-s counters">
            <span className={c.xruns ? "bad" : "ok"}>
              {T.dropouts} <b>{c.xruns}</b>
            </span>
            <span className={c.underruns ? "warn" : ""}>
              FIFO empty <b>{c.underruns}</b>
            </span>
            <span className={c.overruns ? "bad" : ""}>
              input lost <b>{c.overruns}</b>
            </span>
            <span className={c.others ? "bad" : ""}>
              stream errors <b>{c.others}</b>
            </span>
            <span>
              refused <b>{c.ignored}</b>
            </span>
            <span>
              callback max <b>{c.maxMs.toFixed(1)} ms</b>
            </span>
          </div>

          <LoadSection />

          <div className="pop-s">
            <div className="caps">Buses</div>
            {buses.map((b) => (
              <div className="busrow" key={b.id}>
                <span className="nm">
                  <Icon name={b.id === "main" ? "speaker" : "phones"} />
                  {b.id === "main" ? T.main : T.monitorBus}
                  <span className="dim2">{b.label}</span>
                </span>
                <span className="gain" data-midi={busGainAddress(b.id)}>
                  <input
                    className="range"
                    type="range"
                    min={0}
                    max={2}
                    step={0.01}
                    value={b.gain}
                    disabled={collapsed && b.id === "monitor"}
                    onChange={(e) => run(() => api.busGain(b.id, Number(e.target.value)))}
                    title={collapsed && b.id === "monitor" ? "Both buses share one output: one mix, one fader (Main)." : "Only this bus"}
                  />
                  <span className="num val">{gainDb(b.gain)}</span>
                </span>
                <HMeter pick={peaks(b.id)} channels={2} className="grow" />
              </div>
            ))}
            {collapsed && <div className="dim out-note">{T.sharedOutputHint}</div>}
          </div>

          {outputs.length > 0 && (
            <div className="pop-s">
              <div className="caps" title={T.limiterHint}>
                {T.outputs}
              </div>
              {outputs.map((o) => (
                <div className="busrow" key={o.channel} title={T.limiterHint}>
                  <span className="nm">
                    <Icon name="speaker" />
                    {T.outputChannel} {o.channel}
                    <span className="dim2">{o.on || "unused"}</span>
                  </span>
                  <Reduction channel={o.channel} />
                  <HMeter pick={outPeak(o.channel)} channels={1} className="grow" />
                </div>
              ))}
            </div>
          )}

          <div className="pop-s">
            <div className="caps">Levels</div>
            <LevelRow label="Click" icon="metro" pick={(s) => s.click_gain} set={(g) => api.clickGain(g)} />
            <LevelRow label="Monitoring" icon="hear" pick={(s) => s.monitor_gain} set={(g) => api.monitorGain(g)} />
          </div>

          <div className="pop-s row wrap">
            <button
              className="btn"
              data-midi={quantizeAddress(other)}
              onClick={() => run(() => api.setQuantize(other))}
              title="The grid takes snap to. Click switches."
            >
              <Icon name={c.quantize === "loop" ? "loop" : "bar"} />
              {c.quantize === "loop" ? T.quantizeLoop : T.quantizeBar}
            </button>
            <button
              className={`btn${c.click ? " on" : ""}`}
              data-midi={GLOBAL.click}
              onClick={() => run(() => api.setClick(!c.click))}
            >
              <Icon name="metro" />
              {c.click ? T.clickOn : T.clickOff}
            </button>
            <div className="grow" />
            <button className="btn danger" data-midi={GLOBAL.clearAll} onClick={() => run(() => api.clearAll())} title="Delete every layer of every track">
              <Icon name="trash" />
              {T.clearAll}
            </button>
          </div>
        </>
      )}

      <div className="pop-f">
        {c.running ? (
          <button className="btn danger" disabled={busy} onClick={stopEngine}>
            <Icon name="power" />
            {T.stopEngine}
          </button>
        ) : (
          <button className="btn accent" disabled={busy} onClick={() => void startEngine(currentConfig())}>
            <Icon name="power" />
            {busy ? "Starting…" : T.startEngine}
          </button>
        )}
      </div>
    </div>
  );
}
