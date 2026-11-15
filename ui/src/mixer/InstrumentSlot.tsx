//! The instrument of an instrument track, at the top of its strip: what plays it, an activity
//! light for the notes that reach the track, and "all notes off".
//!
//! ```text
//!  INSTRUMENT ●        [!]     caption, activity light (lights while notes arrive or keys are held),
//!  ▸ DLSMusicDevice   AU       panic (MIDI-learnable: track.<n>.instrument.panic); the row opens
//!                              the instrument's own window, × takes it off, "+ Instrument" picks one
//! ```
//!
//! The instrument belongs to the track's **name** (`crate::instruments` on the Rust side), so it
//! survives a restart and a score that orders the tracks differently.

import { useMemo, useRef, useState } from "react";
import { useStatusEffect } from "../status";
import { trackAddress } from "../midi/addresses";
import { instrumentApi, instrumentOf, setInstruments, useInstruments } from "../plugins/instruments";
import { setCatalog, markScanning, usePlugins } from "../plugins/store";
import { pluginApi } from "../plugins/api";
import { fail } from "../shell/notes";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import type { LooperStatus } from "../types";
import { Popover } from "./Popover";

const FORMAT: Record<string, string> = { vst3: "VST3", au: "AU" };

async function attempt(work: () => Promise<unknown>): Promise<void> {
  try {
    await work();
  } catch (e) {
    fail(e instanceof Error ? e.message : String(e));
  }
}

/** The light: on while keys are held, and for a moment after every note that arrives. */
export function NoteLed({ index }: { index: number }) {
  const el = useRef<HTMLSpanElement | null>(null);
  const last = useRef<number | null>(null);
  useStatusEffect((status: LooperStatus) => {
    const t = status.tracks[index];
    if (!t || !el.current) return;
    const notes = t.notes ?? 0;
    const moved = last.current !== null && notes !== last.current;
    last.current = notes;
    el.current.classList.toggle("lit", moved || (t.held_notes ?? 0) > 0);
  });
  return <span ref={el} className="nled" title="Lights while notes reach this track" aria-hidden="true" />;
}

/** Caption row of the zone: the word, the light, the panic button. */
export function InstrumentCaps({ index, track }: { index: number; track: string }) {
  return (
    <span className="caps">
      {T.instrument}
      <NoteLed index={index} />
      <span className="grow" />
      <button
        className="panic"
        data-midi={trackAddress(index, "instrument.panic")}
        onClick={() => void attempt(() => instrumentApi.panic(track))}
        title={`${T.panic}: ends every note this track's instrument is playing`}
      >
        {T.panicShort}
      </button>
    </span>
  );
}

/** The slot row: the loaded instrument, or the button that picks one. */
export function InstrumentSlot({ track }: { track: string }) {
  const { rack } = useInstruments();
  const [picker, setPicker] = useState<HTMLElement | null>(null);
  const inst = instrumentOf(rack, track);

  if (!rack.supported) {
    return (
      <div className="ins plug na" title="Instruments are available on macOS only for now." aria-disabled="true">
        <Icon name="plus" />
        {T.instrumentAdd}
      </div>
    );
  }
  if (!inst) {
    return (
      <>
        <button className="ins plug inst" onClick={(e) => setPicker(e.currentTarget)} title={T.instrumentPick}>
          <Icon name="plus" />
          {T.instrumentAdd}
        </button>
        {picker && <InstrumentPicker track={track} anchor={picker} onClose={() => setPicker(null)} />}
      </>
    );
  }
  const state = inst.error ? "err" : !inst.active ? "idle" : "";
  return (
    <>
      <div
        className={`ins plug full inst${state ? ` ${state}` : ""}`}
        title={
          inst.error
            ? `${inst.name}: ${inst.error}`
            : `${inst.name} · ${inst.vendor}${inst.latency ? ` · ${inst.latency} frames latency` : ""}${inst.active ? "" : " · starts with the engine"}\nClick: open its window`
        }
      >
        <button className="pname" onClick={(e) => {
          if (inst.has_editor) void attempt(async () => setInstruments(await instrumentApi.editor(track, !inst.editor_open)));
          else setPicker(e.currentTarget);
        }}>
          {inst.name}
        </button>
        <span className="v">{FORMAT[inst.format] ?? inst.format}</span>
        <button className="x" onClick={() => void attempt(async () => setInstruments(await instrumentApi.set(track, null)))} title="Take the instrument off this track">
          <Icon name="x" />
        </button>
      </div>
      {picker && <InstrumentPicker track={track} anchor={picker} onClose={() => setPicker(null)} />}
    </>
  );
}

/** A searchable list of the playable instruments the scan found. */
export function InstrumentPicker({ track, anchor, onClose }: { track: string; anchor: HTMLElement; onClose: () => void }) {
  const { catalog, scanning } = usePlugins();
  const [query, setQuery] = useState("");
  const instruments = useMemo(
    () => (catalog?.plugins ?? []).filter((p) => p.status === "ok" && p.kind === "instrument"),
    [catalog],
  );
  const q = query.trim().toLowerCase();
  const found = q ? instruments.filter((p) => `${p.name} ${p.vendor}`.toLowerCase().includes(q)) : instruments;
  return (
    <Popover anchor={anchor} onClose={onClose} className="plugpick" width={280}>
      <div className="pp-h">
        <Icon name="search" size={12} />
        <input autoFocus placeholder="Search instruments" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <ul className="pp-list">
        {found.length === 0 && (
          <li className="hint">
            {catalog === null
              ? "Loading the plugin list…"
              : instruments.length === 0
                ? "No playable instruments scanned yet. Audio Unit instruments can be played; VST3 instruments not yet."
                : "Nothing matches."}
          </li>
        )}
        {found.slice(0, 200).map((p) => (
          <li key={p.id}>
            <button
              onClick={() => {
                onClose();
                void attempt(async () => setInstruments(await instrumentApi.set(track, p.id)));
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
      </div>
    </Popover>
  );
}
