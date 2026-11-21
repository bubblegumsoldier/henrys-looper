//! A channel's devices as one column, top to bottom in signal order - the same component in the
//! Live view's track detail and in the mixer's channel panel, for tracks and for buses:
//!
//! ```text
//!  DEVICES · voice      [Copy chain to…] [Preset  Voice ▾] [⏻ Chain on]
//!  ⏻ • IRDX Studio                        Two notes · 8 smp ⠿ ▸
//!  ⏻ High-pass                                  80 Hz        ▸
//!  ⏻ EQ                                         ~~~~         ▾
//!     [ graph with the three band handles, band knobs ]
//!  ⏻ Compressor                    −18 dB · 3:1  ▮▮▯         ▸
//!  ⏻ Delay                              1/8. · 22 %          ▸
//!  ⏻ Reverb                       62 % room · 18 %           ▸
//!  ⏻ • ValhallaRoom                     Valhalla DSP      ⠿ ▸
//!  + Add plugin
//! ```
//!
//! * The power button of a row switches that device; everything else of the row opens its editor
//!   right under it (an accordion - one open at a time, remembered per channel).
//! * A device that is off is dimmed, but its controls work: turning one switches it on (and the
//!   chain in) - see `TouchFxParam` on the Rust side.
//! * Built-in devices have the engine's fixed order; a plug-in row (marked with a dot) can be
//!   dragged above or below any row - in front of a built-in device, or after all of them. Its
//!   slot stays whenever it only crosses built-in devices, so its MIDI addresses stay too.
//! * "Copy to…" in an open plug-in or instrument row, "Copy chain to…" in the header: copies with
//!   the full plugin state, independent afterwards.
//! * Keyboard: ↑/↓ move between rows, Enter or Space opens and closes one, Alt+↑/↓ moves a plug-in
//!   one step (as do "↑ Up"/"↓ Down" in its open row) - the fallback for the drag.

import { useMemo, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
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
import { FX_NAME, fxOn, type FxActions } from "../mixer/fxModel";
import { curvePath, responseDb } from "../mixer/EqGraph";
import { InstrumentPicker, NoteLed } from "../mixer/InstrumentSlot";
import { Popover } from "../mixer/Popover";
import { CompEditor, CompReduction, DelayEditor, EqEditor, HighPassEditor, OctaverEditor, PresetPicker, ReverbEditor } from "./editors";
import { FORMAT, PluginPicker, attempt } from "./PluginPicker";
import {
  builtinSummary,
  busOwner,
  defaultDevice,
  deviceOrder,
  pluginId,
  pluginSlotOf,
  pluginSummary,
  placeDrop,
  dropIndex,
  stepPlace,
  trackOwner,
  type DeviceId,
  type Owner,
} from "./model";
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
  const order = deviceOrder({ instrument: !!track?.instrument, builtin: !!track, plugins });
  const stored = useOpenDevice(owner);
  const open = stored === undefined || (stored !== null && !order.includes(stored)) ? defaultDevice(order) : stored;
  const toggle = (id: DeviceId) => openDevice(owner, open === id ? null : id);
  const list = useRef<HTMLDivElement | null>(null);

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (e.altKey) {
      // Alt+↑/↓ on a plug-in row moves the plug-in one step - the keyboard twin of the drag.
      const id = (document.activeElement?.closest("[data-dev]") as HTMLElement | null)?.dataset.dev as DeviceId | undefined;
      const slot = pluginSlotOf(id ?? null);
      if (slot === null || !track) return;
      e.preventDefault();
      const where = stepPlace(order, slot, e.key === "ArrowUp" ? -1 : 1, rack.max_slots);
      if (!where) return;
      void attempt(async () => {
        const to = await placePlugin(owner, slot, where, open);
        // The row is drawn at its new place; the focus goes with it.
        setTimeout(() => list.current?.querySelector<HTMLElement>(`[data-dev="${pluginId(to)}"] .dev-main`)?.focus(), 0);
      });
      return;
    }
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
            <CopyChain track={track} />
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
        <ChainRows owner={owner} track={track} fx={fx} plugins={plugins} order={order} open={open} toggle={toggle} />
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
  /** Every row is a drop target while a plug-in is dragged; only plug-in rows can be dragged. */
  drag?: RowDrag;
  title?: string;
  /** A plug-in row (marked as such among the built-in devices). */
  plug?: boolean;
}

