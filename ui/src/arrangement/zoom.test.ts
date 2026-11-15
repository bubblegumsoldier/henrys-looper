import { describe, expect, it } from "vitest";
import { ZOOM_MAX, ZOOM_MIN, anchoredScroll, fitZoom, snapZoom, wheelZoomFactor } from "./zoom";

describe("zoom", () => {
  it("keeps the bar under the pointer in place", () => {
    const origin = 252 + 14;
    const scroll = 300;
    const x = 500; // pointer, from the container's left edge
    const bar = (scroll + x - origin) / 20;
    const next = anchoredScroll(scroll, x, origin, 20, 40);
    expect((next + x - origin) / 40).toBeCloseTo(bar, 9);
    // zooming back lands where it started
    expect(anchoredScroll(next, x, origin, 40, 20)).toBeCloseTo(scroll, 9);
  });

  it("keeps bar 0 still when the pointer is over the track heads", () => {
    expect(anchoredScroll(0, 100, 266, 20, 60)).toBe(0);
    expect(anchoredScroll(10, 200, 266, 20, 60)).toBe(10);
  });

  it("never asks for a negative scroll", () => {
    expect(anchoredScroll(0, 300, 266, 40, 6)).toBeGreaterThanOrEqual(0);
  });

  it("snaps to device pixels and stays in range", () => {
    expect(snapZoom(20.3, 1)).toBe(20);
    expect(snapZoom(20.3, 2)).toBe(20.5);
    expect(snapZoom(2, 2)).toBe(ZOOM_MIN);
    expect(snapZoom(500, 2)).toBe(ZOOM_MAX);
  });

  it("scales with the wheel delta, capped per event", () => {
    expect(wheelZoomFactor(0)).toBe(1);
    expect(wheelZoomFactor(-4)).toBeGreaterThan(1);
    expect(wheelZoomFactor(4)).toBeLessThan(1);
    expect(wheelZoomFactor(-4) * wheelZoomFactor(4)).toBeCloseTo(1, 9);
    // a small pinch step is gentle, a mouse notch about one button step
    expect(wheelZoomFactor(-2)).toBeLessThan(1.03);
    expect(wheelZoomFactor(-120)).toBeCloseTo(wheelZoomFactor(-25), 9);
    expect(wheelZoomFactor(-120)).toBeLessThan(1.3);
    expect(wheelZoomFactor(-3, 1)).toBe(wheelZoomFactor(-48));
  });

  it("fits the whole score", () => {
    expect(fitZoom(800, 40)).toBe(20);
    expect(fitZoom(800, 1000)).toBe(ZOOM_MIN);
    expect(fitZoom(800, 4)).toBe(ZOOM_MAX);
    expect(fitZoom(805, 40)).toBe(20);
  });
});
