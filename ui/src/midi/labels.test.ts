import { describe, expect, it } from "vitest";
import { actionLabel, areaOf, lastSeen, targetLabel } from "./labels";

const tracks = ["gitarre", "voice", "chor"];

describe("how an address reads in the binding list", () => {
  it("names a track by the name the musician gave it", () => {
    expect(targetLabel("track.3.record", tracks)).toBe("chor · Record");
    expect(targetLabel("track.2.overdub", tracks)).toBe("voice · Overdub");
    expect(targetLabel("track.9.play", tracks)).toBe("Track 9 · Play");
  });

  it("uses the words of the buttons for transport and global targets", () => {
    expect(targetLabel("transport.next", tracks)).toBe("Release");
    expect(targetLabel("transport.goto.3", tracks)).toBe("Go to section 3");
    expect(targetLabel("transport.toggle", tracks)).toBe("Start/stop transport");
    expect(targetLabel("global.bus.monitor.gain", tracks)).toBe("Headphone volume");
  });

  it("falls back to the backend's label without its track prefix", () => {
    expect(actionLabel("track.1.fx.reverb.mix", "Track 1: Reverb mix")).toBe("Reverb mix");
    expect(targetLabel("track.1.fx.reverb.mix", tracks, "Track 1: Reverb mix")).toBe("gitarre · Reverb mix");
  });

  it("groups by area: transport, global, then each track in order", () => {
    const keys = ["track.10.play", "global.click", "track.2.record", "transport.next"].map((a) => areaOf(a, tracks).key);
    expect([...keys].sort()).toEqual(["0transport", "1global", "2track.001", "2track.009"]);
    expect(areaOf("track.3.monitor", tracks).title).toBe("Tracks · 3 chor");
  });

  it("says last seen without a date when it was today", () => {
    expect(lastSeen(null)).toBeNull();
    expect(lastSeen(Date.now())).toMatch(/^last seen \d\d:\d\d$/);
    expect(lastSeen(Date.now() - 3 * 86_400_000)).toMatch(/^last seen .+, \d\d:\d\d$/);
  });
});
