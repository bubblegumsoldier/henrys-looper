//! What the Arrangement view needs from the mixer - three pieces, so its own file only has to
//! place them:
//!
//! * `MixerToggle` - the toolbar button (also `M` on the keyboard is deliberately not taken: the
//!   arrangement owns its keys),
//! * `Mixer` - the section at the bottom of the editor area (or the whole of it in full view),
//! * `ChannelPanel` - the selected channel's details, in the inspector's place while the mixer is
//!   open.
//!
//! `mixerEditorClass(m)` is the class the editor container needs for the docked or full layout.

import { Icon } from "../shell/Icon";
import { T } from "../shell/terms";
import { setMixer, useMixer, type MixerState } from "./store";
import "./mixer.css";

export { Mixer } from "./Mixer";
export { ChannelPanel } from "./ChannelPanel";
export { useMixer, selectChannel, selectChannelByName } from "./store";

export function MixerToggle() {
  const m = useMixer();
  return (
    <button
      className={`btn s${m.open ? " on" : ""}`}
      onClick={() => setMixer({ open: !m.open })}
      aria-pressed={m.open}
      title={m.open ? "Hide the mixer" : "Show the mixer: channel strips, buses and the selected channel's effects"}
    >
      <Icon name="sliders" />
      {T.mixer}
    </button>
  );
}

export function mixerEditorClass(m: MixerState): string {
  if (!m.open) return "";
  return m.full ? " with-mixer mixer-full" : " with-mixer";
}
