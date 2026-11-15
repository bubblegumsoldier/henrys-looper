//! A channel's devices as one column, top to bottom in signal order - the same component in the
//! Live view's track detail and in the mixer's channel panel, for tracks and for buses:
//!
//! ```text
//!  DEVICES · voice                [Preset  Voice ▾] [⏻ Chain on]
//!  ─ Built-in ─────────────────────────────────────────────────
//!  ⏻ High-pass                                  80 Hz        ▸
//!  ⏻ EQ                                         ~~~~         ▾
//!     [ graph with the three band handles, band knobs ]
//!  ⏻ Compressor                    −18 dB · 3:1  ▮▮▯         ▸
//!  ⏻ Delay                              1/8. · 22 %          ▸
//!  ⏻ Reverb                       62 % room · 18 %           ▸
//!  ─ Plug-ins ─────────────────────────────────────────────────
//!  ⏻ ValhallaRoom                     Valhalla DSP      ⠿ ▸
//!  + Add plugin
//! ```
//!
//! * The power button of a row switches that device; everything else of the row opens its editor
//!   right under it (an accordion - one open at a time, remembered per channel).
//! * A device that is off is dimmed, but its controls work: turning one switches it on (and the
//!   chain in) - see `TouchFxParam` on the Rust side.
//! * Built-in devices have the engine's fixed order and come before the plug-ins; plug-ins can be
//!   dragged among themselves.
//! * Keyboard: ↑/↓ move between rows, Enter or Space opens and closes one.

import { useMemo, useRef, useState, type DragEvent, type KeyboardEvent, type ReactNode } from "react";
import { fxAddress, fxSlotAddress, trackAddress } from "../midi/addresses";
import { pluginApi } from "../plugins/api";
import { instrumentApi, instrumentOf, setInstruments, useInstruments } from "../plugins/instruments";
import { ParamList } from "../plugins/PluginSlots";
import { setRack, usePlugins } from "../plugins/store";
import type { SlotView } from "../plugins/types";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import { useStatusSlice } from "../status";
import type { FxSlotName, FxStatus } from "../types";
import { FX_NAME, FX_SLOT_ORDER, fxOn, type FxActions } from "../mixer/fxModel";
import { curvePath, responseDb } from "../mixer/EqGraph";
import { InstrumentPicker, NoteLed } from "../mixer/InstrumentSlot";
import { CompEditor, CompReduction, DelayEditor, EqEditor, HighPassEditor, PresetPicker, ReverbEditor } from "./editors";
import { FORMAT, PluginPicker, attempt } from "./PluginPicker";
import { builtinSummary, defaultDevice, deviceOrder, pluginId, pluginSummary, type DeviceId, type Owner } from "./model";
import { openDevice, useOpenDevice } from "./store";
import "./devices.css";

/** A track's column: its effect chain, and its instrument if it is an instrument track. */
export interface TrackChain {
  index: number;
  name: string;
  instrument: boolean;
  fx: FxStatus;
  actions: FxActions;
}

interface Props {
  owner: Owner;
  /** Absent for a bus: a bus has plug-in inserts only. */
  track?: TrackChain;
  /** `stage`: big rows for the Live view. `dense`: the arrangement's channel panel. */
  size: "stage" | "dense";
  /** Caption after "Devices", e.g. the bus name. */
  title?: string;
}

