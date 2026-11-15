//! The searchable list of scanned effects that "+ Add plugin" opens, with a scan button. Loads the
//! chosen plugin into the given slot of the given chain.

import { useMemo, useState } from "react";
import { pluginApi } from "../plugins/api";
import { markScanning, setCatalog, setRack, usePlugins } from "../plugins/store";
import { fail } from "../shell/notes";
import { Icon } from "../shell/Icon";
import { Popover } from "../mixer/Popover";

export const FORMAT: Record<string, string> = { vst3: "VST3", au: "AU" };

/** Run a plugin-host call; a refusal goes to the toasts. */
export async function attempt(work: () => Promise<unknown>): Promise<void> {
  try {
    await work();
  } catch (e) {
    fail(e instanceof Error ? e.message : String(e));
  }
}

export function PluginPicker({
  target,
  slot,
  anchor,
  onClose,
  onLoaded,
}: {
  target: string;
  slot: number;
  anchor: HTMLElement;
  onClose: () => void;
  /** Called with the slot once the plugin is in, so the column can open it. */
  onLoaded?: (slot: number) => void;
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
          <li className="hint">{catalog === null ? "Loading the plugin list…" : effects.length === 0 ? "No scanned effects yet - scan below." : "Nothing matches."}</li>
        )}
        {found.slice(0, 200).map((p) => (
          <li key={p.id}>
            <button
              onClick={() => {
                onClose();
                void attempt(async () => {
                  setRack(await pluginApi.load(target, p.id, slot));
                  onLoaded?.(slot);
                });
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
