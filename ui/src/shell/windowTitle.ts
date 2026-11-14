//! The window title names the score file and marks unsaved changes; closing the window with unsaved
//! changes asks first. Mounted once by the frame, because the score view is not always mounted.
//! (With the overlay title bar the title is not drawn on macOS, but the Dock, Mission Control and
//! the Window menu still show it.)

import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { insideTauri } from "../api";
import { confirmAction } from "../dialogs";
import { isDirty, scoreFile, subscribeScoreFile } from "../scoreFile";

export function useWindowTitle(): void {
  useEffect(() => {
    if (!insideTauri()) return;
    const win = getCurrentWindow();
    let shown = "";
    const show = () => {
      const file = scoreFile();
      const title = `${file.name ?? "Untitled"}${isDirty(file) ? " •" : ""} — Henrys Looper`;
      if (title !== shown) {
        shown = title;
        void win.setTitle(title).catch(() => undefined);
      }
    };
    show();
    const offFile = subscribeScoreFile(show);
    let offClose: (() => void) | null = null;
    let disposed = false;
    void win
      .onCloseRequested(async (event) => {
        if (!isDirty()) return;
        const name = scoreFile().name ?? "the untitled score";
        const quit = await confirmAction(
          `${name} has unsaved changes. Quit without saving?`,
          "Unsaved changes",
          "Quit without saving",
        );
        if (!quit) event.preventDefault();
      })
      .then((off) => {
        if (disposed) off();
        else offClose = off;
      });
    return () => {
      disposed = true;
      offFile();
      offClose?.();
    };
  }, []);
}
