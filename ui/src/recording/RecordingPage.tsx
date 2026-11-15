//! Setup → Recording: whether every run is written to disk, with or without dry stems, where to,
//! how much room is left there - and the newest recordings, with "Show in Finder" and "Delete".
//!
//! What a recording contains is explained in one card, in the words a DAW user expects, because
//! the difference between a stem and the mix (the live pass, the click, the bus inserts) is exactly
//! what one has to know before dragging the files into a session.

import { useCallback, useEffect, useState } from "react";
import { confirmAction, pickFolder } from "../dialogs";
import { Icon } from "../shell/Icon";
import { fail, run } from "../shell/notes";
import { T } from "../shell/terms";
import {
  bytes,
  clock,
  recordingApi,
  useRecording,
  type RecordingEntry,
  type RecordingSettings,
} from "./store";
import "./recording.css";

function Toggle({ on, onChange, title }: { on: boolean; onChange: (on: boolean) => void; title?: string }) {
  return (
    <button
      className={`toggle${on ? " on" : ""}`}
      role="switch"
      aria-checked={on}
      title={title}
      onClick={() => onChange(!on)}
    />
  );
}

export function RecordingPage() {
  const [settings, setSettings] = useState<RecordingSettings | null>(null);
  const [list, setList] = useState<RecordingEntry[]>([]);
  const { active, finished } = useRecording();

  const reload = useCallback(() => {
    recordingApi.settings().then(setSettings).catch((e) => fail((e as Error).message));
    recordingApi.list().then(setList).catch((e) => fail((e as Error).message));
  }, []);

  // Fresh numbers when the page opens and whenever a run has been finished.
  useEffect(reload, [reload, finished]);

  const save = (enabled: boolean, dry: boolean, folder: string | null) => {
    recordingApi
      .set(enabled, dry, folder)
      .then((s) => {
        setSettings(s);
        return recordingApi.list().then(setList);
      })
      .catch((e) => fail((e as Error).message));
  };

  const choose = async () => {
    if (!settings) return;
    const picked = await pickFolder("Folder for recordings", settings.folder);
    if (picked) save(settings.enabled, settings.dry, picked);
  };

  const remove = async (entry: RecordingEntry) => {
    const yes = await confirmAction(
      `Delete "${entry.name}" with all its files? This cannot be undone.`,
      "Delete recording",
      T.deleteRecording,
    );
    if (!yes) return;
    recordingApi
      .delete(entry.path)
      .then(setList)
      .catch((e) => fail((e as Error).message));
  };

  return (
    <>
      <div>
        <h1>{T.recording}</h1>
        <p className="lead">Every run as stems for a DAW: one file per track, plus the mix.</p>
      </div>
      <section className="card">
        <div className="card-h">
          <h3>Recording to disk</h3>
          {active && (
            <span className="hint">
              {T.recordingToDisk}: {clock(active.seconds)} - changes apply to the next run.
            </span>
          )}
        </div>
        <div className="card-b">
          <div className="srow">
            <label>{T.recordEveryRun}</label>
            <div>
              <Toggle
                on={settings?.enabled ?? true}
                onChange={(on) => settings && save(on, settings.dry, settings.default_folder ? null : settings.folder)}
                title="A score run from bar 1 of its first section to its end or Stop all; free play from Start to Stop of the clock."
              />
            </div>
          </div>
          <div className="srow">
            <label>{T.dryStems}</label>
            <div>
              <Toggle
                on={settings?.dry ?? true}
                onChange={(on) =>
                  settings && save(settings.enabled, on, settings.default_folder ? null : settings.folder)
                }
                title="Per track also loop playback plus the live input, before effects, fader and pan."
              />
            </div>
          </div>
          <div className="srow">
            <label>{T.recordingsFolder}</label>
            <div className="rec-folder">
              <span className="mono sel-text" title={settings?.folder}>
                {settings?.folder ?? "—"}
              </span>
              <button className="btn s" onClick={() => void choose()}>
                Choose…
              </button>
              {settings && !settings.default_folder && (
                <button className="btn s" onClick={() => save(settings.enabled, settings.dry, null)}>
                  Default
                </button>
              )}
              <button className="btn s" onClick={() => run(() => recordingApi.reveal())}>
                <Icon name="folder" />
                {T.showInFinder}
              </button>
            </div>
          </div>
          <div className="srow">
            <label>{T.freeSpace}</label>
            <div className={`rec-space${settings?.low_space ? " low" : ""}`}>
              {settings?.free_bytes == null ? "—" : bytes(settings.free_bytes)}
              {settings?.low_space && (
                <>
                  {" "}
                  <Icon name="warn" size={11} /> {T.lowSpace} - a long run may not fit. About 17 MB per minute and
                  stereo file at 48 kHz.
                </>
              )}
            </div>
          </div>
          <p className="hint">
            Runs shorter than 5 seconds, or without a single take, are not kept. Files are 24-bit WAV at the
            engine's sample rate; past 4 GB a file continues as RF64 (about four hours of stereo at 48 kHz).
          </p>
        </div>
      </section>

      <section className="card">
        <div className="card-h">
          <h3>What is in a recording</h3>
        </div>
        <div className="card-b">
          <p className="hint">
            <b>01 voice.wav</b> - the track as the room heard it from this track: built-in effects, plugin inserts,
            fader, mute/solo and pan; before the Main bus inserts and the Main volume. While the track records,
            overdubs or hears through, what is played is in it too - placed where it was played, so the first pass
            and the loop after it line up. If the monitored input does not reach Main, that live part is added
            through fader and pan but without effects; if it does reach Main, it is in the stem as the room heard
            it (with effects, one roundtrip late).
          </p>
          <p className="hint">
            <b>01 voice (dry).wav</b> - loop playback plus the raw live input, before everything else; mono for a
            mono track.
          </p>
          <p className="hint">
            <b>Mix.wav</b> - the Main bus after its inserts, before the Main volume, without the click. Without bus
            inserts the stems add up to it, except for live parts that never reached Main.
          </p>
          <p className="hint">
            Frame 0 of every file is bar 1 (of the first section for a score; the count-in is not in the files).{" "}
            <b>session.json</b> lists tempo, time signature, tracks and a marker for every section change; a copy of
            the score lies beside it.
          </p>
        </div>
      </section>

      <section className="card">
        <div className="card-h">
          <h3>{T.recentRecordings}</h3>
          <div className="grow" />
          <button className="btn s" onClick={reload} title="Read the folder again">
            Refresh
          </button>
        </div>
        {list.length === 0 ? (
          <div className="card-b">
            <span className="hint">No recordings in this folder yet.</span>
          </div>
        ) : (
          <table className="rec-list">
            <tbody>
              {list.map((r) => (
                <tr key={r.path}>
                  <td>
                    <div>{r.name}</div>
                    {r.title && <div className="t">{r.title}</div>}
                  </td>
                  <td className="num">{clock(r.seconds)}</td>
                  <td className="num">{bytes(r.bytes)}</td>
                  <td className="act">
                    <button className="btn s" onClick={() => run(() => recordingApi.reveal(r.path))}>
                      <Icon name="folder" />
                      {T.showInFinder}
                    </button>{" "}
                    <button
                      className="btn s danger"
                      disabled={active?.folder === r.path}
                      onClick={() => void remove(r)}
                    >
                      <Icon name="trash" />
                      {T.deleteRecording}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </>
  );
}
