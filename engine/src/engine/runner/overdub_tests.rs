//! Overdubs and the loop they layer onto, and `{state: play, monitor: true}` - on the real engine
//! core (`Sim`), read back from the engine's own status block by block.
//!
//! * An overdub on a loop that comes back (silent in the section before) plays that loop from the
//!   section start - the owner's chorus_3 of 2026-11-21 14:57, where it stayed silent for the
//!   whole take ("the backing vocals do not play from the start").
//! * An overdub stays inside its section: no pass of the loop starting inside it - nothing is
//!   scheduled; a pass that does not fit - it ends with the section.
//! * `{state: play, monitor: true}` plays the loop without a gap and hears the input on top, and
//!   records nothing.

use super::*;
use crate::engine::command::TrackStatus;
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Sim, SimSpec, TrackSpec, mono};
use crate::engine::track::TrackState;
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;

fn fingerprint(k: u64) -> f32 {
    ((k % 30_011) as f32) / 30_011.0 - 0.5
}

/// One run of a score with a single track on input 1, every block's status of that track kept.
struct Run {
    sim: Sim,
    runner: Runner,
    first_bar: u64,
    offsets: Vec<u64>,
    waits: Vec<bool>,
    released: Vec<bool>,
    /// (output position, status of track 0) after every block.
    seen: Vec<(u64, TrackStatus)>,
}

impl Run {
    fn new(yaml: &str) -> Self {
        let score = compile_score(yaml).expect("die Partitur uebersetzt");
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
        let mut offsets = vec![0u64];
        for s in &score.sections {
            offsets.push(offsets.last().unwrap() + u64::from(s.bars));
        }
        let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
        let mut sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                block: BLOCK as usize,
                tracks: vec![TrackSpec::on(0)],
                input_channels: 1,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 6,
                ..Default::default()
            },
            mono(fingerprint),
        );
        for cmd in setup_commands(&score) {
            sim.send(cmd);
        }
        let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        runner.start(0);
        let first_bar = runner.armed_at().expect("armiert").1;
        let released = vec![false; waits.len()];
        Self { sim, runner, first_bar, offsets, waits, released, seen: Vec::new() }
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    fn section(&self, index: usize) -> u64 {
        self.bar(self.offsets[index])
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.runner.tick(self.sim.pos());
            if let Some(index) = self.runner.view(self.sim.pos()).section {
                let last = self.bar(self.offsets[index + 1].saturating_sub(1));
                if self.waits[index] && !self.released[index] && self.sim.pos() >= last {
                    self.released[index] = true;
                    self.runner.next(self.sim.pos());
                }
            }
            for cmd in self.runner.take_commands() {
                self.sim.send(cmd);
            }
            self.sim.step();
            let status = self.sim.latest_status().expect("Status").tracks()[0];
            self.seen.push((self.sim.pos(), status));
        }
    }

    /// The track's status at the block that ends at or after `pos`.
    fn at(&self, pos: u64) -> TrackStatus {
        self.seen.iter().find(|(p, _)| *p >= pos).expect("so weit gelaufen").1
    }

    /// Every status in `from..to`.
    fn during(&self, from: u64, to: u64) -> impl Iterator<Item = &TrackStatus> {
        self.seen.iter().filter(move |(p, _)| (from..to).contains(p)).map(|(_, s)| s)
    }
}

/// The owner's run, reduced to the track that matters: a ten-bar loop recorded in `chorus_2`,
/// silent in the bridge, overdubbed in a four-bar `chorus_3`, then `chorus_3_all`.
fn owners(chorus_3_all: &str) -> String {
    format!(
        "bpm: 94\ntracks:\n  bv: {{input: 1}}\nsections:\n  - id: chorus_2\n    bars: 10\n    autorelease: true\n    tracks:\n      bv: record\n  - id: bridge\n    bars: 8\n    autorelease: true\n  - id: chorus_3\n    bars: 4\n    autorelease: true\n    tracks:\n      bv: overdub\n  - id: chorus_3_all\n    bars: 6\n    autorelease: true\n    tracks:\n      bv: {chorus_3_all}\n  - id: end\n    bars: 1\n"
    )
}

