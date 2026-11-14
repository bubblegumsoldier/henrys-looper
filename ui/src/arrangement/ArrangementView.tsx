//! The Arrangement mode - for now a shell around the existing score view (YAML editor, block
//! preview, live strip), recoloured with the design tokens.
//!
//! The next stage replaces the inside with the grid, the inspector and the mixer of
//! design/arrangement*.html; the toolbar row already has their place. The score view keeps its own
//! transport row hidden (the title bar has the transport now) but still listens to its keys, which
//! is why the frame switches its own score keys off while this mode is showing.

import { ScoreView } from "../components/ScoreView";
import { Icon } from "../shell/Icon";

export function ArrangementView() {
  return (
    <div className="workspace arr-shell">
      <div className="toolbar">
        <div className="seg">
          <button disabled title="Arrangement grid - next stage">
            <Icon name="grid" />
            Grid
          </button>
          <button className="on">
            <Icon name="code" />
            Text (YAML)
          </button>
        </div>
        <span className="lbl">The grid, inspector and mixer follow in the next stage.</span>
        <div className="grow" />
        <button className="btn" disabled title="Mixer - next stage">
          <Icon name="sliders" />
          Mixer
        </button>
        <button className="btn" disabled title="Inspector - next stage">
          <Icon name="split" />
          Inspector
        </button>
      </div>
      <div className="legacy legacy-score">
        <ScoreView />
      </div>
    </div>
  );
}
