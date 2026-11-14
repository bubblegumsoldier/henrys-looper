//! The MIDI strip: devices, learn switch, monitor and the bindings with their status.
//!
//! # Where this sits, and why it is not a third tab
//!
//! There are two tabs, „Live“ and „Partitur“, and each already borrows a little of the other so
//! that neither has to be chosen against the other on stage (see `App.tsx`). A third one would be a
//! third thing to decide between while a song runs, for something that is set up **before** the
//! song and looked at almost never during it.
//!
//! So this is one slim row under the header, in both tabs, which says the two things that matter at
//! a glance - which controllers are there (and how many bindings sleep because one is not), and is
//! the learn mode on - and folds out into the rest: devices, monitor, bindings.
//!
//! # No "connect", no "save" in the normal case
//!
//! Known controllers connect by themselves at start and when plugged in, and every learned,
//! removed or edited binding is written at once (see `app/src-tauri/src/midi.rs`). "Connect" is
//! only needed once for a device that has never been used; from then on it is remembered. A save
//! button only appears when writing a profile failed.
//!
//! Everything shown here comes from the view the backend sends (`types.ts`); this component holds
//! no MIDI state of its own, so a new design can replace it without losing anything.

import { useCallback, useEffect, useState } from "react";
import { midiApi } from "../api";
import { IS_MAC } from "../platform";
import { MidiMonitor } from "./MidiMonitor";
import { sayMidi, setLearnMode, setMidiView, useLearnMode, useMidiView } from "./store";
import {
  bindingWorks,
  type MidiBindingOptions,
  type MidiBindingView,
  type MidiDeviceState,
  type MidiDeviceView,
  type MidiView,
} from "./types";

const DEVICE_STATE: Record<MidiDeviceState, { label: string; dot: string }> = {
  connected: { label: "connected", dot: "dot-ok" },
  missing: { label: "not plugged in", dot: "dot-err" },
  disconnected: { label: "disconnected", dot: "dot-warn" },
  unavailable: { label: "busy", dot: "dot-warn" },
  error: { label: "profile error", dot: "dot-err" },
};

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