interface RowDrag {
  /** Plug-in rows only: pressing the header starts a drag (see `ChainRows`). */
  onPointerDown?: (e: ReactPointerEvent) => void;
  /** Swallows the click that follows the release of a drag. */
  onClickCapture: (e: MouseEvent) => void;
  /** The dragged plug-in would land in front of this row: the insertion line shows above it. */
  over: "above" | null;
}

function DeviceRow(p: RowProps) {
  return (
    <div
      className={`dvc ${p.state}${p.open ? " open" : ""}${p.plug ? " plug" : ""}${p.drag?.over ? ` drop-${p.drag.over}` : ""}`}
      role="listitem"
      data-dev={p.id}
      onClickCapture={p.drag?.onClickCapture}
    >
      <div className={`dev-h${p.drag?.onPointerDown ? " grab" : ""}`} onPointerDown={p.drag?.onPointerDown}>
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

function BuiltinRow({ slot, track, fx, open, onToggle, drag }: { slot: FxSlotName; track: TrackChain; fx: FxStatus; open: boolean; onToggle: () => void; drag?: RowDrag }) {
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
      drag={drag}
      title={`${FX_NAME[slot]} - click to ${open ? "fold" : "edit"}`}
    >
      {slot === "octaver" && <OctaverEditor {...editor} />}
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
  const copyTargets = useCopyTargets();
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
          <CopyTo
            label={T.copyTo}
            title={`Put ${inst.name} with all its settings on another instrument track, replacing what plays there. The copy is independent.`}
            targets={copyTargets
              .filter((t) => t.instrument && t.index !== null && t.index !== track.index)
              .map((t) => ({ key: t.name, name: t.name }))}
            onPick={async (to) => setInstruments(await instrumentApi.copy(track.name, to))}
          />
        )}
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

// ---- the rows: built-in devices and plug-ins in one list ----------------------------------------

interface ChainRowsProps {
  owner: Owner;
  track?: TrackChain;
  fx?: FxStatus;
  plugins: SlotView[];
  /** The column in signal order (`deviceOrder`). */
  order: DeviceId[];
  open: DeviceId | null;
  toggle: (id: DeviceId) => void;
}

/**
 * Move plug-in `from` of `owner` to `move` (from `placeDrop`/`stepPlace`). An unfolded plug-in
 * stays unfolded under its new slot. Answers the new slot.
 */
async function placePlugin(owner: Owner, from: number, move: { to: number; before: FxSlotName | null }, open: DeviceId | null): Promise<number> {
  setRack(await pluginApi.place(owner, from, move.to, move.before));
  if (open === pluginId(from) && move.to !== from) openDevice(owner, pluginId(move.to));
  return move.to;
}

/**
 * Every device of a column in signal order. A plug-in row can be dragged above or below any other
 * row - in front of a built-in device, between two of them, or after the chain; the built-in
 * devices keep their order. Dropping asks the host for the slot and the place `placeDrop` works out.
 *
 * **Pointer events, not HTML5 drag and drop.** In the app window (WKWebView) Tauri's own file-drop
 * handling (`dragDropEnabled`, on by default) takes the native drag session, and the page never
 * sees `dragover`/`drop` - the plug-in could be picked up but not dropped anywhere. Pointer events
 * are delivered the same in every web view, which is why every other drag in the app (grid, mixer
 * edge, sheet) uses them too. The rows are hit-tested by their header middles (`dropIndex`), so an
 * unfolded device's controls count as "behind it", and on/off or unfolded rows take a drop like
 * any other.
 */
function ChainRows({ owner, track, fx, plugins, order, open, toggle }: ChainRowsProps) {
  const { rack } = usePlugins();
  const [picker, setPicker] = useState<HTMLElement | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  /** Index into `order` the dragged plug-in would land in front of (`order.length`: the end). */
  const [at, setAt] = useState<number | null>(null);
  /** A drag just ended: the click that follows the release must not fold the row. */
  const dragged = useRef(false);

  const free = (() => {
    for (let n = 0; n < rack.max_slots; n += 1) if (!plugins.some((s) => s.slot === n)) return n;
    return -1;
  })();

  /** Pressing a plug-in row's header (not its power switch) and moving 4 px starts a drag. */
  const startDrag = (e: ReactPointerEvent, slot: number) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".pwr")) return;
    const list = (e.currentTarget as HTMLElement).closest(".devlist");
    if (!list) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    const columnOrder = order;
    let active = false;
    let target: number | null = null;
    const mids = () =>
      columnOrder.map((id) => {
        const head = list.querySelector(`[data-dev="${id}"] > .dev-h`);
        if (!head) return Number.POSITIVE_INFINITY;
        const box = head.getBoundingClientRect();
        return box.top + box.height / 2;
      });
    const move = (ev: PointerEvent) => {
      if (!active) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 4) return;
        active = true;
        dragged.current = true;
        setDrag(slot);
        document.body.classList.add("dev-dragging");
      }
      ev.preventDefault();
      target = dropIndex(ev.clientY, mids());
      setAt(target);
    };
    const end = (drop: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", escape, true);
      document.body.classList.remove("dev-dragging");
      setDrag(null);
      setAt(null);
      // The click that follows a release lands after this; let it pass by first.
      setTimeout(() => (dragged.current = false), 0);
      if (!active || !drop || target === null) return;
      const where = placeDrop(columnOrder, slot, target, rack.max_slots);
      if (where) void attempt(() => placePlugin(owner, slot, where, open));
    };
    const up = () => end(true);
    const cancel = () => end(false);
    const escape = (ev: globalThis.KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      ev.stopPropagation();
      end(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", escape, true);
  };

  const rowDrag = (index: number, slot: number | null): RowDrag => ({
    onPointerDown: slot === null || !rack.supported ? undefined : (e) => startDrag(e, slot),
    onClickCapture: (e) => {
      if (!dragged.current) return;
      e.stopPropagation();
      e.preventDefault();
    },
    over: drag !== null && at === index ? "above" : null,
  });

  /** One step up or down, for the buttons of an unfolded plug-in row; null at either end. */
  const step = (slot: number, dir: -1 | 1): (() => void) | null => {
    const where = stepPlace(order, slot, dir, rack.max_slots);
    return where ? () => void attempt(() => placePlugin(owner, slot, where, open)) : null;
  };

  return (
    <>
      {order.map((id, index) => {
        if (id === "instrument") {
          return track ? <InstrumentRow key={id} track={track} open={open === id} onToggle={() => toggle(id)} /> : null;
        }
        const slot = pluginSlotOf(id);
        if (slot === null) {
          if (!track || !fx) return null;
          const name = id as FxSlotName;
          return <BuiltinRow key={id} slot={name} track={track} fx={fx} open={open === id} onToggle={() => toggle(id)} drag={rowDrag(index, null)} />;
        }
        const s = plugins.find((p) => p.slot === slot);
        if (!s) return null;
        return (
          <PluginRow
            key={`${s.slot}-${s.plugin_id}`}
            s={s}
            open={open === id}
            onToggle={() => toggle(id)}
            drag={rowDrag(index, s.slot)}
            moves={track ? { up: step(s.slot, -1), down: step(s.slot, 1) } : undefined}
          />
        );
      })}
      {!rack.supported ? (
        <div className="dev-hint">{T.plugUnsupported}</div>
      ) : (
        <button
          className={`dev-add${drag !== null && at === order.length ? " drop-above" : ""}`}
          disabled={free < 0}
          onClick={(e) => setPicker(e.currentTarget)}
          title={
            free < 0
              ? `All ${rack.max_slots} slots are taken`
              : track
                ? "Load a plugin - after the built-in devices; drag it anywhere in the chain afterwards"
                : "Load a plugin into this bus"
          }
        >
          <Icon name="plus" />
          {T.addPlugin}
        </button>
      )}
      {picker && free >= 0 && (
        <PluginPicker target={owner} slot={free} anchor={picker} onClose={() => setPicker(null)} onLoaded={(slot) => openDevice(owner, pluginId(slot))} />
      )}
    </>
  );
}