export function DeviceChain({ owner, track, size, title }: Props) {
  const { rack } = usePlugins();
  const plugins = useMemo(
    () => rack.slots.filter((s) => s.target === owner).sort((a, b) => a.slot - b.slot),
    [rack, owner],
  );
  const order = deviceOrder({ instrument: !!track?.instrument, builtin: !!track, plugins: plugins.map((p) => p.slot) });
  const stored = useOpenDevice(owner);
  const open = stored === undefined || (stored !== null && !order.includes(stored)) ? defaultDevice(order) : stored;
  const toggle = (id: DeviceId) => openDevice(owner, open === id ? null : id);
  const list = useRef<HTMLDivElement | null>(null);

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const rows = [...(list.current?.querySelectorAll<HTMLElement>(".dev-main") ?? [])];
    const at = rows.indexOf(document.activeElement as HTMLElement);
    if (at < 0) return;
    e.preventDefault();
    rows[Math.max(0, Math.min(rows.length - 1, at + (e.key === "ArrowDown" ? 1 : -1)))]?.focus();
  };

  const fx = track?.fx;

  return (
    <div className={`devchain ${size}`}>
      <div className="dev-head">
        <span className="caps">
          {T.devices}
          {title ? ` · ${title}` : track ? ` · ${track.name}` : ""}
        </span>
        {track && fx && (
          <div className="dev-head-ctl">
            <PresetPicker track={track.index} fx={fx} actions={track.actions} />
            <button
              className={`btn s chainsw${fx.bypass ? " out" : " on"}`}
              data-midi={fxAddress(track.index, "bypass")}
              onClick={() => track.actions.fxBypass(track.index, !fx.bypass)}
              aria-pressed={!fx.bypass}
              title={
                fx.bypass
                  ? "The built-in chain is bypassed: the track passes bit for bit. Click to switch it in."
                  : "The built-in chain is in the signal path. Click to bypass it - bit for bit, the panic switch (key F in Live)."
              }
            >
              <Icon name="power" />
              {fx.bypass ? T.chainBypassedShort : T.chainOn}
            </button>
          </div>
        )}
      </div>

      {track && fx?.bypass && (
        <button className="bypbanner" onClick={() => track.actions.fxBypass(track.index, false)} title={T.chainBypassedHint}>
          <Icon name="power" />
          {T.chainBypassed}
        </button>
      )}

      <div className="devlist" ref={list} onKeyDown={onKey} role="list" aria-label={`${T.devices}${track ? ` of ${track.name}` : ""}`}>
        {track?.instrument && (
          <InstrumentRow track={track} open={open === "instrument"} onToggle={() => toggle("instrument")} />
        )}
        {track && fx && (
          <>
            <div className="dev-sep" role="presentation">
              <span>{T.builtIn}</span>
            </div>
            {FX_SLOT_ORDER.map((slot) => (
              <BuiltinRow key={slot} slot={slot} track={track} fx={fx} open={open === slot} onToggle={() => toggle(slot)} />
            ))}
          </>
        )}
        <div className="dev-sep" role="presentation">
          <span>{T.plugIns}</span>
        </div>
        <PluginRows owner={owner} plugins={plugins} open={open} toggle={toggle} />
      </div>
    </div>
  );
}

// ---- one row ---------------------------------------------------------------------------------

interface RowProps {
  id: string;
  name: string;
  summary?: ReactNode;
  /** The power switch; absent for an instrument (it has a note light instead). */
  power?: { on: boolean; address?: string; title: string; toggle: () => void };
  /** Instead of the power switch. */
  lead?: ReactNode;
  /** On, and heard - lit. On while the chain is bypassed - a hollow ring. */
  state: "on" | "held" | "off" | "err";
  open: boolean;
  onToggle: () => void;
  children?: ReactNode;
  trail?: ReactNode;
  drag?: {
    onDragStart: (e: DragEvent) => void;
    onDragOver: (e: DragEvent) => void;
    onDrop: (e: DragEvent) => void;
    onDragEnd: () => void;
    over: boolean;
  };
  title?: string;
}

function DeviceRow(p: RowProps) {
  return (
    <div
      className={`dvc ${p.state}${p.open ? " open" : ""}${p.drag?.over ? " dropover" : ""}`}
      role="listitem"
      draggable={!!p.drag}
      onDragStart={p.drag?.onDragStart}
      onDragOver={p.drag?.onDragOver}
      onDrop={p.drag?.onDrop}
      onDragEnd={p.drag?.onDragEnd}
    >
      <div className="dev-h">
        {p.power ? (
          <button
            className={`pwr${p.power.on ? " on" : ""}`}
            data-midi={p.power.address}
            onClick={p.power.toggle}
            aria-pressed={p.power.on}
            aria-label={`${p.name} ${p.power.on ? T.fxOn : T.fxOff}`}
            title={p.power.title}
          >
            <Icon name="power" />
          </button>
        ) : (
          <span className="pwr lead">{p.lead}</span>
        )}
        <button className="dev-main" onClick={p.onToggle} aria-expanded={p.open} title={p.title}>
          <span className="dev-n">{p.name}</span>
          <span className="dev-s">{p.summary}</span>
          <Icon name={p.open ? "chev-d" : "chev-r"} className="dev-chev" />
        </button>
        {p.trail}
      </div>
      {p.open && <div className="dev-b">{p.children}</div>}
    </div>
  );
}

// ---- built-in devices --------------------------------------------------------------------------

