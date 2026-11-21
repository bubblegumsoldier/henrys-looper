// The sheet in the UI: where it stands for every phase of the runner (section-aware, looping,
// Release, count-in, rest), what NOW and NEXT say, the layout memory and the static page. The
// sheets here are hand-built the way the engine compiles them (`score/sheet.rs`); the texts are
// invented placeholders.

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  (globalThis as unknown as { window: unknown }).window ??= globalThis;
});
import { renderToStaticMarkup } from "react-dom/server";
import type { CompiledScore, CompiledSheet, LooperStatus, SheetItem } from "../types";
import { IDLE_STATUS } from "../types";
import { beatsLeft, inBarsText, mapSection, runPoint, sheetAt, sheetState, type RunPoint } from "./position";
import { SheetView, chipColumns } from "./SheetView";
import { defaultLayout, sanitize } from "./layout";
import { pickSheet, sameSections } from "./liveSheet";
import { sheetIssues, sheetKeyLine } from "./SheetEditor";

// intro: 2 bars, autorelease · verse: 2-bar loop until Release, 4 bars of text · outro: 2 bars.
const ITEMS: SheetItem[] = [
  { kind: "part", name: "Intro", at: 0, line: 1 },
  { kind: "direction", text: "REC theme · 2 bars", rec: true, bars: 0, start: 0, line: 2 },
  { kind: "line", text: "", chords: [{ name: "Am", col: 0 }, { name: "C/G", col: 5 }, { name: "F", col: 11 }, { name: "C", col: 15 }], bars: 2, start: 0, line: 3 },
  { kind: "part", name: "Verse", section: "verse", at: 2, line: 4 },
  { kind: "direction", text: "Sing · theme plays", rec: false, bars: 0, start: 2, line: 5 },
  { kind: "line", text: "Paper boats on Sunday", chords: [{ name: "F", col: 0 }, { name: "C", col: 15 }], bars: 2, start: 2, line: 6 },
  { kind: "line", text: "drifting down the road", chords: [], bars: 2, start: 4, line: 7 },
  { kind: "part", name: "Outro", section: "outro", at: 6, line: 8 },
  { kind: "direction", text: "REC lead", rec: true, bars: 0, start: 6, line: 9 },
  { kind: "line", text: "la la", chords: [], bars: 2, start: 6, line: 10 },
];
const SHEET: CompiledSheet = {
  items: ITEMS,
  total: 8,
  spans: [
    { start: 0, end: 2 },
    { start: 2, end: 6 },
    { start: 6, end: 8 },
  ],
};
const SECTIONS = [
  { bars: 2, autorelease: true },
  { bars: 2, autorelease: false },
  { bars: 2, autorelease: true },
];

function point(p: Partial<RunPoint>): RunPoint {
  return { phase: "running", section: 0, bar: 1, pass: 1, beat: 1, inBeat: 0, beatsPerBar: 4, bpm: 120, armed: null, ...p };
}

