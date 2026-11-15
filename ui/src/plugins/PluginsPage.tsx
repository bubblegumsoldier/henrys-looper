//! Setup → Plugins: what the scan found (and what it refused, with the reason), and the insert slots
//! of the two output buses. Track slots live in each track's drawer.

import { pluginApi } from "./api";
import { PluginSlots } from "./PluginSlots";
import { markScanning, setCatalog, setNotice, usePlugins } from "./store";
import type { ScannedPlugin } from "./types";

const STATUS_LABEL: Record<ScannedPlugin["status"], string> = {
  ok: "ready",
  failed: "failed",
  crashed: "crashed",
  timeout: "hung",
  instrument: "instrument",
};

export function PluginsPage() {
  const { catalog, scanning, notice, rack } = usePlugins();
  const parked = rack.parked ?? [];
  const plugins = catalog?.plugins ?? [];
  const ready = plugins.filter((p) => p.status === "ok").length;

  const scan = async (full: boolean) => {
    markScanning();
    try {
      setCatalog(await pluginApi.scan(full));
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <>
      <div>
        <h1>Plugins</h1>
        <p className="lead">
          VST3 and Audio Unit effects. Every plugin is tested in a separate process before it can be loaded, so a
          broken one cannot take the program down while scanning. Track plugins are in each track's drawer.
        </p>
      </div>
      {notice && (
        <section className="card">
          <div className="plg-error">{notice}</div>
        </section>
      )}
      <section className="card">
        <div className="plg-add" style={{ marginBottom: 8 }}>
          <div className="caps" style={{ flex: 1 }}>
            Found <span className="dim2">{ready} ready of {plugins.length}</span>
          </div>
          <button className="btn s" disabled={scanning !== null} onClick={() => void scan(false)}>
            {scanning !== null ? `Scanning${scanning ? `: ${scanning}` : "…"}` : "Scan"}
          </button>
          <button
            className="btn s ghost"
            disabled={scanning !== null}
            onClick={() => void scan(true)}
            title="Test every plugin again, also the ones that are already known"
          >
            Rescan all
          </button>
        </div>
        {plugins.length === 0 ? (
          <div className="hint">
            Nothing scanned yet. VST3 plugins are looked for in /Library/Audio/Plug-Ins/VST3 and
            ~/Library/Audio/Plug-Ins/VST3; Audio Units come from the system.
          </div>
        ) : (
          <div className="plg-params" style={{ maxHeight: 360 }}>
            {plugins.map((p) => (
              <div key={p.id} className="plg-param" title={p.message ?? p.path ?? p.id}>
                <span className="plg-pname">
                  {p.name} <span className="plg-fmt">{p.format === "vst3" ? "VST3" : "AU"}</span>
                </span>
                <span className="plg-pname">{p.vendor}</span>
                <span className="plg-pval" style={{ color: p.status === "ok" ? undefined : "#ff8a95" }}>
                  {STATUS_LABEL[p.status]}
                </span>
              </div>
            ))}
          </div>
        )}
        {catalog && <div className="hint">Scan results: {catalog.cache_path}</div>}
      </section>
      {parked.length > 0 && (
        <section className="card">
          <div className="caps" style={{ marginBottom: 8 }}>
            Kept for tracks not in this session
          </div>
          <div className="plg-params">
            {parked.map((p) => (
              <div key={`${p.track}:${p.slot}`} className="plg-param" title={p.plugin_id}>
                <span className="plg-pname">{p.name}</span>
                <span className="plg-pname">
                  {p.track} · slot {p.slot + 1}
                </span>
              </div>
            ))}
          </div>
          <div className="hint">
            Plugins belong to a track by its name. These come back as soon as a track of that name is in the engine
            again - from the setup or from a score.
          </div>
        </section>
      )}
      <section className="card">
        <PluginSlots target="bus.main" title="Main bus" />
      </section>
      <section className="card">
        <PluginSlots target="bus.monitor" title="Monitor bus" />
      </section>
    </>
  );
}
