//! The plugin insert slots of one strip, compact as in the mockup: a loaded plugin is a row with
//! its bypass LED, its name and its format; an empty slot is "+ Plugin".
//!
//! * the LED switches bypass (MIDI-learnable: `….insert.<n>.bypass`),
//! * a click on the name opens the plugin's own window (or, without one, the slot manager with its
//!   parameter list), the × removes it,
//! * "+ Plugin" opens a searchable list of the scanned effects; a scan can be started from there,
//! * when more plugins are loaded than the strip has rows for, the last row says how many more and
//!   opens the slot manager (`plugins/PluginSlots`: reorder, parameters, everything).
//!
//! Built on the plugin host's own API and store (`plugins/api.ts`, `plugins/store.ts`).

import { useMemo, useRef, useState } from "react";
import { pluginApi } from "../plugins/api";
import { PluginSlots } from "../plugins/PluginSlots";
import { markScanning, setCatalog, setRack, usePlugins } from "../plugins/store";
import type { SlotView } from "../plugins/types";
import { fail } from "../shell/notes";
import { Icon } from "../shell/Icon";
import { Popover } from "./Popover";

/** `track.1`, `bus.main` - how the plugin host names a slot owner. */
export type InsertTarget = string;

const FORMAT: Record<string, string> = { vst3: "VST3", au: "AU" };

async function attempt(work: () => Promise<unknown>): Promise<void> {
  try {
    await work();
  } catch (e) {
    fail(e instanceof Error ? e.message : String(e));
  }
}

interface Props {
  target: InsertTarget;
  /** Rows the strip has room for. */
  rows: number;
}

export function InsertSlots({ target, rows }: Props) {
  const { rack } = usePlugins();
  const [picker, setPicker] = useState<{ el: HTMLElement; slot: number } | null>(null);
  const [manage, setManage] = useState<HTMLElement | null>(null);
  const slots = useMemo(
    () => rack.slots.filter((s) => s.target === target).sort((a, b) => a.slot - b.slot),
    [rack, target],
  );

  if (!rack.supported) {
    return (
      <>
        {Array.from({ length: rows }, (_, i) => (
          <div key={i} className="ins plug na" title="Plugins are available on macOS only for now." aria-disabled="true">
            <Icon name="plus" />
            Plugin
          </div>
        ))}
      </>
    );
  }

  const overflow = slots.length > rows;
  const shown = overflow ? slots.slice(0, rows - 1) : slots;
  const freeSlot = () => {
    for (let n = 0; n < rack.max_slots; n += 1) if (!slots.some((s) => s.slot === n)) return n;
    return -1;
  };
  const empties = overflow ? 0 : rows - shown.length;

  return (
    <>
      {shown.map((s) => (
        <Loaded key={`${s.slot}-${s.plugin_id}`} s={s} onManage={(el) => setManage(el)} />
      ))}
      {overflow && (
        <button className="ins plug more" onClick={(e) => setManage(e.currentTarget)} title="All plugins of this strip">
          <Icon name="plug" />+{slots.length - shown.length} more
        </button>
      )}
      {Array.from({ length: empties }, (_, i) => (
        <button
          key={`e${i}`}
          className="ins plug"
          disabled={freeSlot() < 0}
          onClick={(e) => setPicker({ el: e.currentTarget, slot: freeSlot() })}
          title="Load a plugin into this channel - after the built-in chain, before the fader"
        >
          <Icon name="plus" />
          Plugin
        </button>
      ))}
      {picker && <Picker target={target} slot={picker.slot} anchor={picker.el} onClose={() => setPicker(null)} onManage={() => {
        setManage(picker.el);
        setPicker(null);
      }} />}
      {manage && (
        <Popover anchor={manage} onClose={() => setManage(null)} className="plugmgr" width={300}>
          <PluginSlots target={target} title={target.startsWith("bus.") ? `Plugins · ${target === "bus.main" ? "Main" : "Monitor"}` : "Plugins"} />
        </Popover>
      )}
    </>
  );
}

function Loaded({ s, onManage }: { s: SlotView; onManage: (el: HTMLElement) => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const state = s.error ? "err" : !s.active ? "idle" : s.bypass ? "byp" : "";
  return (
    <div
      ref={ref}
      className={`ins plug full${state ? ` ${state}` : ""}`}
      title={
        s.error
          ? `${s.name}: ${s.error}`
          : `${s.name} · ${s.vendor}${s.latency ? ` · ${s.latency} frames latency` : ""}${s.active ? "" : " · starts with the engine"}\nClick: open · LED: bypass`
      }
    >
      <button
        className={`led${s.bypass ? "" : " lit"}`}
        data-midi={s.bypass_address}
        onClick={() => void attempt(async () => setRack(await pluginApi.bypass(s.target, s.slot, !s.bypass)))}
        aria-pressed={!s.bypass}
        title={s.bypass ? "Bypassed - click to switch in" : "Active - click to bypass"}
      />
      <button
        className="pname"
        onClick={() => {
          if (s.has_editor) void attempt(async () => setRack(await pluginApi.editor(s.target, s.slot, !s.editor_open)));
          else if (ref.current) onManage(ref.current);
        }}
      >
        {s.name}
      </button>
      <span className="v">{FORMAT[s.format] ?? s.format}</span>
      <button className="x" onClick={() => void attempt(async () => setRack(await pluginApi.remove(s.target, s.slot)))} title="Remove this plugin">
        <Icon name="x" />
      </button>
    </div>
  );
}

function Picker({
  target,
  slot,
  anchor,
  onClose,
  onManage,
}: {
  target: string;
  slot: number;
  anchor: HTMLElement;
  onClose: () => void;
  onManage: () => void;
}) {
  const { catalog, scanning } = usePlugins();
  const [query, setQuery] = useState("");
  const effects = useMemo(
    () => (catalog?.plugins ?? []).filter((p) => p.status === "ok" && p.kind === "effect"),
    [catalog],
  );
  const q = query.trim().toLowerCase();
  const found = q ? effects.filter((p) => `${p.name} ${p.vendor} ${p.category}`.toLowerCase().includes(q)) : effects;

  return (
    <Popover anchor={anchor} onClose={onClose} className="plugpick" width={280}>
      <div className="pp-h">
        <Icon name="search" size={12} />
        <input autoFocus placeholder="Search plugins" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <ul className="pp-list">
        {found.length === 0 && (
          <li className="hint">{catalog === null ? "Loading the plugin list…" : effects.length === 0 ? "No scanned effects yet." : "Nothing matches."}</li>
        )}
        {found.slice(0, 200).map((p) => (
          <li key={p.id}>
            <button
              onClick={() => {
                onClose();
                void attempt(async () => setRack(await pluginApi.load(target, p.id, slot)));
              }}
              title={`${p.category}${p.latency ? ` · ${p.latency} frames latency` : ""}`}
            >
              <span className="n">{p.name}</span>
              <span className="d">{p.vendor}</span>
              <span className="f">{FORMAT[p.format] ?? p.format}</span>
            </button>
          </li>
        ))}
      </ul>
      <div className="pp-f">
        <button
          className="btn s"
          disabled={scanning !== null}
          onClick={() =>
            void attempt(async () => {
              markScanning();
              setCatalog(await pluginApi.scan(false));
            })
          }
          title="Look for new or changed plugins (each is tried in a separate process)"
        >
          <Icon name="search" />
          {scanning !== null ? `Scanning ${scanning}…` : "Scan"}
        </button>
        <div className="grow" />
        <button className="btn s ghost" onClick={onManage}>
          Manage…
        </button>
      </div>
    </Popover>
  );
}