export function MidiPanel() {
  const view = useMidiView();
  const learn = useLearnMode();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  // Whatever the Rust side already knows - connections survive a reload of the web view.
  useEffect(() => {
    midiApi
      .state()
      .then(setMidiView)
      .catch(() => {
        // No engine behind this window (a browser tab on the dev server).
      });
  }, []);

  const run = useCallback((work: () => Promise<unknown>) => {
    setBusy(true);
    work()
      .catch((e) => sayMidi((e as Error).message))
      .finally(() => setBusy(false));
  }, []);

  const cancelLearn = () => run(() => midiApi.learn(null).then(setMidiView));

  /**
   * The switch. Turning it off also takes back a pending target: the Rust side would otherwise keep
   * waiting for a control, and the next pad pressed for a completely different reason would be
   * swallowed and bound to whatever was last clicked.
   */
  const toggleLearn = () => {
    const next = !learn;
    setLearnMode(next);
    if (!next && view.learning) cancelLearn();
  };

  const total = view.bindings.length;
  const inactive = view.bindings.filter((b) => !bindingWorks(b.state)).length;
  const unknownPorts = view.ports.filter((p) => !p.connected && p.profile === null);

  return (
    <section className={`midi${open ? " midi-open" : ""}${learn ? " midi-learn-on" : ""}`}>
      <div className="midi-bar">
        <button
          className="btn btn-mini midi-fold"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          title="Devices, monitor and bindings"
        >
          MIDI {open ? "▲" : "▼"}
        </button>

        <DeviceChips view={view} unknownPorts={unknownPorts.length} />

        <span className="midi-count muted">
          {total === 0 ? "no bindings" : plural(total, "binding", "bindings")}
          {inactive > 0 && <b className="midi-inactive-count"> · {inactive} inactive</b>}
        </span>

        <button
          className={`btn midi-learn-switch${learn ? " btn-on" : ""}`}
          onClick={toggleLearn}
          title={`While this is on, clicking a control maps it instead of operating it. Then press the pad, key or knob on the controller. ${IS_MAC ? "⌘" : "Ctrl"}+click works without the switch.`}
        >
          MIDI Learn {learn ? "on" : "off"}
        </button>

        {learn && !view.learning && (
          <span className="midi-hint-inline">Click the control you want to map.</span>
        )}
        {view.learning && (
          <span className="midi-waiting-banner" aria-live="polite">
            waiting for a control for <b>{view.learning_label ?? view.learning}</b>
            <button className="btn btn-mini" onClick={cancelLearn} disabled={busy}>
              Esc — cancel
            </button>
          </span>
        )}

        <div className="spacer" />

        {view.save_error ? (
          <span className="midi-save-error" title={view.save_error}>
            <b>Not saved</b>
            <button
              className="btn btn-mini btn-on"
              onClick={() => run(() => midiApi.save().then((saved) => setMidiView(saved.view)))}
              disabled={busy}
            >
              Retry
            </button>
          </span>
        ) : (
          total > 0 && (
            <span className="midi-saved muted" title={`Saved automatically in ${view.dir}`}>
              saved
            </span>
          )
        )}
      </div>

      {view.message && <div className="midi-message">{view.message}</div>}

      {open && (
        <div className="midi-panel">
          <Devices view={view} busy={busy} run={run} />

          <p className="midi-explain muted">
            Known controllers connect by themselves when the app starts and whenever they are plugged
            in; every mapping is saved at once. A profile belongs to the device and applies to every
            piece; a score can remap single controls for itself — those are marked „Score“.
            {!IS_MAC &&
              " Windows hands a MIDI device to one program at a time: while Ableton uses the controller, it shows as busy here."}
          </p>

          {view.notes.length > 0 && (
            <ul className="midi-notes">
              {view.notes.map((note) => (
                <li key={note}>{note}</li>
              ))}
            </ul>
          )}

          {view.dropped > 0 && (
            <div className="midi-dropped">{view.dropped} MIDI events were lost — the queue overflowed.</div>
          )}

          <div className="midi-columns">
            <MidiMonitor />
            <Bindings view={view} busy={busy} run={run} />
          </div>
        </div>
      )}
    </section>
  );
}

/** The bar's at-a-glance line: one chip per known device. */
function DeviceChips({ view, unknownPorts }: { view: MidiView; unknownPorts: number }) {
  if (view.devices.length === 0) {
    return (
      <span className="midi-state">
        <span className="dot dot-err" />
        {unknownPorts > 0
          ? `no device connected · ${plural(unknownPorts, "input", "inputs")} available`
          : "no MIDI device"}
      </span>
    );
  }
  return (
    <span className="midi-chips">
      {view.devices.map((d) => (
        <span key={d.path} className={`midi-chip midi-chip-${d.state}`} title={d.summary}>
          <span className={`dot ${DEVICE_STATE[d.state].dot}`} />
          {d.device}
          {!d.connected && d.bindings > 0 && (
            <span className="midi-chip-note">
              {" "}
              {DEVICE_STATE[d.state].label} · {d.inactive} inactive
            </span>
          )}
          {d.connected && d.inactive > 0 && <span className="midi-chip-note"> · {d.inactive} inactive</span>}
        </span>
      ))}
    </span>
  );
}

