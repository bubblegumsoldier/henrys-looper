//! The Arrangement mode: the score as a grid of sections × tracks, an inspector, and the same
//! document as YAML text (design/arrangement.html, design/arrangement-text.html).
//!
//! The transport, the position and the score's title live in the title bar; this view adds the
//! score file (File menu) and "Load" to its toolbar. The mixer of design/arrangement-mixer.html
//! (`../mixer`) docks under the grid and takes the inspector's place with its channel panel.

import { scoreApi } from "../api";
import { say } from "../shell/notes";
import { Arrangement } from "./Arrangement";
import { useScoreActions } from "./scoreActions";

const goto = (section: number) => say(() => scoreApi.goto(section));

export function ArrangementView() {
  const actions = useScoreActions();
  return <Arrangement onGoto={goto} actions={actions} />;
}
