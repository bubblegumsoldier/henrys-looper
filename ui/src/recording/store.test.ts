import { describe, expect, it } from "vitest";
import { bytes, clock, dismissFinished, pushRecording, recordingState, type FinishedRecording } from "./store";

const done: FinishedRecording = {
  name: "2026-11-15 20.31.07 Song",
  folder: "/x/2026-11-15 20.31.07 Song",
  kept: true,
  reason: null,
  seconds: 61,
  bytes: 100,
  overflows: 0,
  silent_frames: 0,
  error: null,
  text: "Recording saved",
};

const running = { name: "a", folder: "/a", seconds: 1, stopping: false, mode: "free" as const, overflows: 0 };

describe("recording store", () => {
  it("prints times and sizes the way the list shows them", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(65.7)).toBe("1:05");
    expect(clock(3723)).toBe("1:02:03");
    expect(bytes(5 * 1_048_576)).toBe("5 MB");
    expect(bytes(1536 * 1_048_576)).toBe("1.5 GB");
  });

  it("keeps the finished run past an idle event, until it is dismissed", () => {
    pushRecording({ active: null, finished: done });
    pushRecording({ active: null, finished: null });
    expect(recordingState().finished?.name).toBe(done.name);
    dismissFinished();
    expect(recordingState().finished).toBeNull();
  });

  it("forgets the finished run when the next one starts", () => {
    pushRecording({ active: null, finished: done });
    pushRecording({ active: running, finished: null });
    expect(recordingState().active?.name).toBe("a");
    expect(recordingState().finished).toBeNull();
  });
});