function lastSeen(device: MidiDeviceView): string | null {
  if (device.connected || device.last_seen === null) return null;
  const at = new Date(device.last_seen);
  return `last seen ${at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
}

/** Known devices with their state, and inputs that have never been used. */
function Devices({
  view,
  busy,
  run,
}: {
  view: MidiView;
  busy: boolean;
  run: (work: () => Promise<unknown>) => void;
}) {
  const connect = (name: string) => run(() => midiApi.open(name).then(setMidiView));
  const disconnect = (name: string) => run(() => midiApi.close(name).then(setMidiView));
  const rescan = () => run(() => midiApi.ports().then(() => midiApi.state().then(setMidiView)));
  const unknown = view.ports.filter((p) => !p.connected && p.profile === null);

  return (
    <section className="midi-device-list">
      <div className="midi-monitor-head">
        <h3>Devices</h3>
        <span className="muted mono" title="Profiles are kept here, one YAML file per device">
          {view.dir}
        </span>
        <div className="spacer" />
        <button className="btn btn-mini" onClick={rescan} disabled={busy}>
          Rescan
        </button>
      </div>
      <ul className="midi-device-rows">
        {view.devices.map((d) => {
          const port = view.ports.find((p) => p.profile === d.device && !p.connected);
          const seen = lastSeen(d);
          return (
            <li key={d.path} className={`midi-device midi-device-${d.state}`}>
              <span className={`dot ${DEVICE_STATE[d.state].dot}`} />
              <b>{d.device}</b>
              <span className="midi-device-summary">
                {d.summary}
                {seen && <span className="muted"> · {seen}</span>}
              </span>
              <span className="mono muted midi-path" title={d.path}>
                {d.path.split(/[\\/]/).pop()}
              </span>
              {d.connected && d.port ? (
                <button className="btn btn-mini" onClick={() => disconnect(d.port!)} disabled={busy}>
                  Disconnect
                </button>
              ) : port ? (
                <button className="btn btn-mini" onClick={() => connect(port.name)} disabled={busy}>
                  Connect
                </button>
              ) : (
                <span />
              )}
            </li>
          );
        })}
        {unknown.map((p) => (
          <li key={`port-${p.name}`} className="midi-device midi-device-new">
            <span className="dot" />
            <b>{p.name}</b>
            <span className="midi-device-summary muted">new input — connect once, it is remembered from then on</span>
            <span />
            <button className="btn btn-mini" onClick={() => connect(p.name)} disabled={busy}>
              Connect
            </button>
          </li>
        ))}
        {view.devices.length === 0 && unknown.length === 0 && (
          <li className="muted">No MIDI input found. Plug in a controller — it shows up here by itself.</li>
        )}
      </ul>
    </section>
  );
}

/**
 * Which control does what, grouped by device, with whether it works right now and its options.
 * Within a device sorted by the control, exactly as the profile file is written.
 */
function Bindings({
  view,
  busy,
  run,
}: {
  view: MidiView;
  busy: boolean;
  run: (work: () => Promise<unknown>) => void;
}) {
  const groups: { title: string; key: string; bindings: MidiBindingView[] }[] = view.devices
    .filter((d) => d.bindings > 0)
    .map((d) => ({
      title: d.device,
      key: d.path,
      bindings: view.bindings.filter((b) => !b.from_score && b.device === d.device),
    }));
  const fromScore = view.bindings.filter((b) => b.from_score);
  if (fromScore.length > 0) groups.push({ title: "Score", key: "score", bindings: fromScore });

  const unbind = (b: MidiBindingView) => run(() => midiApi.unbind(b.id, b.device).then(setMidiView));
  const edit = (b: MidiBindingView, options: MidiBindingOptions) =>
    run(() => midiApi.edit(b.id, b.device, options).then(setMidiView));

  return (
    <section className="midi-bindings">
      <div className="midi-monitor-head">
        <h3>Mappings</h3>
        <span className="muted">
          {view.bindings.length === 0
            ? "Nothing mapped yet — switch on MIDI Learn and click a control."
            : plural(view.bindings.length, "mapping", "mappings")}
        </span>
      </div>
      {groups.map((group) => (
        <div key={group.key} className="midi-binding-group">
          <div className="midi-binding-group-title">{group.title}</div>
          <ul className="midi-binding-list">
            {group.bindings.map((b) => (
              <BindingRow key={`${b.device ?? "score"}:${b.id}`} binding={b} busy={busy} onUnbind={unbind} onEdit={edit} />
            ))}
          </ul>
        </div>
      ))}
    </section>
  );
}

const STATE_LABEL: Record<MidiBindingView["state"], string> = {
  active: "active",
  unchecked: "not checked",
  device_missing: "device missing",
  target_missing: "no target",
  overridden: "score overrides",
};

function BindingRow({
  binding: b,
  busy,
  onUnbind,
  onEdit,
}: {
  binding: MidiBindingView;
  busy: boolean;
  onUnbind: (b: MidiBindingView) => void;
  onEdit: (b: MidiBindingView, options: MidiBindingOptions) => void;
}) {
  const works = bindingWorks(b.state);
  const current: MidiBindingOptions = { mode: b.mode, takeover: b.takeover, min: b.min, max: b.max };
  return (
    <li
      className={`midi-binding${b.from_score ? " midi-binding-score" : ""}${works ? "" : " midi-binding-inactive"}`}
    >
      <span className="midi-binding-key mono" title={b.id}>
        {b.short}
      </span>
      <span className="midi-binding-label" title={b.describe}>
        {b.label}
        <span className="midi-binding-address mono muted"> {b.address}</span>
        {b.reason && <span className="midi-binding-reason">{b.reason}</span>}
      </span>
      <span className={`midi-binding-state midi-state-${b.state}`} title={b.reason ?? "works"}>
        {STATE_LABEL[b.state]}
      </span>
      {b.from_score ? (
        <span className="midi-binding-source" title="From the score's 'midi:' block; it goes when the score goes">
          Score
        </span>
      ) : (
        <BindingOptionsEditor binding={b} busy={busy} onChange={(o) => onEdit(b, { ...current, ...o })} />
      )}
      <button
        className="btn btn-mini btn-danger"
        onClick={() => onUnbind(b)}
        disabled={busy || b.from_score}
        title={b.from_score ? "This mapping is in the score, not in the profile" : "Remove this mapping"}
      >
        ✕
      </button>
    </li>
  );
}

/** Mode for a button; takeover and range for a knob. Changes are saved at once. */
function BindingOptionsEditor({
  binding: b,
  busy,
  onChange,
}: {
  binding: MidiBindingView;
  busy: boolean;
  onChange: (options: Partial<MidiBindingOptions>) => void;
}) {
  if (b.control !== "range") {
    return (
      <select
        className="midi-option"
        value={b.mode ?? ""}
        disabled={busy}
        onChange={(e) => onChange({ mode: e.target.value })}
        title="press: on press · release: on release · toggle: each press switches · momentary: on while held"
      >
        {b.modes.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
    );
  }
  return (
    <span className="midi-option-range">
      <select
        className="midi-option"
        value={b.takeover ?? "pickup"}
        disabled={busy}
        onChange={(e) => onChange({ takeover: e.target.value })}
        title="pickup: the knob acts once it passes the current value · jump: acts at once"
      >
        <option value="pickup">pickup</option>
        <option value="jump">jump</option>
      </select>
      <NumberField value={b.min} placeholder={b.range_min} disabled={busy} title="Lowest value (empty = full range)" onCommit={(min) => onChange({ min })} />
      <span className="muted">–</span>
      <NumberField value={b.max} placeholder={b.range_max} disabled={busy} title="Highest value (empty = full range)" onCommit={(max) => onChange({ max })} />
    </span>
  );
}

/** A number that is sent when the field is left or Enter is pressed; empty means "default". */
function NumberField({
  value,
  placeholder,
  disabled,
  title,
  onCommit,
}: {
  value: number | null;
  placeholder: number | null;
  disabled: boolean;
  title: string;
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
      className="midi-option-number mono"
      inputMode="decimal"
      value={text}
      placeholder={placeholder === null ? "" : String(placeholder)}
      disabled={disabled}
      title={title}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
    />
  );
}