/** The other tracks (and buses) of the session: where a copy can go. */
function useCopyTargets(): { owner: Owner; index: number | null; name: string; instrument: boolean }[] {
  const key = useStatusSlice((s) => s.tracks.map((t) => `${t.instrument ? 1 : 0}${t.name}`).join("\u0001"));
  return useMemo(() => {
    const tracks = key
      ? key.split("\u0001").map((entry, index) => ({
          owner: trackOwner(index),
          index,
          name: entry.slice(1),
          instrument: entry[0] === "1",
        }))
      : [];
    return [
      ...tracks,
      { owner: busOwner("main"), index: null, name: T.main, instrument: false },
      { owner: busOwner("monitor"), index: null, name: T.monitorBus, instrument: false },
    ];
  }, [key]);
}

/** A small "Copy to …" button with the list of places a copy can go. */
function CopyTo({ label, title, targets, onPick }: { label: string; title: string; targets: { key: string; name: string }[]; onPick: (key: string) => Promise<unknown> }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  return (
    <>
      <button className="btn s" disabled={targets.length === 0} onClick={(e) => setAnchor(anchor ? null : e.currentTarget)} aria-expanded={anchor !== null} title={title}>
        <Icon name="dup" />
        {label}
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} className="menu" width={Math.max(160, anchor.offsetWidth)}>
          {targets.map((t) => (
            <button
              key={t.key}
              role="menuitem"
              onClick={() => {
                setAnchor(null);
                void attempt(() => onPick(t.key));
              }}
            >
              {t.name}
            </button>
          ))}
        </Popover>
      )}
    </>
  );
}