function BuiltinRow({ slot, track, fx, open, onToggle }: { slot: FxSlotName; track: TrackChain; fx: FxStatus; open: boolean; onToggle: () => void }) {
  const on = fxOn(fx, slot);
  const i = track.index;
  const editor = { track: i, fx, actions: track.actions };
  return (
    <DeviceRow
      id={slot}
      name={FX_NAME[slot]}
      summary={<BuiltinSummary slot={slot} fx={fx} track={i} on={on} />}
      power={{
        on,
        address: fxSlotAddress(i, slot),
        toggle: () => track.actions.fxEnable(i, slot, !on),
        title: on
          ? `${FX_NAME[slot]} on${fx.bypass ? " (the chain is bypassed)" : ""} - click to switch off`
          : `${FX_NAME[slot]} off - click to switch on (turning one of its knobs does that too)`,
      }}
      state={on ? (fx.bypass ? "held" : "on") : "off"}
      open={open}
      onToggle={onToggle}
      title={`${FX_NAME[slot]} - click to ${open ? "fold" : "edit"}`}
    >
      {slot === "high_pass" && <HighPassEditor {...editor} />}
      {slot === "eq" && <EqEditor {...editor} />}
      {slot === "comp" && <CompEditor {...editor} on={on} />}
      {slot === "delay" && <DelayEditor {...editor} />}
      {slot === "reverb" && <ReverbEditor {...editor} />}
    </DeviceRow>
  );
}

const MINI_W = 46;
const MINI_H = 14;

function BuiltinSummary({ slot, fx, track, on }: { slot: FxSlotName; fx: FxStatus; track: number; on: boolean }) {
  const fs = useStatusSlice((s) => s.sample_rate) || 48000;
  if (slot === "eq") {
    const d = curvePath(responseDb(fx, fs, 40, { hp: false, eq: true, heard: false }), MINI_W, MINI_H, 8);
    return (
      <>
        <svg className="minicurve" viewBox={`0 0 ${MINI_W} ${MINI_H}`} width={MINI_W} height={MINI_H} aria-hidden="true">
          <line x1={0} x2={MINI_W} y1={MINI_H / 2} y2={MINI_H / 2} />
          <path d={d} />
        </svg>
        <span>{builtinSummary(fx, slot)}</span>
      </>
    );
  }
  if (slot === "comp") {
    return (
      <>
        <span>{builtinSummary(fx, slot)}</span>
        <CompReduction track={track} on={on && !fx.bypass} mini />
      </>
    );
  }
  return <span>{builtinSummary(fx, slot)}</span>;
}

// ---- instrument ------------------------------------------------------------------------------

function InstrumentRow({ track, open, onToggle }: { track: TrackChain; open: boolean; onToggle: () => void }) {
  const { rack } = useInstruments();
  const inst = instrumentOf(rack, track.name);
  const [picker, setPicker] = useState<HTMLElement | null>(null);
  const loading = rack.loading?.includes(track.name) ?? false;
  const summary = !rack.supported
    ? "macOS only for now"
    : loading
      ? T.instrumentLoading
      : !inst
      ? "none - choose one"
      : inst.error
        ? "failed to start"
        : `${inst.vendor || FORMAT[inst.format]}${inst.active ? "" : " · starts with the engine"}`;
  return (
    <DeviceRow
      id="instrument"
      name={inst ? inst.name : T.instrument}
      summary={<span>{summary}</span>}
      lead={<NoteLed index={track.index} />}
      state={inst?.error ? "err" : inst ? "on" : "off"}
      open={open}
      onToggle={onToggle}
      title={`${T.instrument} of this track - click to ${open ? "fold" : "show"}`}
    >
      {inst?.error && <div className="plg-error">{inst.error}</div>}
      <div className="dev-acts">
        {inst?.has_editor && (
          <button className={`btn s${inst.editor_open ? " on" : ""}`} onClick={() => void attempt(async () => setInstruments(await instrumentApi.editor(track.name, !inst.editor_open)))}>
            <Icon name="sliders" />
            {inst.editor_open ? T.closeEditor : T.openEditor}
          </button>
        )}
        {rack.supported && (
          <button className="btn s" onClick={(e) => setPicker(e.currentTarget)} title={T.instrumentPick}>
            {inst ? T.changeDevice : T.instrumentPick}
          </button>
        )}
        <button className="btn s" data-midi={trackAddress(track.index, "instrument.panic")} onClick={() => void attempt(() => instrumentApi.panic(track.name))} title={`${T.panic}: ends every note this track's instrument is playing`}>
          {T.panicShort}
        </button>
        <div className="grow" />
        {inst && (
          <button className="btn s danger" onClick={() => void attempt(async () => setInstruments(await instrumentApi.set(track.name, null)))} title="Take the instrument off this track">
            <Icon name="trash" />
            {T.removeDevice}
          </button>
        )}
      </div>
      {picker && <InstrumentPicker track={track.name} anchor={picker} onClose={() => setPicker(null)} />}
    </DeviceRow>
  );
}

