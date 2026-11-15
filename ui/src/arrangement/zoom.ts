//! Horizontal zoom of the grid: pixels per bar, and the arithmetic that keeps one bar in place
//! while the scale changes (zoom around the pointer, the selection or the middle of the view).

export const ZOOM_MIN = 6;
export const ZOOM_MAX = 80;
/** One step of the zoom buttons and of the +/- keys. */
export const ZOOM_STEP = 1.25;

export function clampZoom(z: number): number {
  if (!Number.isFinite(z)) return 20;
  return Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));
}

/** A zoom the grid can draw cleanly: whole device pixels per bar, so the bar lines (a repeating
 *  gradient of period `--b`) stay sharp. On a Retina screen that is half a CSS pixel, which is
 *  fine enough for a pinch to feel continuous. */
export function snapZoom(z: number, dpr = 1): number {
  const step = Math.max(1, Math.round(dpr));
  return clampZoom(Math.round(clampZoom(z) * step) / step);
}

/** Scale factor for one wheel event with Cmd/Ctrl held (or a trackpad pinch, which WebKit reports
 *  as a wheel with `ctrlKey`). Proportional to the delta, so the many small pinch deltas add up
 *  smoothly, and capped per event, so one notch of a mouse wheel is about one +/- step. */
export function wheelZoomFactor(deltaY: number, deltaMode = 0): number {
  const px = deltaMode === 1 ? deltaY * 16 : deltaMode === 2 ? deltaY * 400 : deltaY;
  const capped = Math.max(-25, Math.min(25, px));
  return Math.exp(-capped * 0.009);
}

/** The new `scrollLeft` that keeps the bar under `anchorX` where it is.
 *
 *  `anchorX` is measured from the scroll container's left edge, `origin` is where bar 0 sits in
 *  the scrolled content (the track heads plus the lanes' left inset). Left of bar 0 (over the
 *  heads) bar 0 is the anchor, and it does not move with the zoom. The browser clamps the result
 *  to the scroll range. */
export function anchoredScroll(scrollLeft: number, anchorX: number, origin: number, from: number, to: number): number {
  const bar = (scrollLeft + anchorX - origin) / from;
  if (!(bar > 0)) return scrollLeft;
  return Math.max(0, origin + bar * to - anchorX);
}

/** The zoom that shows all `bars` in a lane area `width` pixels wide (after the left inset). */
export function fitZoom(width: number, bars: number): number {
  if (bars <= 0 || width <= 0) return clampZoom(20);
  return clampZoom(Math.floor((width / bars) * 2) / 2);
}
