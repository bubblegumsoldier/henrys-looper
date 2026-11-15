//! The MIDI side of the window: the learn band, the sidebar while learning, and the building
//! blocks the Setup page "MIDI" uses too (`design/midi-learn.html`, `design/midi-missing.html`).
//!
//! ```text
//!  learnbar   MIDI Learn on. Click a marked control …         [FS-4 missing · 3 inactive] [Done]
//!  ┌ view (Live / Arrangement) ───────────────────┐┌ MIDI Mapping ───────── [By area ▾] ┐
//!  │ every [data-midi] marked, bound ones badged   ││ devices (connected / missing …)     │
//!  │                                               ││ waiting card (target, Esc)          │
//!  │                                               ││ bindings, grouped, click = details  │
//!  │                                               ││ input monitor                       │
//!  └───────────────────────────────────────────────┘└────────────────────────────────────┘
//! ```
//!
//! Everything shown comes from the view the backend sends (`types.ts`) - which devices, which
//! bindings, whether each works and why not. This file decides nothing about MIDI; it only lays the
//! data out. The one piece of state that is shared with the controls is the *focus* (`store.ts`):
//! hovering a row outlines its control, hovering a control lights up its row.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { midiApi } from "../api";
import { confirmAction } from "../dialogs";
import { Icon } from "../shell/Icon";
import {
  BINDING_STATE_WORD,
  DEVICE_STATE_WORD,
  M,
  OUTCOME_WORD,
  actionLabel,
  areaOf,
  clock,
  lastSeen,
  plural,
  targetLabel,
  trackOf,
} from "./labels";
import {

  leaveLearnMode,
  sayMidi,
  setMidiFocus,
  setMidiView,
  useMidiEvents,
  useMidiFocus,
  useMidiView,
} from "./store";
import {
  bindingWorks,
  type MidiBindingOptions,
  type MidiBindingView,
  type MidiDeviceView,
  type MidiEventReport,
  type MidiView,
} from "./types";
import "./midi.css";

export type Grouping = "area" | "device";

const GROUPING_KEY = "looper.midi.grouping";

function storedGrouping(): Grouping {
  try {
    return localStorage.getItem(GROUPING_KEY) === "device" ? "device" : "area";
  } catch {
    return "area";
  }
}

/** The list's grouping, remembered across restarts. */
export function useGrouping(): [Grouping, (g: Grouping) => void] {
  const [grouping, setGrouping] = useState<Grouping>(storedGrouping);
  const choose = useCallback((g: Grouping) => {
    setGrouping(g);
    try {
      localStorage.setItem(GROUPING_KEY, g);
    } catch {
      /* a preference, not worth an error */
    }
  }, []);
  return [grouping, choose];
}

/** Run a MIDI command; its answer is the new view, a refusal lands in the message line. */
function useRun(): [boolean, (work: () => Promise<MidiView>) => void] {
  const [busy, setBusy] = useState(false);
  const run = useCallback((work: () => Promise<MidiView>) => {
    setBusy(true);
    work()
      .then(setMidiView)
      .catch((e) => sayMidi((e as Error).message ?? String(e)))
      .finally(() => setBusy(false));
  }, []);
  return [busy, run];
}

const rescan = () => midiApi.ports().then(() => midiApi.state());

