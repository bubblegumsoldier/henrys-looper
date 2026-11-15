//! A small floating panel anchored to a control - menus and pickers of the mixer. Rendered into
//! `document.body` with fixed coordinates, because the strip row scrolls sideways and would clip
//! anything positioned inside it. Closes on a click outside and on Escape.

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

interface Props {
  anchor: HTMLElement | null;
  onClose: () => void;
  children: ReactNode;
  className?: string;
  /** Preferred side; flips when there is no room. */
  side?: "below" | "above";
  width?: number;
}

export function Popover({ anchor, onClose, children, className, side = "below", width }: Props) {
  const panel = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const close = useRef(onClose);
  close.current = onClose;

  useLayoutEffect(() => {
    if (!anchor || !panel.current) return;
    const a = anchor.getBoundingClientRect();
    const p = panel.current.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let top = side === "below" ? a.bottom + 4 : a.top - p.height - 4;
    if (side === "below" && top + p.height > vh - 8) top = a.top - p.height - 4;
    if (side === "above" && top < 8) top = a.bottom + 4;
    const left = Math.min(Math.max(8, a.left), vw - p.width - 8);
    setPos({ left, top: Math.max(8, Math.min(top, vh - p.height - 8)) });
  }, [anchor, side, children]);

  useEffect(() => {
    const down = (e: PointerEvent) => {
      const t = e.target as Node;
      if (panel.current?.contains(t) || anchor?.contains(t)) return;
      close.current();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close.current();
      }
    };
    document.addEventListener("pointerdown", down, true);
    window.addEventListener("keydown", key, true);
    return () => {
      document.removeEventListener("pointerdown", down, true);
      window.removeEventListener("keydown", key, true);
    };
  }, [anchor]);

  return createPortal(
    <div
      ref={panel}
      className={`mx-pop${className ? ` ${className}` : ""}`}
      style={{ left: pos?.left ?? -9999, top: pos?.top ?? -9999, width }}
      role="dialog"
    >
      {children}
    </div>,
    document.body,
  );
}