/** "Copy chain to …" in a track's header: every built-in setting and every plug-in with its state. */
function CopyChain({ track }: { track: TrackChain }) {
  const targets = useCopyTargets().filter((t) => t.index !== null && t.index !== track.index);
  return (
    <CopyTo
      label={T.copyChain}
      title={`Copy the whole device chain of ${track.name} - every built-in setting and every plug-in with its settings - onto another track. Its plug-ins are replaced; fader, mute and instrument stay.`}
      targets={targets.map((t) => ({ key: String(t.index), name: t.name }))}
      onPick={async (to) => setRack(await pluginApi.copyChain(track.index, Number(to)))}
    />
  );
}

/** One step up / down through the places and the other plug-ins; null where it cannot go. */
interface Moves {
  up: (() => void) | null;
  down: (() => void) | null;
}

function PluginRow({ s, open, onToggle, drag, moves }: { s: SlotView; open: boolean; onToggle: () => void; drag: RowProps["drag"]; moves?: Moves }) {
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
      plug
      title={`${s.name} · ${s.vendor} · ${FORMAT[s.format] ?? s.format}${s.latency ? ` · ${s.latency} frames latency` : ""}\nClick to ${open ? "fold" : "show its parameters"} · drag to move it anywhere in the chain (Alt+↑/↓ moves it too)`}
      trail={
        <span className="dev-grip" title="Drag to move this plug-in - in front of a built-in device, between two, or after them" aria-hidden="true">
          <Icon name="grip" />
        </span>
      }
    >
      <PluginEditor s={s} moves={moves} />
    </DeviceRow>
  );
}

function PluginEditor({ s, moves }: { s: SlotView; moves?: Moves }) {
  const run = async (work: () => Promise<unknown>) => attempt(work);
  const targets = useCopyTargets().filter((t) => t.owner !== s.target);
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
        {moves && (
          <span className="seg dev-move">
            <button className="btn s" data-move="up" disabled={!moves.up} onClick={() => moves.up?.()} title={T.moveUpHint}>
              {T.moveUp}
            </button>
            <button className="btn s" data-move="down" disabled={!moves.down} onClick={() => moves.down?.()} title={T.moveDownHint}>
              {T.moveDown}
            </button>
          </span>
        )}
        <CopyTo
          label={T.copyTo}
          title={`Put a copy of ${s.name} - with all its settings, on/off and place in the chain - on another track or bus. The copy is independent.`}
          targets={targets.map((t) => ({ key: t.owner, name: t.name }))}
          onPick={async (to) => setRack(await pluginApi.copy(s.target, s.slot, to))}
        />
        <button className="btn s danger" onClick={() => void run(async () => setRack(await pluginApi.remove(s.target, s.slot)))} title="Remove this plugin">
          <Icon name="trash" />
          {T.removeDevice}
        </button>
      </div>
      {s.active ? <ParamList target={s.target} slot={s.slot} run={run} /> : <div className="dev-hint">The parameters show once the engine runs.</div>}
    </>
  );
}
