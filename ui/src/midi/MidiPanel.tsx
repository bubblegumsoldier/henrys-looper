//! The MIDI strip: connection, learn switch, monitor and the profile overview.
//!
//! # Where this sits, and why it is not a third tab
//!
//! There are two tabs, „Live“ and „Partitur“, and each already borrows a little of the other so
//! that neither has to be chosen against the other on stage (see `App.tsx`). A third one would be a
//! third thing to decide between while a song runs, for something that is set up **before** the
//! song and looked at almost never during it.
//!
//! So this is one slim row under the header, in both tabs, which says the two things that matter at
//! a glance - is the controller there, and is the learn mode on - and folds out into the rest:
//! device list, monitor, profile. Folded, it costs one line; folded out, it is a setup screen that
//! covers whatever is behind it and is closed again with the same click.
//!
//! # What the row shows while playing
//!
//! Connection and nothing else. A pad that stopped working is the one MIDI question a musician has
//! mid-song, and „verbunden“ against „getrennt“ answers it without opening anything.

import { useCallback, useEffect, useState } from "react";
import { midiApi } from "../api";
import { MidiMonitor } from "./MidiMonitor";
import { sayMidi, setLearnMode, setMidiView, useLearnMode, useMidiView } from "./store";
import type { MidiPortView } from "./types";

export function MidiPanel() {
  const view = useMidiView();
  const learn = useLearnMode();
  const [open, setOpen] = useState(false);
  const [ports, setPorts] = useState<MidiPortView[] | null>(null);
  const [device, setDevice] = useState("");
  const [busy, setBusy] = useState(false);

  // Whatever the Rust side already knows - a connection survives a reload of the web view.
  useEffect(() => {
    midiApi
      .state()
      .then(setMidiView)
      .catch(() => {
        // No engine behind this window (a browser tab on the dev server). The panel still works as
        // a place to look at; every button will say the same thing when pressed.
      });
  }, []);

  const refresh = useCallback(() => {
    setBusy(true);
    midiApi
      .ports()
      .then((found) => {
        setPorts(found);
        setDevice((current) => (current !== "" ? current : found[0] ? String(found[0].index) : ""));
      })
      .catch((e) => sayMidi((e as Error).message))
      .finally(() => setBusy(false));
  }, []);

  // The port list is asked for when the panel is opened, not on every render: enumerating MIDI
  // inputs is cheap but not free, and a list nobody is looking at is a list nobody needs.
  useEffect(() => {
    if (open && ports === null) refresh();
  }, [open, ports, refresh]);

  const run = useCallback((work: () => Promise<unknown>) => {
    setBusy(true);
    work()
      .catch((e) => sayMidi((e as Error).message))
      .finally(() => setBusy(false));
  }, []);

  const connect = () => run(() => midiApi.open(device).then(setMidiView));
  const disconnect = () => run(() => midiApi.close().then(setMidiView));
  const save = () => run(() => midiApi.save().then((saved) => setMidiView(saved.view)));
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

  const bindings = view.bindings.length;

  return (
    <section className={`midi${open ? " midi-open" : ""}${learn ? " midi-learn-on" : ""}`}>
      <div className="midi-bar">
        <button
          className="btn btn-mini midi-fold"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          title="Geräte, Monitor und Belegung"
        >
          MIDI {open ? "▲" : "▼"}
        </button>

        <span className="midi-state">
          <span className={`dot ${view.connected ? "dot-ok" : "dot-err"}`} />
          {view.connected ? (view.port ?? "verbunden") : "kein Gerät verbunden"}
        </span>

        <span className="midi-count muted">
          {bindings === 0 ? "keine Belegung" : `${bindings} ${bindings === 1 ? "Belegung" : "Belegungen"}`}
          {view.dirty && <b className="midi-dirty"> · ungespeichert</b>}
        </span>

        <button
          className={`btn midi-learn-switch${learn ? " btn-on" : ""}`}
          onClick={toggleLearn}
          title="Solange das an ist, belegt ein Klick auf ein Bedienelement dieses Element, statt es zu bedienen. Danach das gewünschte Pad drücken. Strg+Klick geht auch ohne den Schalter."
        >
          MIDI lernen {learn ? "an" : "aus"}
        </button>

        {learn && !view.learning && (
          <span className="midi-hint-inline">
            Jetzt das Bedienelement anklicken, das belegt werden soll.
          </span>
        )}
        {view.learning && (
          <span className="midi-waiting-banner" aria-live="polite">
            wartet auf Taste für <b>{view.learning_label ?? view.learning}</b>
            <button className="btn btn-mini" onClick={cancelLearn} disabled={busy}>
              Esc — abbrechen
            </button>
          </span>
        )}

        <div className="spacer" />

        {view.dirty && (
          <button className="btn btn-mini btn-on" onClick={save} disabled={busy}>
            Profil speichern
          </button>
        )}
      </div>

      {view.message && <div className="midi-message">{view.message}</div>}

      {open && (
        <div className="midi-panel">
          <div className="midi-devices">
            <label className="field">
              <span>MIDI-Eingang</span>
              <select value={device} onChange={(e) => setDevice(e.target.value)} disabled={busy}>
                {(ports ?? []).length === 0 && <option value="">— keiner gefunden —</option>}
                {(ports ?? []).map((port) => (
                  <option key={port.index} value={String(port.index)}>
                    {port.name}
                    {port.profile ? ` · Profil "${port.profile}" (${port.bindings})` : " · kein Profil"}
                  </option>
                ))}
              </select>
            </label>
            <button className="btn btn-mini" onClick={refresh} disabled={busy}>
              Neu suchen
            </button>
            {view.connected ? (
              <button className="btn btn-mini btn-danger" onClick={disconnect} disabled={busy}>
                Trennen
              </button>
            ) : (
              <button className="btn" onClick={connect} disabled={busy || (ports ?? []).length === 0}>
                Verbinden
              </button>
            )}
            <button className="btn btn-mini" onClick={save} disabled={busy || !view.dirty}>
              Profil speichern
            </button>
            {view.path && (
              <span className="midi-path mono muted" title="Hier liegt das Controller-Profil">
                {view.path}
              </span>
            )}
          </div>

          <p className="midi-explain muted">
            Windows gibt ein MIDI-Gerät immer nur an ein Programm gleichzeitig heraus: läuft Ableton
            mit dem Controller als Control Surface, lässt er sich hier nicht öffnen. — Das Profil
            gehört dem Gerät und gilt für jedes Stück; eine Partitur kann einzelne Tasten für sich
            umbelegen, und das steht dann hier als Herkunft „Partitur“.
          </p>

          {view.notes.length > 0 && (
            <ul className="midi-notes">
              {view.notes.map((note) => (
                <li key={note}>{note}</li>
              ))}
            </ul>
          )}

          {view.dropped > 0 && (
            <div className="midi-dropped">
              {view.dropped} MIDI-Ereignisse sind verloren gegangen — die Queue lief über.
            </div>
          )}

          <div className="midi-columns">
            <MidiMonitor />
            <Bindings busy={busy} onUnbind={(id) => run(() => midiApi.unbind(id).then(setMidiView))} />
          </div>
        </div>
      )}
    </section>
  );
}

