//! The recorder in the title bar.
//!
//! While a run is written to disk: a red dot, the running time and "Recording to disk", plus a
//! square to stop it. Calm on purpose - the dot does not blink, because on a stage anything that
//! blinks is read as an alarm. After a run: one line with the folder's name and "Show in Finder",
//! until it is clicked away or the next run starts.

import { useEffect } from "react";
import { Icon } from "../shell/Icon";
import { fail, note, run } from "../shell/notes";
import { T } from "../shell/terms";
import { clock, dismissFinished, recordingApi, useRecording } from "./store";
import "./recording.css";

export function RecordingBadge() {
  const { active, finished } = useRecording();

  // The sentence also goes to the ticker, so it is in the history on hover.
  useEffect(() => {
    if (!finished) return;
    if (finished.error) fail(finished.text);
    else note(finished.text, "engine");
  }, [finished]);

  if (active) {
    const title = `${T.recordingToDisk}: ${active.folder}${
      active.overflows > 0 ? ` · the disk fell behind ${active.overflows}×, gaps filled with silence` : ""
    }`;
    return (
      <div className={`rec-badge${active.stopping ? " tail" : ""}`} title={title} role="status">
        <i className="rec-dot" aria-hidden />
        <b className="mono">{clock(active.seconds)}</b>
        <span className="hide-narrow">{active.stopping ? T.recordingTail : T.recordingToDisk}</span>
        {active.overflows > 0 && <Icon name="warn" size={11} />}
        {!active.stopping && (
          <button
            className="btn s sq ghost"
            onClick={() => run(() => recordingApi.stop())}
            title={T.stopRecording}
            aria-label={T.stopRecording}
          >
            <Icon name="stop" />
          </button>
        )}
      </div>
    );
  }

  if (finished?.kept) {
    return (
      <div className="rec-badge done" title={finished.folder}>
        <Icon name="check" size={11} />
        <span className="name">{finished.name}</span>
        <button className="btn s" onClick={() => run(() => recordingApi.reveal(finished.folder))}>
          <Icon name="folder" />
          {T.showInFinder}
        </button>
        <button className="btn s sq ghost" onClick={dismissFinished} aria-label="Dismiss" title="Dismiss">
          <Icon name="x" />
        </button>
      </div>
    );
  }
  return null;
}