describe("where the sheet stands", () => {
  it("rests at the top while ready, stopped or finished", () => {
    for (const phase of ["none", "idle", "stopped", "finished"] as const) {
      expect(sheetAt(SHEET, SECTIONS, point({ phase }))).toBeNull();
    }
    const st = sheetState(SHEET, null);
    expect(st.current).toBe(-1);
    expect(st.pastBefore).toBe(0);
    expect(st.next).toEqual({ text: "REC theme · 2 bars", rec: true, inBars: null });
  });

  it("counts in at the top and names the first direction", () => {
    // Two bars of count-in, one and a half to go.
    const p = point({ phase: "count_in", section: null, bar: 0, armed: { section: 0, beatsLeft: 6, countIn: true } });
    const at = sheetAt(SHEET, SECTIONS, p)!;
    expect(at.bar).toBe(0);
    expect(at.wait).toBe(1.5);
    const st = sheetState(SHEET, at);
    expect(st.current).toBe(-1);
    expect(st.next).toEqual({ text: "REC theme · 2 bars", rec: true, inBars: 1.5 });
    expect(inBarsText(st.next!.inBars)).toBe("in 2 bars");
  });

  it("leaves the count-in on the bar line, between two status events", () => {
    const p = point({ phase: "count_in", section: null, bar: 0, armed: { section: 0, beatsLeft: 1, countIn: true } });
    // 120 BPM: one beat is half a second; 0.6 s later the song has begun.
    const at = sheetAt(SHEET, SECTIONS, p, 0.6)!;
    expect(at.section).toBe(0);
    expect(at.bar).toBeCloseTo(0.05);
    expect(sheetState(SHEET, at).current).toBe(2);
  });

  it("runs linearly through a section that moves on by itself, chord by chord", () => {
    const at = sheetAt(SHEET, SECTIONS, point({ bar: 2, beat: 1 }))!;
    expect(at.bar).toBe(1);
    const st = sheetState(SHEET, at);
    expect(st.current).toBe(2);
    expect(st.progress).toBe(0.5);
    expect(st.chord).toBe(2);
    expect(st.now).toEqual({ text: "REC theme · 2 bars", rec: true });
    expect(st.active).toEqual([1]);
  });

  it("extrapolates between status events at the tempo", () => {
    // Bar 1, beat 4, half way through it; 0.25 s at 120 BPM is another half beat: bar 2 begins.
    const at = sheetAt(SHEET, SECTIONS, point({ bar: 1, beat: 4, inBeat: 0.5 }), 0.25)!;
    expect(at.bar).toBeCloseTo(1);
  });

  it("wraps around the text of a section that loops until Release", () => {
    const span = (pass: number, bar: number) => sheetAt(SHEET, SECTIONS, point({ section: 1, pass, bar }))!.bar;
    // 4 bars of text over a 2-bar loop: the text runs on over two passes, then starts again.
    expect(span(1, 1)).toBe(2);
    expect(span(1, 2)).toBe(3);
    expect(span(2, 1)).toBe(4);
    expect(span(2, 2)).toBe(5);
    expect(span(3, 1)).toBe(2);
    const st = sheetState(SHEET, sheetAt(SHEET, SECTIONS, point({ section: 1, pass: 3, bar: 1 })));
    expect(st.current).toBe(5);
    expect(st.pastBefore).toBe(5);
    expect(st.now?.text).toBe("Sing · theme plays");
  });

  it("wraps every pass when the text is as long as the loop", () => {
    const sheet: CompiledSheet = { ...SHEET, spans: [SHEET.spans[0], { start: 2, end: 4 }, { start: 4, end: 8 }] };
    expect(mapSection(sheet, SECTIONS, 1, 0.5)).toBe(2.5);
    expect(mapSection(sheet, SECTIONS, 1, 2.5)).toBe(2.5);
    expect(mapSection(sheet, SECTIONS, 1, 7.25)).toBe(3.25);
  });

  it("holds the last line when the text is shorter than its section", () => {
    const sections = [{ bars: 2, autorelease: true }, { bars: 8, autorelease: true }, { bars: 2, autorelease: true }];
    const at = mapSection(SHEET, sections, 1, 6.5);
    expect(at).toBeLessThan(6);
    expect(sheetState(SHEET, sheetAt(SHEET, sections, point({ section: 1, bar: 7 })))!.current).toBe(6);
  });

  it("says the next section's direction comes after Release, and when once it is armed", () => {
    const loose = sheetState(SHEET, sheetAt(SHEET, SECTIONS, point({ section: 1, pass: 2, bar: 2 })));
    expect(loose.current).toBe(6);
    expect(loose.next).toEqual({ text: "REC lead", rec: true, inBars: null });
    expect(inBarsText(null)).toBe("after Release");
    // Released: the change is one bar and two beats away.
    const armed = { section: 2, beatsLeft: beatsLeft(1, 2, 4, 0), countIn: false };
    const st = sheetState(SHEET, sheetAt(SHEET, SECTIONS, point({ section: 1, pass: 2, bar: 1, armed })));
    expect(st.next?.inBars).toBe(1.5);
  });

  it("jumps to the next section's text exactly at the change", () => {
    const armed = { section: 2, beatsLeft: 2, countIn: false };
    const p = point({ section: 1, pass: 5, bar: 2, beat: 3, armed });
    // Before the change: the loop's text, wrapped (pass 5 = third bar of the text... and so on).
    expect(sheetAt(SHEET, SECTIONS, p, 0.9)!.section).toBe(1);
    // 1.1 s later at 120 BPM: 2.2 beats - the change was 2 beats away.
    const at = sheetAt(SHEET, SECTIONS, p, 1.1)!;
    expect(at.section).toBe(2);
    expect(at.bar).toBeCloseTo(6.05);
    expect(sheetState(SHEET, at).now).toEqual({ text: "REC lead", rec: true });
  });

  it("goes to rest at the end of the score", () => {
    const armed = { section: null, beatsLeft: 1, countIn: false };
    expect(sheetAt(SHEET, SECTIONS, point({ section: 2, bar: 2, beat: 4, armed }), 0.6)).toBeNull();
  });

  it("starts again from the top after Back to start", () => {
    const running = sheetState(SHEET, sheetAt(SHEET, SECTIONS, point({ section: 2, bar: 2 })));
    expect(running.pastBefore).toBeGreaterThan(0);
    const back = sheetState(SHEET, sheetAt(SHEET, SECTIONS, point({ phase: "idle", section: null })));
    expect(back.current).toBe(-1);
    expect(back.pastBefore).toBe(0);
  });

  it("reads the position from a status event", () => {
    const s: LooperStatus = {
      ...IDLE_STATUS,
      bpm: 90,
      beats_per_bar: 3,
      samples_per_beat: 1000,
      beat_offset: 250,
      score: {
        title: "t",
        phase: "running",
        phase_label: "",
        section: 1,
        section_id: "verse",
        section_count: 3,
        bars_total: 2,
        bar: 2,
        beat: 3,
        pass: 4,
        tracks: [],
        armed: { label: "outro", section: 2, bars: 1, beats: 1, count_in: false, text: "" },
        count_in: 2,
      },
    };
    expect(runPoint(s)).toEqual({
      phase: "running",
      section: 1,
      bar: 2,
      pass: 4,
      beat: 3,
      inBeat: 0.25,
      beatsPerBar: 3,
      bpm: 90,
      armed: { section: 2, beatsLeft: 3.75, countIn: false },
    });
    expect(runPoint({ ...IDLE_STATUS, score: null }).phase).toBe("none");
  });
});