/** The element a binding belongs to, if it is on screen. */
function controlOf(address: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-midi="${CSS.escape(address)}"]`);
}

function isBroken(d: MidiDeviceView): boolean {
  return d.state !== "connected";
}

// =============================================================================================
// Learn band
// =============================================================================================

export function LearnBar({ mode }: { mode: "live" | "arrangement" | "setup" }) {
  const view = useMidiView();
  const missing = view.devices.filter((d) => d.state === "missing" && d.bindings > 0);
  return (
    <div className="learnbar" role="status">
      <Icon name="target" />
      <b>{M.learnOn}</b>
      <span className="learnbar-hint">{mode === "arrangement" ? M.learnHintArrangement : M.learnHintLive}</span>
      <div className="grow" />
      {missing.length > 0 ? (
        missing.map((d) => (
          <span key={d.path} className="pill warn" style={{ marginRight: 8 }}>
            <i className="led" />
            {d.name} missing · {d.inactive} inactive
          </span>
        ))
      ) : (
        <span className="dim learnbar-keep">{M.keepsPlaying}</span>
      )}
      <button className="btn s" style={{ marginLeft: 8 }} onClick={leaveLearnMode}>
        {M.done}
      </button>
    </div>
  );
}

// =============================================================================================
// Sidebar
// =============================================================================================

export function MidiSide() {
  const view = useMidiView();
  const [grouping, setGrouping] = useGrouping();
  const scroller = useRef<HTMLDivElement | null>(null);
  // A target was just armed: the waiting card near the top is what has to be in sight.
  useEffect(() => {
    if (view.learning) scroller.current?.scrollTo({ top: 0 });
  }, [view.learning]);

  return (
    <aside className="side midi-side" aria-label={M.title}>
      <div className="side-h">
        <Icon name="midi" size={18} className="learn-ic" />
        <h3>{M.title}</h3>
        <div className="grow" />
        <button
          className="btn s"
          onClick={() => setGrouping(grouping === "area" ? "device" : "area")}
          title="Group the bindings by area or by device"
        >
          {grouping === "area" ? M.byArea : M.byDevice}
          <Icon name="chev-d" />
        </button>
      </div>
      <div className="side-scroll" ref={scroller}>
        <DeviceStatus view={view} />
        {view.learning ? <WaitCard view={view} /> : <Message view={view} />}
        <BindingList view={view} grouping={grouping} />
      </div>
      <InputMonitor view={view} lines={3} />
    </aside>
  );
}

/** The backend's last sentence (learned, refused, moved …), for a few seconds. */
function Message({ view, sticky = false }: { view: MidiView; sticky?: boolean }) {
  const [shown, setShown] = useState<string | null>(null);
  useEffect(() => {
    if (!view.message) return;
    setShown(view.message);
    if (sticky) return;
    const t = window.setTimeout(() => setShown(null), 7000);
    return () => window.clearTimeout(t);
  }, [view.message, sticky]);
  const [busy, run] = useRun();
  return (
    <>
      {view.save_error && (
        <div className="midi-msg err" title={view.save_error}>
          <Icon name="warn" />
          <span>
            <b>{M.notSaved}:</b> {view.save_error}
          </span>
          <button className="btn s" disabled={busy} onClick={() => run(() => midiApi.save().then((s) => s.view))}>
            {M.retry}
          </button>
        </div>
      )}
      {shown && (
        <div className="midi-msg" aria-live="polite">
          <Icon name="info" />
          <span>{shown}</span>
        </div>
      )}
    </>
  );
}

// =============================================================================================
// Devices
// =============================================================================================

/**
 * One card per known controller, problems first. While every one of several devices works, the
 * sidebar folds them into one line ("2 devices connected") - the design's calm state; a click
 * unfolds it. `full` (the Setup page) never folds and always shows each device's actions.
 */
export function DeviceStatus({ view, full = false }: { view: MidiView; full?: boolean }) {
  const [unfolded, setUnfolded] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, run] = useRun();
  const devices = [...view.devices].sort((a, b) => Number(!isBroken(a)) - Number(!isBroken(b)));
  const newPorts = view.ports.filter((p) => !p.connected && p.profile === null);
  const missing = devices.filter((d) => d.state === "missing" && d.bindings > 0);
  const allFine = devices.length >= 2 && devices.every((d) => d.state === "connected");

  if (!full && allFine && !unfolded) {
    return (
      <>
        <button className="dev dev-sum" onClick={() => setUnfolded(true)} title="Show each device">
          <Icon name="pedal" />
          <div>
            <b>{devices.length} devices connected</b>
            <span>{devices.map((d) => d.name).join(" · ")}</span>
          </div>
          <span className="pill" style={{ height: 20 }}>
            <i className="led" />
            {M.active}
          </span>
        </button>
        {newPorts.map((p) => (
          <NewPort key={p.name} name={p.name} busy={busy} run={run} />
        ))}
      </>
    );
  }

  return (
    <>
      {devices.map((d) => (
        <DeviceCard
          key={d.path}
          device={d}
          view={view}
          open={full || open === d.path || (d.state === "missing" && d.bindings > 0)}
          full={full}
          onToggle={() => setOpen(open === d.path ? null : d.path)}
          busy={busy}
          run={run}
        />
      ))}
      {newPorts.map((p) => (
        <NewPort key={p.name} name={p.name} busy={busy} run={run} />
      ))}
      {devices.length === 0 && newPorts.length === 0 && (
        <div className="hint midi-pad">
          {M.noDeviceYet}{" "}
          <button className="btn s" disabled={busy} onClick={() => run(rescan)}>
            <Icon name="search" />
            {M.scanAgain}
          </button>
        </div>
      )}
      {!full && missing.length > 0 && (
        <div className="hint midi-pad">
          The bindings stay saved and work again the moment {missing.map((d) => d.name).join(" and ")}{" "}
          {missing.length === 1 ? "is" : "are"} plugged back in. Until then: <b>Release with the space bar</b> or
          touch.
        </div>
      )}
    </>
  );
}

function deviceLine(d: MidiDeviceView): string {
  const seen = lastSeen(d.last_seen);
  switch (d.state) {
    case "connected":
      return d.inactive > 0
        ? `connected · ${d.active} of ${d.bindings} bindings active`
        : `connected · ${plural(d.bindings, "binding")} active`;
    case "missing":
      return d.bindings === 0
        ? ["not plugged in · no bindings", seen].filter(Boolean).join(" · ")
        : [`${plural(d.bindings, "binding")} inactive`, seen].filter(Boolean).join(" · ");
    case "disconnected":
      return `disconnected by hand · ${plural(d.bindings, "binding")} inactive`;
    case "unavailable":
      return `plugged in, but another program holds it · ${plural(d.bindings, "binding")} inactive`;
    case "error":
      return d.error ?? "the profile file has errors";
  }
}

function DeviceCard({
  device: d,
  view,
  open,
  full,
  onToggle,
  busy,
  run,
}: {
  device: MidiDeviceView;
  view: MidiView;
  open: boolean;
  full: boolean;
  onToggle: () => void;
  busy: boolean;
  run: (work: () => Promise<MidiView>) => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [moving, setMoving] = useState(false);
  const port = view.ports.find((p) => p.profile === d.device && !p.connected);
  const others = view.devices.filter((o) => o.device !== d.device && o.state !== "error");
  // A missing device only matters while something waits for it.
  const alarming = (d.state === "missing" && d.bindings > 0) || d.state === "error";
  const title =
    d.state === "missing" && alarming
      ? `Device missing: ${d.name}`
      : d.state === "error"
        ? `Profile error: ${d.name}`
        : d.name;

  const remove = async () => {
    const ok = await confirmAction(
      `Forget "${d.name}" and its ${plural(d.bindings, "binding")}? It is no longer connected by itself. The file is kept as ${d.path.split(/[\\/]/).pop()}.deleted.`,
      "Delete MIDI device",
      M.delete,
    );
    if (ok) run(() => midiApi.profile({ action: "delete", device: d.device }));
  };

  return (
    <div className={`dev-wrap${open ? " open" : ""}`}>
      <div
        className={`dev dev-${d.state}${alarming ? " missing" : ""}`}
        onClick={full ? undefined : onToggle}
        role={full ? undefined : "button"}
        title={d.path}
      >
        <Icon name={alarming ? "warn" : "pedal"} />
        <div>
          {renaming ? (
            <RenameField
              device={d}
              onDone={(name) => {
                setRenaming(false);
                if (name !== null) run(() => midiApi.profile({ action: "rename", device: d.device, name }));
              }}
            />
          ) : (
            <b>{title}</b>
          )}
          <span>{deviceLine(d)}</span>
        </div>
        {d.state === "connected" ? (
          <span className="pill" style={{ height: 20 }}>
            <i className="led" />
            {M.active}
          </span>
        ) : d.state === "missing" ? (
          <span />
        ) : (
          <span className="pill warn" style={{ height: 20 }}>
            {DEVICE_STATE_WORD[d.state]}
          </span>
        )}
      </div>
      {open && !renaming && (
        <div className="dev-acts" onClick={(e) => e.stopPropagation()}>
          {d.state === "missing" && (
            <button className="btn s" disabled={busy} onClick={() => run(rescan)}>
              <Icon name="search" />
              {M.scanAgain}
            </button>
          )}
          {d.connected && d.port && (
            <button className="btn s" disabled={busy} onClick={() => run(() => midiApi.close(d.port!))}>
              {M.disconnect}
            </button>
          )}
          {!d.connected && port && (
            <button className="btn s" disabled={busy} onClick={() => run(() => midiApi.open(port.name))}>
              {M.connect}
            </button>
          )}
          {d.bindings > 0 && others.length > 0 && d.state !== "error" && (
            <button className={`btn s${moving ? " on" : ""}`} disabled={busy} onClick={() => setMoving((m) => !m)}>
              {M.moveTo}
            </button>
          )}
          {(full || !alarming) && (
            <>
              <button className="btn s ghost" disabled={busy} onClick={() => setRenaming(true)}>
                {M.rename}
              </button>
              <button className="btn s ghost danger" disabled={busy} onClick={() => void remove()}>
                {M.delete}
              </button>
            </>
          )}
        </div>
      )}
      {open && moving && (
        <MovePicker
          from={d}
          others={others}
          busy={busy}
          onMove={(to) => {
            setMoving(false);
            run(() => midiApi.profile({ action: "move", device: d.device, to }));
          }}
          onCancel={() => setMoving(false)}
        />
      )}
    </div>
  );
}

function RenameField({ device, onDone }: { device: MidiDeviceView; onDone: (name: string | null) => void }) {
  const [text, setText] = useState(device.name);
  const ref = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  return (
    <input
      ref={ref}
      className="field midi-rename"
      value={text}
      placeholder={device.device}
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => onDone(text === device.name ? null : text)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        if (e.key === "Escape") onDone(null);
      }}
      title="Empty = the device's own name"
    />
  );
}

function MovePicker({
  from,
  others,
  busy,
  onMove,
  onCancel,
}: {
  from: MidiDeviceView;
  others: MidiDeviceView[];
  busy: boolean;
  onMove: (to: string) => void;
  onCancel: () => void;
}) {
  const preferred = others.find((o) => o.connected) ?? others[0];
  const [to, setTo] = useState(preferred?.device ?? "");
  return (
    <div className="dev-move">
      <span className="hint">
        Move {plural(from.bindings, "binding")} of <b>{from.name}</b> to
      </span>
      <select className="field" value={to} onChange={(e) => setTo(e.target.value)} disabled={busy}>
        {others.map((o) => (
          <option key={o.device} value={o.device}>
            {o.name} ({DEVICE_STATE_WORD[o.state]})
          </option>
        ))}
      </select>
      <button className="btn s accent" disabled={busy || !to} onClick={() => onMove(to)}>
        {M.move}
      </button>
      <button className="btn s ghost" onClick={onCancel}>
        {M.cancel}
      </button>
      <span className="hint dev-move-note">
        Controls the other device already uses for something else stay where they are.
      </span>
    </div>
  );
}

function NewPort({
  name,
  busy,
  run,
}: {
  name: string;
  busy: boolean;
  run: (work: () => Promise<MidiView>) => void;
}) {
  return (
    <div className="dev dev-new">
      <Icon name="plug" />
      <div>
        <b>
          {M.newInput}: {name}
        </b>
        <span>{M.newInputHint}</span>
      </div>
      <button className="btn s" disabled={busy} onClick={() => run(() => midiApi.open(name))}>
        {M.connect}
      </button>
    </div>
  );
}

// =============================================================================================
// Waiting for the controller
// =============================================================================================

export function WaitCard({ view }: { view: MidiView }) {
  const [busy, run] = useRun();
  const address = view.learning!;
  const bound = view.bindings.filter((b) => !b.from_score && (b.resolved_address ?? b.address) === address);
  const label = targetLabel(address, view.session.tracks, view.learning_label ?? address);
  const unbindAll = () =>
    run(async () => {
      let last: MidiView | null = null;
      for (const b of bound) last = await midiApi.unbind(b.id, b.device);
      return midiApi.learn(null).then((v) => v ?? last);
    });
  return (
    <div className="waitcard">
      <span className="caps">{M.targetChosen}</span>
      <div className="tgt">{label}</div>
      <div className="addr">{address}</div>
      <div className="wait">
        <span className="spinner" />
        <div>
          <b>{M.waitingForController}</b>
          <br />
          <span className="dim" style={{ fontSize: 11 }}>
            {view.connected ? M.waitingHint : "No controller is connected - connect one, then press a control."}
          </span>
        </div>
      </div>
      <div className="acts">
        <button className="btn s" disabled={busy} onClick={() => run(() => midiApi.learn(null))}>
          <Icon name="x" />
          {M.cancel}
        </button>
        {bound.length > 0 && (
          <button className="btn s ghost" disabled={busy} onClick={unbindAll}>
            {M.removeBinding}
          </button>
        )}
        <div className="grow" />
        <span className="dim" style={{ fontSize: 11, alignSelf: "center" }}>
          Esc
        </span>
      </div>
    </div>
  );
}

// =============================================================================================
// Bindings
// =============================================================================================

const PART_ORDER = ["record", "overdub", "play", "stop", "clear", "monitor", "pan", "bus", "monitor_bus", "latency_trim", "layer", "fx"];

function sortKey(address: string): string {
  const m = /^track\.(\d+)\.([a-z_]+)/.exec(address);
  if (m) {
    const part = PART_ORDER.indexOf(m[2]);
    return `${m[1].padStart(3, "0")}.${String(part < 0 ? 99 : part).padStart(2, "0")}.${address}`;
  }
  const order = ["transport.next", "transport.start", "transport.stop_all"].indexOf(address);
  return `${order < 0 ? 9 : order}.${address}`;
}

interface Row {
  key: string;
  address: string;
  binding: MidiBindingView | null;
}

interface Group {
  key: string;
  title: string;
  rows: Row[];
}

function groupsOf(view: MidiView, grouping: Grouping): Group[] {
  const tracks = view.session.tracks;
  const groups = new Map<string, Group>();
  const add = (key: string, title: string, row: Row) => {
    const g = groups.get(key) ?? { key, title, rows: [] };
    g.rows.push(row);
    groups.set(key, g);
  };
  const place = (address: string, b: MidiBindingView | null, row: Row) => {
    if (b?.from_score) return add("9score", M.score, row);
    if (grouping === "device") {
      const device = view.devices.find((d) => d.device === b?.device);
      if (!b) return add("0pending", M.targetChosen, row);
      return add(`1${device?.name ?? b.device}`, device?.name ?? b.device ?? "?", row);
    }
    const area = areaOf(address, tracks);
    add(area.key, area.title, row);
  };
  for (const b of view.bindings) {
    const address = b.resolved_address ?? b.address;
    place(address, b, { key: `${b.device ?? "score"}:${b.id}`, address, binding: b });
  }
  const pending = view.learning;
  if (pending && !view.bindings.some((b) => (b.resolved_address ?? b.address) === pending)) {
    place(pending, null, { key: "pending", address: pending, binding: null });
  }
  for (const g of groups.values()) g.rows.sort((a, b) => sortKey(a.address).localeCompare(sortKey(b.address)));
  return [...groups.values()].sort((a, b) => a.key.localeCompare(b.key));
}

export function BindingList({ view, grouping }: { view: MidiView; grouping: Grouping }) {
  const focus = useMidiFocus();
  const [open, setOpen] = useState<string | null>(null);
  const groups = useMemo(() => groupsOf(view, grouping), [view, grouping]);
  const listRef = useRef<HTMLDivElement | null>(null);

  // A control hovered in the view: its row comes into sight. (Not when one is armed - then the
  // waiting card at the top is what has to be seen.)
  useEffect(() => {
    if (!focus || !listRef.current) return;
    const row = listRef.current.querySelector<HTMLElement>(`[data-row-address="${CSS.escape(focus)}"]`);
    row?.scrollIntoView({ block: "nearest" });
  }, [focus]);

  if (groups.length === 0) {
    return <div className="hint midi-pad">{M.nothingMapped}</div>;
  }
  return (
    <div className="blist" ref={listRef}>
      {groups.map((g) => (
        <div key={g.key} className="bgroup">
          <div className="bgrp caps">{g.title}</div>
          {g.rows.map((row) => (
            <BindingRow
              key={row.key}
              row={row}
              view={view}
              grouping={grouping}
              cur={view.learning === row.address}
              lit={focus === row.address}
              open={open === row.key}
              onOpen={() => setOpen(open === row.key ? null : row.key)}
            />
          ))}
        </div>
      ))}
    </div>
  );
}

function BindingRow({
  row,
  view,
  grouping,
  cur,
  lit,
  open,
  onOpen,
}: {
  row: Row;
  view: MidiView;
  grouping: Grouping;
  cur: boolean;
  lit: boolean;
  open: boolean;
  onOpen: () => void;
}) {
  const [busy, run] = useRun();
  const b = row.binding;
  const tracks = view.session.tracks;
  const fallback = b?.label ?? view.learning_label ?? row.address;
  // Under a track's own heading the track name is said already.
  const inTrackGroup = grouping === "area" && !b?.from_score && trackOf(row.address) !== null;
  const label = inTrackGroup ? actionLabel(row.address, fallback) : targetLabel(row.address, tracks, fallback);
  const works = b ? bindingWorks(b.state) : true;
  const word = b ? BINDING_STATE_WORD[b.state] : null;
  // "device missing" is said by the device card already; the others need saying here.
  const showWord = word && b?.state !== "device_missing";

  const show = () => {
    const el = controlOf(row.address);
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };

  return (
    <>
      <div
        className={`brow${works ? "" : " off"}${cur ? " cur" : ""}${lit ? " lit" : ""}${open ? " open" : ""}`}
        data-row-address={row.address}
        onMouseEnter={() => setMidiFocus(row.address)}
        onMouseLeave={() => setMidiFocus(null)}
        onClick={() => {
          show();
          if (b) onOpen();
        }}
        title={b?.reason ?? b?.describe ?? undefined}
      >
        <span className="t">
          {label}
          <small>{row.address}</small>
          {showWord && <em className={`bst bst-${b!.state}`}>{word}</em>}
        </span>
        {cur ? (
          <span className="dim2" style={{ fontSize: 11 }}>
            {M.waiting}
          </span>
        ) : b ? (
          <span className={`mb${works ? "" : " inactive"}${b.from_score ? " score" : ""}`}>{b.short}</span>
        ) : (
          <span />
        )}
        {b && !b.from_score ? (
          <button
            className="bx"
            disabled={busy}
            title="Remove binding"
            onClick={(e) => {
              e.stopPropagation();
              run(() => midiApi.unbind(b.id, b.device));
            }}
          >
            <Icon name="x" />
          </button>
        ) : (
          <span />
        )}
      </div>
      {open && b && <BindingDetail binding={b} view={view} busy={busy} run={run} />}
    </>
  );
}

/** What a row folds out into: why it does (not) work, where it lives, and its options. */
function BindingDetail({
  binding: b,
  view,
  busy,
  run,
}: {
  binding: MidiBindingView;
  view: MidiView;
  busy: boolean;
  run: (work: () => Promise<MidiView>) => void;
}) {
  const current: MidiBindingOptions = { mode: b.mode, takeover: b.takeover, min: b.min, max: b.max };
  const edit = (o: Partial<MidiBindingOptions>) => run(() => midiApi.edit(b.id, b.device, { ...current, ...o }));
  const device = view.devices.find((d) => d.device === b.device);
  return (
    <div className="bdet" onClick={(e) => e.stopPropagation()}>
      <div className="bdet-src">
        {b.from_score ? (
          <>From the score's <code>midi:</code> block - it goes when the score goes.</>
        ) : (
          <>
            <b>{b.short}</b> on {device?.name ?? b.device} · <code>{b.id}</code>
          </>
        )}
      </div>
      {b.reason && (
        <div className={`bdet-reason${bindingWorks(b.state) ? "" : " bad"}`}>
          <Icon name={bindingWorks(b.state) ? "info" : "warn"} />
          {b.reason}
        </div>
      )}
      {!b.from_score &&
        (b.control === "range" ? (
          <div className="bdet-opts">
            <label>Takeover</label>
            <select
              className="field"
              value={b.takeover ?? "pickup"}
              disabled={busy}
              onChange={(e) => edit({ takeover: e.target.value })}
              title="pickup: the knob acts once it passes the current value · jump: acts at once"
            >
              <option value="pickup">pickup</option>
              <option value="jump">jump</option>
            </select>
            <span className="bdet-range">
              <label>Range</label>
              <NumberField value={b.min} placeholder={b.range_min} disabled={busy} onCommit={(min) => edit({ min })} />
              <span className="dim">–</span>
              <NumberField value={b.max} placeholder={b.range_max} disabled={busy} onCommit={(max) => edit({ max })} />
            </span>
          </div>
        ) : (
          <div className="bdet-opts">
            <label>Mode</label>
            <select
              className="field"
              value={b.mode ?? ""}
              disabled={busy}
              onChange={(e) => edit({ mode: e.target.value })}
              title="press: on press · release: on release · toggle: each press switches · momentary: on while held"
            >
              {b.modes.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </div>
        ))}
    </div>
  );
}

/** A number sent when the field is left or Enter is pressed; empty means "the full range". */
function NumberField({
  value,
  placeholder,
  disabled,
  onCommit,
}: {
  value: number | null;
  placeholder: number | null;
  disabled: boolean;
  onCommit: (value: number | null) => void;
}) {
  const [text, setText] = useState(value === null ? "" : String(value));
  useEffect(() => setText(value === null ? "" : String(value)), [value]);
  const commit = () => {
    const next = text.trim() === "" ? null : Number(text.replace(",", "."));
    if (next !== null && !Number.isFinite(next)) {
      setText(value === null ? "" : String(value));
      return;
    }
    if (next !== value) onCommit(next);
  };
  return (
    <input
      className="field num-in bdet-num"
      inputMode="decimal"
      value={text}
      placeholder={placeholder === null ? "" : String(placeholder)}
      disabled={disabled}
      title="Empty = the target's full range"
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
    />
  );
}

// =============================================================================================
// Input monitor
// =============================================================================================

function eventText(e: MidiEventReport): string {
  switch (e.what) {
    case "Note on":
      return `Note ${e.number} on · v ${e.value}`;
    case "Note off":
      return `Note ${e.number} off`;
    case "CC":
      return `CC ${e.number} = ${e.value}`;
    default:
      return `Bend ${e.value}`;
  }
}

function resultText(e: MidiEventReport, tracks: string[]): string {
  const target = e.target ? targetLabel(e.target, tracks, e.target_label ?? e.target) : null;
  switch (e.outcome) {
    case "action":
      return target ? `→ ${target}` : OUTCOME_WORD.action;
    case "learned":
      return target ? `learned: ${target}` : OUTCOME_WORD.learned;
    case "unbound":
      return OUTCOME_WORD.unbound;
    default:
      return target ? `${OUTCOME_WORD[e.outcome]}: ${target}` : OUTCOME_WORD[e.outcome];
  }
}

/** The last few events with what came of them. `lines` is how many; the Setup page shows more. */
export function InputMonitor({ view, lines, card = false }: { view: MidiView; lines: number; card?: boolean }) {
  const events = useMidiEvents();
  // The sidebar reads like the design: the last few, oldest on top, without the note-offs that
  // only close a press. The Setup card shows everything, newest first.
  const shown = card
    ? events.slice(0, lines)
    : events
        .filter((e) => !(e.what === "Note off" && (e.outcome === "absorbed" || e.outcome === "unbound")))
        .slice(0, lines)
        .reverse();
  const nameOf = (e: MidiEventReport) =>
    view.devices.find((d) => d.device === e.device)?.name ?? e.device ?? e.port ?? "";
  return (
    <div className={`mon${card ? " mon-card" : ""}`}>
      {!card && (
        <div className="caps mon-h">
          <span>{M.inputMonitor}</span>
          <span className="dim2">{M.channels}</span>
        </div>
      )}
      {shown.length === 0 && <div className="mon-empty">{M.monitorEmpty}</div>}
      {shown.map((e) => (
        <div
          key={e.seq}
          className={`ln${e.outcome === "learned" ? " new" : ""} out-${e.outcome}`}
          title={e.note ?? undefined}
        >
          <span>{e.at ? clock(e.at, true) : ""}</span>
          <span className="mon-dev">{nameOf(e)}</span>
          <b>
            {eventText(e)}
            <i className="mon-res"> {resultText(e, view.session.tracks)}</i>
          </b>
        </div>
      ))}
    </div>
  );
}

export { Message as MidiMessage };