// ---- plug-ins --------------------------------------------------------------------------------

function PluginRows({ owner, plugins, open, toggle }: { owner: Owner; plugins: SlotView[]; open: DeviceId | null; toggle: (id: DeviceId) => void }) {
  const { rack } = usePlugins();
  const [picker, setPicker] = useState<HTMLElement | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const [over, setOver] = useState<number | null>(null);

  if (!rack.supported) return <div className="dev-hint">{T.plugUnsupported}</div>;

  const free = (() => {
    for (let n = 0; n < rack.max_slots; n += 1) if (!plugins.some((s) => s.slot === n)) return n;
    return -1;
  })();

  return (
    <>
      {plugins.map((s) => (
        <PluginRow
          key={`${s.slot}-${s.plugin_id}`}
          s={s}
          open={open === pluginId(s.slot)}
          onToggle={() => toggle(pluginId(s.slot))}
          drag={{
            onDragStart: (e) => {
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData("text/plain", String(s.slot));
              setDrag(s.slot);
            },
            onDragOver: (e) => {
              if (drag === null) return;
              e.preventDefault();
              if (over !== s.slot) setOver(s.slot);
            },
            onDrop: (e) => {
              e.preventDefault();
              const from = drag;
              setDrag(null);
              setOver(null);
              if (from === null || from === s.slot) return;
              void attempt(async () => setRack(await pluginApi.move(owner, from, s.slot)));
            },
            onDragEnd: () => {
              setDrag(null);
              setOver(null);
            },
            over: over === s.slot && drag !== s.slot,
          }}
        />
      ))}
      <button
        className="dev-add"
        disabled={free < 0}
        onClick={(e) => setPicker(e.currentTarget)}
        title={free < 0 ? `All ${rack.max_slots} slots are taken` : "Load a plugin - after the built-in chain, before the fader"}
      >
        <Icon name="plus" />
        {T.addPlugin}
      </button>
      {picker && free >= 0 && (
        <PluginPicker target={owner} slot={free} anchor={picker} onClose={() => setPicker(null)} onLoaded={(slot) => openDevice(owner, pluginId(slot))} />
      )}
    </>
  );
}

function PluginRow({ s, open, onToggle, drag }: { s: SlotView; open: boolean; onToggle: () => void; drag: RowProps["drag"] }) {
  const on = !s.bypass;
  return (
    <DeviceRow
      id={pluginId(s.slot)}
      name={s.name}
      summary={<span>{pluginSummary(s)}</span>}
      power={{
        on,
        address: s.bypass_address,
        toggle: () => void attempt(async () => setRack(await pluginApi.bypass(s.target, s.slot, on))),
        title: on ? "Active - click to bypass" : "Bypassed - click to switch in",
      }}
      state={s.error ? "err" : on ? "on" : "off"}
      open={open}
      onToggle={onToggle}
      drag={drag}
      title={`${s.name} · ${s.vendor} · ${FORMAT[s.format] ?? s.format}${s.latency ? ` · ${s.latency} frames latency` : ""}\nClick to ${open ? "fold" : "show its parameters"} · drag to reorder`}
      trail={
        <span className="dev-grip" title="Drag to reorder the plug-ins" aria-hidden="true">
          <Icon name="grip" />
        </span>
      }
    >
      <PluginEditor s={s} />
    </DeviceRow>
  );
}

function PluginEditor({ s }: { s: SlotView }) {
  const run = async (work: () => Promise<unknown>) => attempt(work);
  return (
    <>
      {s.error && <div className="plg-error">{s.error}</div>}
      <div className="dev-acts">
        {s.has_editor && (
          <button className={`btn s${s.editor_open ? " on" : ""}`} onClick={() => void run(async () => setRack(await pluginApi.editor(s.target, s.slot, !s.editor_open)))} title="The plugin's own window">
            <Icon name="sliders" />
            {s.editor_open ? T.closeEditor : T.openEditor}
          </button>
        )}
        <span className="dim2 num">
          {FORMAT[s.format] ?? s.format}
          {s.active ? ` · ${s.cpu_us.toFixed(0)} µs` : ""}
        </span>
        <div className="grow" />
        <button className="btn s danger" onClick={() => void run(async () => setRack(await pluginApi.remove(s.target, s.slot)))} title="Remove this plugin">
          <Icon name="trash" />
          {T.removeDevice}
        </button>
      </div>
      {s.active ? <ParamList target={s.target} slot={s.slot} run={run} /> : <div className="dev-hint">The parameters show once the engine runs.</div>}
    </>
  );
}