describe("the page", () => {
  it("puts chips on their columns and pushes one right only where they would overlap", () => {
    expect(chipColumns([{ name: "Am", col: 0 }, { name: "C/G", col: 14 }])).toEqual([0, 14]);
    expect(chipColumns([{ name: "Am7", col: 0 }, { name: "G", col: 2 }])).toEqual([0, 5]);
  });

  it("draws parts, directions and lines with chords at exact monospace columns", () => {
    const html = renderToStaticMarkup(<SheetView sheet={SHEET} />);
    expect(html).toContain('class="sh-part" data-i="3"><span>Verse</span><span class="sh-anchor">@verse</span>');
    expect(html).toContain('class="sh-dir rec" data-i="1">REC theme · 2 bars');
    expect(html).toContain('<span class="sh-chip" style="left:15ch"><b>C</b></span>');
    expect(html).toContain('<div class="sh-tx">Paper boats on Sunday</div>');
  });
});

describe("layout and source", () => {
  it("starts closed on a narrow window and open on a wide one", () => {
    expect(defaultLayout(1024).open).toBe(false);
    expect(defaultLayout(1600).open).toBe(true);
    expect(defaultLayout(1600).width).toBe(0.4);
  });

  it("keeps stored values in range", () => {
    expect(sanitize({ open: true, width: 3, fontSize: 200 }, defaultLayout(1024))).toEqual({ open: true, width: 0.7, fontSize: 40 });
    expect(sanitize("nonsense", defaultLayout(1024))).toEqual(defaultLayout(1024));
  });

  it("shows the open document's sheet while its sections match the loaded score", () => {
    const score = (bars: number, sheet?: CompiledSheet): CompiledScore =>
      ({
        title: "t",
        bpm: 100,
        time_signature: "4/4",
        beats_per_bar: 4,
        beat_unit: 4,
        tracks: [],
        midi: {},
        sections: [{ id: "a", index: 0, bars, autorelease: true, quantize: "bar", tracks: {} }],
        sheet,
      }) as CompiledScore;
    const old = { ...SHEET, total: 1 };
    const loaded = score(2, old);
    expect(sameSections(loaded, score(2))).toBe(true);
    expect(pickSheet(loaded, score(2, SHEET)).sheet).toBe(SHEET);
    expect(pickSheet(loaded, score(4, SHEET)).sheet).toBe(old);
    expect(pickSheet(null, score(2, SHEET))).toEqual({ sheet: null, sections: null });
  });

  it("finds the sheet's issues and its place in the text", () => {
    const issue = (field: string) => ({ line: 9, column: 3, message: "m", suggestion: null, at: { kind: "score" as const, field } });
    expect(sheetIssues([issue("sheet"), issue("bpm")])).toHaveLength(1);
    expect(sheetKeyLine("bpm: 100\nsections: []\nsheet: |\n  # Intro\n")).toBe(3);
    expect(sheetKeyLine("bpm: 100\n")).toBeNull();
  });
});