/**
 * Which key does what. Sorted by the key, exactly as the profile file is written, so the list on
 * screen and the file read the same way round.
 */
function Bindings({ busy, onUnbind }: { busy: boolean; onUnbind: (id: string) => void }) {
  const view = useMidiView();

  return (
    <section className="midi-bindings">
      <div className="midi-monitor-head">
        <h3>Belegung</h3>
        <span className="muted">
          {view.bindings.length === 0
            ? "Noch nichts belegt — „MIDI lernen“ einschalten und ein Bedienelement anklicken."
            : `${view.bindings.length} Tasten`}
        </span>
      </div>
      {view.bindings.length > 0 && (
        <ul className="midi-binding-list">
          {view.bindings.map((binding) => (
            <li key={binding.id} className={binding.from_score ? "midi-binding-score" : undefined}>
              <span className="midi-binding-key mono" title={binding.id}>
                {binding.short}
              </span>
              <span className="midi-binding-label">{binding.label}</span>
              <span className="midi-binding-address mono muted">{binding.address}</span>
              {binding.from_score && (
                <span className="midi-binding-source" title="Kommt aus dem 'midi:'-Block der geladenen Partitur und geht mit ihr wieder weg">
                  Partitur
                </span>
              )}
              <button
                className="btn btn-mini btn-danger"
                onClick={() => onUnbind(binding.id)}
                disabled={busy || binding.from_score}
                title={
                  binding.from_score
                    ? "Diese Bindung steht in der Partitur, nicht im Profil"
                    : "Diese Belegung wieder lösen"
                }
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
