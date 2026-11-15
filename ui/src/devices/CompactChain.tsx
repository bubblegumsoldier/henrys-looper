//! The device column of a mixer strip - the same list as `DeviceChain`, in the same order, one
//! narrow row per device (as the inserts of a console strip):
//!
//! ```text
//!  DEVICES   Voice ▾  ⏻     preset, chain bypass
//!  ⏻ High-p.
//!  ⏻ EQ
//!  ⏻ Comp
//!  ○ Delay
//!  ⏻ Reverb
//!  ─────────
//!  ⏻ Valhalla…
//!  +                        add a plugin
//! ```
//!
//! The dot switches the device; a click on the name selects the channel and unfolds exactly that
//! device in the channel panel.

import { useMemo, useState } from "react";
import { fxAddress, fxSlotAddress } from "../midi/addresses";
import { pluginApi } from "../plugins/api";
import { instrumentOf, useInstruments } from "../plugins/instruments";
import { setRack, usePlugins } from "../plugins/store";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import { FX_NAME, FX_SHORT, FX_SLOT_ORDER, fxOn } from "../mixer/fxModel";
import { NoteLed } from "../mixer/InstrumentSlot";
import { PluginPicker, attempt } from "./PluginPicker";
import { pluginId, pluginSummary, type DeviceId, type Owner } from "./model";
import { openDevice } from "./store";
import type { TrackChain } from "./DeviceChain";

interface Props {
  owner: Owner;
  track?: TrackChain;
  /** Select this channel (or bus) for the panel. */
  onSelect: () => void;
  /** The preset menu of the strip's caption - the strip owns it. */
  caption?: React.ReactNode;
}

export function CompactChain({ owner, track, onSelect, caption }: Props) {
  const { rack } = usePlugins();
  const instruments = useInstruments();
  const plugins = useMemo(
    () => rack.slots.filter((s) => s.target === owner).sort((a, b) => a.slot - b.slot),
    [rack, owner],
  );
  const [picker, setPicker] = useState<HTMLElement | null>(null);
  const show = (id: DeviceId) => {
    openDevice(owner, id);
    onSelect();
  };
  const fx = track?.fx;
  const free = (() => {
    for (let n = 0; n < rack.max_slots; n += 1) if (!plugins.some((s) => s.slot === n)) return n;
    return -1;
  })();
  const inst = track?.instrument ? instrumentOf(instruments.rack, track.name) : undefined;
  const instLoading = !!track && (instruments.rack.loading?.includes(track.name) ?? false);

  return (
    <div className="cchain">
      <span className="caps">
        {T.devices}
        {caption}
        {track && fx && (
          <button
            className={`cpw${fx.bypass ? " out" : " on"}`}
            data-midi={fxAddress(track.index, "bypass")}
            onClick={() => track.actions.fxBypass(track.index, !fx.bypass)}
            aria-pressed={!fx.bypass}
            title={fx.bypass ? "Built-in chain bypassed - passes bit for bit. Click to switch it in." : "Built-in chain in. Click to bypass it."}
          >
            <Icon name="power" />
          </button>
        )}
      </span>
      <div className="crows">
        {track?.instrument && (
          <div className={`crow inst${inst ? " on" : ""}`}>
            <NoteLed index={track.index} />
            <button className="cn" onClick={() => show("instrument")} title={inst ? `${inst.name} · ${inst.vendor}` : T.instrumentPick}>
              {instLoading ? T.instrumentLoading : inst ? inst.name : `+ ${T.instrument}`}
            </button>
          </div>
        )}
        {track &&
          fx &&
          FX_SLOT_ORDER.map((slot) => {
            const on = fxOn(fx, slot);
            return (
              <div key={slot} className={`crow${on ? (fx.bypass ? " held" : " on") : ""}`}>
                <button
                  className="cled"
                  data-midi={fxSlotAddress(track.index, slot)}
                  onClick={() => track.actions.fxEnable(track.index, slot, !on)}
                  aria-pressed={on}
                  aria-label={`${FX_NAME[slot]} ${on ? T.fxOn : T.fxOff}`}
                  title={`${FX_NAME[slot]}: ${on ? "on" : "off"} - click to switch`}
                />
                <button className="cn" onClick={() => show(slot)} title={`${FX_NAME[slot]} - open in the channel panel`}>
                  {FX_SHORT[slot]}
                </button>
              </div>
            );
          })}
        {track && <i className="csep" aria-hidden="true" />}
        {!rack.supported ? (
          <div className="crow na" title={T.plugUnsupported}>
            <span className="cn">–</span>
          </div>
        ) : (
          plugins.map((s) => (
            <div key={`${s.slot}-${s.plugin_id}`} className={`crow plug${s.error ? " err" : !s.bypass ? " on" : ""}`}>
              <button
                className="cled"
                data-midi={s.bypass_address}
                onClick={() => void attempt(async () => setRack(await pluginApi.bypass(s.target, s.slot, !s.bypass)))}
                aria-pressed={!s.bypass}
                aria-label={`${s.name} ${s.bypass ? T.fxOff : T.fxOn}`}
                title={s.bypass ? "Bypassed - click to switch in" : "Active - click to bypass"}
              />
              <button className="cn" onClick={() => show(pluginId(s.slot))} title={`${s.name} · ${pluginSummary(s)} - open in the channel panel`}>
                {s.name}
              </button>
            </div>
          ))
        )}
        {rack.supported && (
          <button
            className="cadd"
            disabled={free < 0}
            onClick={(e) => setPicker(e.currentTarget)}
            title={free < 0 ? `All ${rack.max_slots} slots are taken` : T.addPlugin}
            aria-label={T.addPlugin}
          >
            <Icon name="plus" />
          </button>
        )}
      </div>
      {picker && free >= 0 && (
        <PluginPicker
          target={owner}
          slot={free}
          anchor={picker}
          onClose={() => setPicker(null)}
          onLoaded={(slot) => show(pluginId(slot))}
        />
      )}
    </div>
  );
}