#[test]
fn an_overdub_on_a_loop_that_comes_back_plays_it_from_the_section_start() {
    let mut run = Run::new(&owners("play"));
    let (c3, c3_all) = (run.section(2), run.section(3));
    run.run_to(run.section(4) + 2_000);
    // Silent in the bridge, as written.
    assert!(run.during(run.section(1) + 2_000, c3).all(|s| !s.playing));
    // From the first block of chorus_3 on, the loop plays - through the whole overdub, without a
    // block of silence, and on into chorus_3_all.
    let gaps: Vec<u64> = run
        .seen
        .iter()
        .filter(|(p, s)| (c3 + u64::from(BLOCK)..run.section(4)).contains(p) && !s.playing)
        .map(|(p, _)| *p)
        .collect();
    assert!(gaps.is_empty(), "stumm ab {:?}", gaps.first().map(|p| p - c3));
    assert_eq!(run.at(c3 + 1_000).state, TrackState::Overdub);
    // The layer is one four-bar pass on the re-anchored ten-bar loop.
    assert_eq!(run.at(c3_all + 2_000).layers, 2);
    assert_eq!(run.at(c3_all + 2_000).origin, c3);
}

#[test]
fn play_with_monitor_plays_the_loop_hears_the_input_and_records_nothing() {
    let mut run = Run::new(&owners("{state: play, monitor: true}"));
    let (c3_all, end) = (run.section(3), run.section(4));
    run.run_to(end + 4_000);
    // Monitoring is a control-thread switch at the boundary: on within a block or two of it.
    let monitored = run.during(c3_all + 4 * u64::from(BLOCK), end).all(|s| s.monitor && s.playing);
    assert!(monitored, "Loop und Mithoeren in chorus_3_all");
    let layers = run.at(c3_all + 4_000).layers;
    assert!(run.during(c3_all + 4_000, end).all(|s| s.layers == layers && s.state == TrackState::Playing));
    assert!(!run.at(end + 4 * u64::from(BLOCK)).monitor, "aus mit der Sektion");
    // Gap-free across the boundary into it.
    assert!(run.during(run.section(2) + 1_000, end).all(|s| s.playing));
}

#[test]
fn stop_all_closes_a_play_with_monitor() {
    let mut run = Run::new(&owners("{state: play, monitor: true}"));
    let c3_all = run.section(3);
    run.run_to(c3_all + 20_000);
    assert!(run.at(c3_all + 20_000).monitor);
    run.runner.stop_all(run.sim.pos());
    for cmd in run.runner.take_commands() {
        run.sim.send(cmd);
    }
    let from = run.sim.pos() + run.runner.scheduler().guard;
    run.run_to(from + 48_000);
    assert!(run.during(from, from + 48_000).all(|s| !s.monitor && !s.playing));
}

/// A loop of four bars, a one-bar section that waits for Release, then a section in which the
/// loop's next pass begins only after `bars` bars - see the two tests below.
fn after_a_wait(bars: u32) -> String {
    format!(
        "bpm: 120\ntracks:\n  a: {{input: 1}}\nsections:\n  - id: rec\n    bars: 4\n    autorelease: true\n    tracks:\n      a: record\n  - id: wait\n    bars: 1\n    autorelease: false\n    tracks:\n      a: play\n  - id: short\n    bars: {bars}\n    autorelease: true\n    tracks:\n      a: overdub\n  - id: after\n    bars: 4\n    autorelease: true\n    tracks:\n      a: play\n"
    )
}

