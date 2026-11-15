//! Setup → MIDI: the devices with everything that can be done to them, every binding, and what
//! arrives. The same building blocks as the learn sidebar (`MidiSide.tsx`), with room to spare.

import { useEffect } from "react";
import { midiApi } from "../api";
import { IS_MAC } from "../platform";
import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import { M, plural } from "./labels";
import { BindingList, DeviceStatus, InputMonitor, MidiMessage, useGrouping, WaitCard } from "./MidiSide";
import { clearMidiEvents, loadMidiView, sayMidi, setMidiView, useMidiEvents, useMidiView } from "./store";
import { bindingWorks } from "./types";
import "./midi.css";

export function MidiSetup() {
  const view = useMidiView();
  const events = useMidiEvents();
  const [grouping, setGrouping] = useGrouping();
  useEffect(loadMidiView, []);

  const inactive = view.bindings.filter((b) => !bindingWorks(b.state)).length;
  const rescan = () =>
    midiApi
      .ports()
      .then(() => midiApi.state())
      .then(setMidiView)
      .catch((e) => sayMidi((e as Error).message));

  return (
    <div className="midi-setup" style={{ display: "contents" }}>
      <div>
        <h1>MIDI</h1>
        <p className="lead">
          Controllers and what they are bound to. Click <b>{T.midiLearn}</b> in the title bar, then a control, then
          press a pad - or {IS_MAC ? "⌘" : "Ctrl"}+click a control without the switch. Known controllers connect by
          themselves; every change is saved at once.
        </p>
      </div>

      {view.learning ? <WaitCard view={view} /> : <MidiMessage view={view} sticky />}

      <div className="midi-cols">
        <div style={{ display: "flex", flexDirection: "column", gap: 18, minWidth: 0 }}>
          <section className="card">
            <div className="card-h">
              <Icon name="pedal" />
              <h3>Devices</h3>
              <span className="dim" title="One YAML file per device">
                {view.dir}
              </span>
              <div className="grow" />
              <button className="btn s" onClick={() => void rescan()}>
                <Icon name="search" />
                {M.scanAgain}
              </button>
            </div>
            <div className="card-b midi-devs">
              <DeviceStatus view={view} full />
            </div>
            {!IS_MAC && (
              <p className="hint" style={{ margin: "0 16px 12px" }}>
                Windows hands a MIDI device to one program at a time: while Ableton uses the controller, it shows as
                busy here.
              </p>
            )}
          </section>

          <section className="card">
            <div className="card-h">
              <Icon name="search" />
              <h3>{M.inputMonitor}</h3>
              <span className="dim2" style={{ fontSize: 11 }}>
                {M.channels}
              </span>
              <div className="grow" />
              <button className="btn s ghost" onClick={clearMidiEvents} disabled={events.length === 0}>
                Clear
              </button>
            </div>
            <InputMonitor view={view} lines={14} card />
            {view.dropped > 0 && (
              <p className="hint" style={{ margin: "0 16px 12px", color: "var(--warn)" }}>
                {view.dropped} MIDI events were lost - the queue overflowed.
              </p>
            )}
          </section>
        </div>

        <section className="card">
          <div className="card-h">
            <Icon name="midi" />
            <h3>Bindings</h3>
            <span className="dim2" style={{ fontSize: 11 }}>
              {plural(view.bindings.length, "binding")}
              {inactive > 0 && ` · ${inactive} inactive`}
            </span>
            <div className="grow" />
            <div className="seg">
              <span className={grouping === "area" ? "on" : ""} onClick={() => setGrouping("area")} role="button">
                {M.byArea}
              </span>
              <span className={grouping === "device" ? "on" : ""} onClick={() => setGrouping("device")} role="button">
                {M.byDevice}
              </span>
            </div>
          </div>
          {view.notes.length > 0 && (
            <ul className="midi-notes">
              {view.notes.map((n) => (
                <li key={n}>{n}</li>
              ))}
            </ul>
          )}
          <BindingList view={view} grouping={grouping} />
        </section>
      </div>
    </div>
  );
}
