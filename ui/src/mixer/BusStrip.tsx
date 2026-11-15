//! The two output buses as strips: Main (the room) and Monitor (the headphones). Inserts, where the
//! bus leaves the machine, the click (which lives on Monitor only), the bus volume and what
//! actually leaves - measured after the volume, i.e. the real output.

import { memo, useState } from "react";
import { GLOBAL, busGainAddress } from "../midi/addresses";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import type { BusId, LooperStatus } from "../types";
import { busActions } from "./actions";
import { DragNumber, Fader, linear } from "./controls";
import { InsertSlots } from "./InsertSlots";
import { Popover } from "./Popover";
import { PeakHold, VMeter } from "./meters";
import { MAX_BUS_GAIN, faderText, gainToDb } from "./scale";
import { BUS_FADER_CURVE, Scale } from "./Strip";

export interface BusView {
  id: BusId;
  channel: number;
  width: number;
  channelLabel: string;
  gain: number;
  hasClick: boolean;
}

export interface BusGlobals {
  click: boolean;
  clickGain: number;
  outputChannels: number;
  collapsed: boolean;
}

const CLICK_CURVE = linear(0, 4);
const clickText = (g: number) => {
  const db = gainToDb(g);
  return Number.isFinite(db) ? `${db > 0.05 ? "+" : db < -0.05 ? "−" : ""}${Math.abs(db).toFixed(0)} dB` : "−∞ dB";
};

/** Every way a bus can leave a device with `n` outputs: stereo pairs first, then single channels. */
function outputChoices(n: number): { channel: number; width: number; label: string }[] {
  const out: { channel: number; width: number; label: string }[] = [];
  for (let c = 1; c + 1 <= n; c += 2) out.push({ channel: c, width: 2, label: `Out ${c}–${c + 1}` });
  for (let c = 1; c <= n; c += 1) out.push({ channel: c, width: 1, label: `Out ${c}` });
  return out;
}

function OutputPick({ bus, g }: { bus: BusView; g: BusGlobals }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const label = bus.width === 2 ? `Out ${bus.channelLabel.replace("-", "–")}` : `Out ${bus.channelLabel}`;
  return (
    <>
      <button
        className="ins on outbtn"
        onClick={(e) => setAnchor(anchor ? null : e.currentTarget)}
        aria-expanded={anchor !== null}
        title="Which device outputs this bus leaves on"
      >
        <Icon name={bus.id === "main" ? "speaker" : "phones"} />
        {label}
        <span className="v">{bus.width === 2 ? T.stereo : T.mono}</span>
      </button>
      {anchor && (
        <Popover anchor={anchor} onClose={() => setAnchor(null)} className="menu">
          {outputChoices(g.outputChannels).map((c) => {
            const on = c.channel === bus.channel && c.width === bus.width;
            return (
              <button
                key={`${c.channel}-${c.width}`}
                role="menuitemradio"
                aria-checked={on}
                className={on ? "on" : ""}
                onClick={() => {
                  busActions.output(bus.id, c.channel, c.width);
                  setAnchor(null);
                }}
              >
                {c.label}
                <span className="v">{c.width === 2 ? T.stereo : T.mono}</span>
              </button>
            );
          })}
        </Popover>
      )}
    </>
  );
}

function BusStripImpl({ bus, g }: { bus: BusView; g: BusGlobals }) {
  const index = bus.id === "main" ? 0 : 1;
  const pick = (s: LooperStatus) => s.buses[index]?.peaks ?? [0, 0];
  const name = bus.id === "main" ? `${T.main} · Room` : `${T.monitorBus} · Phones`;
  return (
    <div className="strip bus" aria-label={`${bus.id === "main" ? T.main : T.monitorBus} bus`}>
      <div className="zone">
        <span className="caps">Inserts</span>
        <InsertSlots target={`bus.${bus.id}`} rows={2} />
      </div>
      <div className="zone">
        <span className="caps">Output</span>
        <OutputPick bus={bus} g={g} />
        {bus.hasClick || (g.collapsed && bus.id === "main") ? (
          <div className={`ins clk${g.click ? " on" : ""}`}>
            <button
              className="clk-sw"
              data-midi={GLOBAL.click}
              onClick={() => busActions.click(!g.click)}
              aria-pressed={g.click}
              title={g.click ? "Click on - click to switch off" : "Click off - click to switch on"}
            >
              <Icon name="metro" />
              {g.click ? T.click : "Click off"}
            </button>
            <DragNumber
              className="v"
              value={g.clickGain}
              def={1}
              curve={CLICK_CURVE}
              format={clickText}
              onChange={busActions.clickGain}
              address={GLOBAL.clickGain}
              label="Click level"
            />
          </div>
        ) : (
          <div className="ins nil" title="The click is on the Monitor bus only; it never reaches the room.">
            No click
          </div>
        )}
      </div>
      <div className="panrow sum">Stereo sum</div>
      <div className="fz">
        <Scale />
        <div className="pair">
          <Fader
            value={bus.gain}
            def={1}
            curve={BUS_FADER_CURVE}
            format={(v) => `${faderText(v)} dB`}
            onChange={(v) => busActions.gain(bus.id, Math.min(MAX_BUS_GAIN, v))}
            address={busGainAddress(bus.id)}
            label={`${bus.id === "main" ? T.main : T.monitorBus} volume`}
            title={bus.id === "main" ? "Volume of what goes to the room" : "Volume of the headphones - the room does not change"}
          />
          <VMeter pick={pick} channels={2} wide />
        </div>
      </div>
      <div className="stripfoot">
        <div className="vol">
          <b title={`Bus volume ${faderText(bus.gain)} dB`}>{faderText(bus.gain)}</b>
          <PeakHold pick={(s) => s.buses[index]?.peak ?? 0} />
        </div>
        <div className="nm bus">
          <Icon name={bus.id === "main" ? "speaker" : "phones"} />
          <span className="tname">{name}</span>
        </div>
      </div>
    </div>
  );
}

export const BusStrip = memo(BusStripImpl, (a, b) => JSON.stringify(a) === JSON.stringify(b));

/** Everything the bus strips draw that is a setting - their render key. */
export function busSignature(s: LooperStatus): string {
  return JSON.stringify({
    b: s.buses.map((b) => [b.id, b.channel, b.width, b.channel_label, b.gain, b.has_click]),
    c: s.click,
    g: s.click_gain,
    o: s.output_channels,
    x: s.buses_collapsed,
    n: s.bus_note,
  });
}
