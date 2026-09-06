//! The two output buses, as one strip under the head: where each one goes, how loud it is, and
//! what is leaving on it right now.
//!
//! It is one row and not a panel, on purpose. What a musician does with it during a song is one
//! thing - turn the headphones down - and what he does with it before the song is another: decide
//! which socket carries which bus. The first is a fader that has to be reachable at a glance; the
//! second is a select that is used once and then never looked at again, which is why the same
//! choice also stands in the setup screen.
//!
//! The meters are drawn by `Meter`, i.e. straight into the DOM without a render. Everything else
//! here is a setting and changes when somebody drags something, so a render per drag step is the
//! right cost.

import { api } from "../api";
import { busGainAddress } from "../midi/addresses";
import { currentStatus, useStatusSlice } from "../status";
import { Meter } from "./Meter";
import type { BusId, BusStatus, LooperStatus } from "../types";

/** Everything about the buses that is a *setting* - what a render has to follow. */
function signature(s: LooperStatus): string {
  const buses = s.buses.map((b) => `${b.id}:${b.channel}:${b.width}:${b.gain}:${b.has_click}`).join("|");
  return `${buses}~${s.output_channels}~${s.buses_collapsed}~${s.bus_note ?? ""}`;
}

/** Every routing a device with `outs` output channels can offer, stereo pairs first. */
function choices(outs: number): { value: string; label: string }[] {
  const list: { value: string; label: string }[] = [];
  for (let c = 1; c + 1 <= outs; c += 2) list.push({ value: `${c}-${c + 1}`, label: `${c}-${c + 1}` });
  for (let c = 1; c <= outs; c += 1) list.push({ value: `${c}`, label: `nur ${c}` });
  return list;
}

const peakOf = (bus: BusId, side: number) => (s: LooperStatus) =>
  s.buses.find((b) => b.id === bus)?.peaks?.[side] ?? 0;

interface Props {
  /** Report a German sentence (a confirmation or a refusal) the way the rest of the view does. */
  run: (work: () => Promise<unknown>) => void;
}

export function BusStrip({ run }: Props) {
  // One render when a routing, a volume or the note changes; none at all when a level moves. The
  // signature is what is watched; the objects themselves are read from the current status, because
  // a selector that returns the array would hand back a fresh one twenty times a second.
  useStatusSlice(signature);
  const status = currentStatus();
  const buses = status.buses;
  if (buses.length === 0) return null;

  return (
    <section className="bus-strip">
      {buses.map((bus) => (
        <BusColumn
          key={bus.id}
          bus={bus}
          outs={status.output_channels}
          // With both buses on one socket there is one mix and one fader for it. The second
          // volume is shown disabled rather than hidden: it is where it will be as soon as a
          // second output pair exists, and hiding it would make the strip change shape.
          deaf={status.buses_collapsed && bus.id === "monitor"}
          run={run}
        />
      ))}
      {status.bus_note && (
        <p className="bus-note" role="note">
          {status.buses_collapsed ? "Ein Weg hinaus, eine Mischung: " : ""}
          {status.bus_note}
        </p>
      )}
    </section>
  );
}

function BusColumn({
  bus,
  outs,
  deaf,
  run,
}: {
  bus: BusStatus;
  outs: number;
  deaf: boolean;
  run: Props["run"];
}) {
  const value = bus.width >= 2 ? `${bus.channel}-${bus.channel + 1}` : `${bus.channel}`;
  return (
    <div className={`bus-col bus-${bus.id}`}>
      <div className="bus-head">
        <span className="bus-name">{bus.label}</span>
        {bus.has_click && (
          <span className="bus-click" title="Der Klick liegt auf diesem Bus.">
            Klick
          </span>
        )}
      </div>
      <label className="bus-out">
        <span>Ausgang</span>
        <select
          value={value}
          onChange={(e) => {
            const [first, second] = e.target.value.split("-");
            run(() => api.busOutput(bus.id, Number(first), second === undefined ? 1 : 2));
          }}
          title="Auf welches Ausgangspaar dieser Bus geht. Ein einzelner Kanal faltet ihn auf mono — der Notbehelf mit zwei Ausgängen: Mix links, Klick rechts."
        >
          {choices(outs).map((c) => (
            <option key={c.value} value={c.value}>
              {c.label}
            </option>
          ))}
        </select>
      </label>
      <label className="bus-gain" data-midi={busGainAddress(bus.id)}>
        <span>Lautstärke</span>
        {/* The engine clamps at 4, and a MIDI fader covers all of it. This slider stops at 2:
            a mouse has one throw, and spending half of it on +6 to +12 dB - which is where a
            master bus is being abused, not mixed - would make the useful half twice as coarse. */}
        <input
          type="range"
          min={0}
          max={2}
          step={0.01}
          value={bus.gain}
          disabled={deaf}
          onChange={(e) => run(() => api.busGain(bus.id, Number(e.target.value)))}
          title={
            deaf
              ? "Beide Busse liegen auf demselben Ausgang: es gibt eine Mischung und damit einen Regler dafür — den von Main."
              : "Nur dieser Bus. Der andere bleibt, wo er ist — dafür gibt es zwei."
          }
        />
        <span className="mono bus-gain-value">{bus.gain.toFixed(2)}</span>
      </label>
      <div className="bus-meters">
        <Meter label="L" kind="out" pick={peakOf(bus.id, 0)} />
        <Meter label="R" kind="out" pick={peakOf(bus.id, 1)} />
      </div>
    </div>
  );
}
