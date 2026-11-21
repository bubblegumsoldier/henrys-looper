//! The sheet sidebar's layout - open or closed, width, font size - remembered in `localStorage`
//! like the arrangement's view settings (`arr.*`), and shared between the Live view's key (L) and
//! the sidebar itself.
//!
//! Without a stored choice the sidebar starts open only on a wide window: at 1024 px the stage
//! needs every pixel, and the sheet is one press away.

import { useSyncExternalStore } from "react";

export interface SheetLayout {
  open: boolean;
  /** Share of the window width, 0.2..0.7. */
  width: number;
  /** Font size of the sheet in px. */
  fontSize: number;
}

export const SHEET_FONT_MIN = 12;
export const SHEET_FONT_MAX = 40;
export const SHEET_WIDTH_MIN = 0.2;
export const SHEET_WIDTH_MAX = 0.7;
/** Below this window width the sidebar starts closed. */
export const SHEET_WIDE_WINDOW = 1280;

const KEY = "live.sheet";

function windowWidth(): number {
  return typeof window !== "undefined" && typeof window.innerWidth === "number" ? window.innerWidth : 0;
}

export function defaultLayout(width = windowWidth()): SheetLayout {
  return { open: width >= SHEET_WIDE_WINDOW, width: 0.4, fontSize: 20 };
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** A stored value made safe: anything missing or out of range falls back to the default. */
export function sanitize(value: unknown, fallback: SheetLayout = defaultLayout()): SheetLayout {
  const v = (value ?? {}) as Partial<SheetLayout>;
  return {
    open: typeof v.open === "boolean" ? v.open : fallback.open,
    width: typeof v.width === "number" && Number.isFinite(v.width) ? clamp(v.width, SHEET_WIDTH_MIN, SHEET_WIDTH_MAX) : fallback.width,
    fontSize:
      typeof v.fontSize === "number" && Number.isFinite(v.fontSize)
        ? clamp(Math.round(v.fontSize), SHEET_FONT_MIN, SHEET_FONT_MAX)
        : fallback.fontSize,
  };
}

function load(): SheetLayout {
  try {
    const raw = localStorage.getItem(KEY);
    return raw === null ? defaultLayout() : sanitize(JSON.parse(raw));
  } catch {
    return defaultLayout();
  }
}

let layout: SheetLayout | null = null;
const listeners = new Set<() => void>();

export function sheetLayout(): SheetLayout {
  if (layout === null) layout = load();
  return layout;
}

export function setSheetLayout(change: Partial<SheetLayout>): void {
  layout = sanitize({ ...sheetLayout(), ...change });
  try {
    localStorage.setItem(KEY, JSON.stringify(layout));
  } catch {
    /* a convenience only */
  }
  for (const l of listeners) l();
}

export function toggleSheet(): void {
  setSheetLayout({ open: !sheetLayout().open });
}

export function useSheetLayout(): SheetLayout {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    sheetLayout,
    sheetLayout,
  );
}
