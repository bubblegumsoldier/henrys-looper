//! Pieces of the instrument device (the first row of an instrument track's device column, see
//! `devices/DeviceChain`): the note light and the picker of playable instruments.
//!
//! The instrument belongs to the track's **name** (`crate::instruments` on the Rust side), so it
//! survives a restart and a score that orders the tracks differently.

import { useMemo, useRef, useState } from "react";
import { useStatusEffect } from "../status";
import { instrumentApi, setInstruments } from "../plugins/instruments";
import { setCatalog, markScanning, usePlugins } from "../plugins/store";
import { pluginApi } from "../plugins/api";
import { fail } from "../shell/notes";
import { Icon } from "../shell/Icon";
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

/** How long the note light stays on after a note. */
const NOTE_LED_MS = 150;

/** The light: on while keys are held, and for a moment after every note that arrives. */
export function NoteLed({ index }: { index: number }) {
  const el = useRef<HTMLSpanElement | null>(null);
  const last = useRef<number | null>(null);
  const litUntil = useRef(0);
  useStatusEffect((status: LooperStatus) => {
    const t = status.tracks[index];
    if (!t || !el.current) return;
    const notes = t.notes ?? 0;
    const now = performance.now();
    // A note keeps the light on for a moment, so fast notes read as "lit", not as flicker.
    if (last.current !== null && notes !== last.current) litUntil.current = now + NOTE_LED_MS;
    last.current = notes;
    el.current.classList.toggle("lit", now < litUntil.current || (t.held_notes ?? 0) > 0);
  });
  return <span ref={el} className="nled" title="Lights while notes reach this track" aria-hidden="true" />;
}

/** A searchable list of the playable instruments the scan found. */
/**
 * The list of playable instruments. Picking one puts it on `track` at once - or, with `onPick`,
 * only hands its id over (the arrangement writes it into the score).
 */
export function InstrumentPicker({
  track,
  anchor,
  onClose,
  onPick,
}: {
  track: string;
  anchor: HTMLElement;
  onClose: () => void;
  onPick?: (id: string) => void;
}) {
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
                ? "No playable instruments scanned yet. Scan to find Audio Unit and VST3 instruments."
                : "Nothing matches."}
          </li>
        )}
        {found.slice(0, 200).map((p) => (
          <li key={p.id}>
            <button
              onClick={() => {
                onClose();
                if (onPick) onPick(p.id);
                else void attempt(async () => setInstruments(await instrumentApi.set(track, p.id)));
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