#[test]
fn an_overdub_with_no_pass_inside_its_section_schedules_nothing() {
    let mut run = Run::new(&after_a_wait(2));
    let (short, after) = (run.section(2), run.section(3));
    run.run_to(run.section(4) + 2_000);
    assert!(run.during(short, run.section(4)).all(|s| s.state == TrackState::Playing && s.layers == 1));
    let skipped = run.runner.skipped().to_vec();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!((skipped[0].section, skipped[0].loop_bars, skipped[0].next_pass), (2, 4, 3));
    assert!(run.at(after + 2_000).playing);
}

#[test]
fn a_pass_that_does_not_fit_ends_with_its_section() {
    // Short is four bars and the loop's next pass begins in its bar 4: one bar of overdub.
    let mut run = Run::new(&after_a_wait(4));
    let (short, after) = (run.section(2), run.section(3));
    run.run_to(run.section(4) + 2_000);
    let into = |s: &TrackStatus| matches!(s.state, TrackState::Overdub | TrackState::Recording);
    assert!(run.during(short, short + 2 * run.bar(1).saturating_sub(run.bar(0))).all(|s| !into(s)));
    assert_eq!(run.at(after - 4_000).state, TrackState::Overdub);
    // Nothing of it survives into `after` (an overdub's tail is the roundtrip, under a block).
    assert!(run.during(after + 2_000, run.section(4)).all(|s| !into(s)), "laeuft in 'after' weiter");
    assert_eq!(run.at(after + 2_000).layers, 2);
    assert!(run.runner.skipped().is_empty());
}

/// What `score --check` uses: the dry run names the owner's chorus_3_all.
#[test]
fn the_dry_run_finds_the_owners_overdub() {
    let source = crate::score::read::read_score(&after_a_wait(2)).expect("liest");
    let compiled = crate::score::compile::compile(&source);
    let skipped = dry_run(&compiled);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].track, "a");
    assert_eq!(skipped[0].state, ScoreState::Overdub);
}

/// `skip_end: 1.5` on a two-bar take at 120 BPM: the last 36 000 frames of the take are silent,
/// with the loop's 10 ms edge fade into the cut, and everything before is bit for bit the take without it -
/// compared with the same run without the option, so the cut sits on the very sample the
/// latency-compensated take puts it. The loop keeps its length; the input stays monitored.
#[test]
fn skip_end_leaves_the_last_beats_of_a_take_silent_and_keeps_its_length() {
    let yaml = |clip: &str| {
        format!(
            "bpm: 120\ntracks:\n  a: {{input: 1}}\nsections:\n  - id: rec\n    bars: 2\n    autorelease: true\n    tracks:\n      a: {clip}\n  - id: after\n    bars: 2\n    autorelease: true\n    tracks:\n      a: play\n"
        )
    };
    let mut plain = Run::new(&yaml("record"));
    let mut cut = Run::new(&yaml("{state: record, skip_end: 1.5}"));
    for run in [&mut plain, &mut cut] {
        let to = run.section(2);
        run.run_to(to);
    }
    assert!(cut.at(cut.section(1) - 10_000).monitor, "im ausgelassenen Schlag zu hoeren");
    let (a, b) = (plain.sim.core.track(0), cut.sim.core.track(0));
    assert_eq!(a.loop_len(), b.loop_len());
    let len = a.loop_len();
    let (ca, cb) = (a.layer(0).unwrap().content(len), b.layer(0).unwrap().content(len));
    let from = len - 36_000;
    let fade = crate::engine::track::edge_frames(RATE);
    for i in 0..len {
        let want = if i >= from {
            0.0
        } else if i + fade > from {
            ca[i as usize] * crate::engine::track::fade_out(fade - (from - i), fade)
        } else {
            ca[i as usize]
        };
        assert!((cb[i as usize] - want).abs() < 1e-6, "Frame {i}: {} statt {want}", cb[i as usize]);
    }
    assert!(ca[(from + 10) as usize] != 0.0, "ohne die Option ist dort Klang");
}
