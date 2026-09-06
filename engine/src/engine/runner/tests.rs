//! Offline proof that a written score plays itself.
//!
//! Nothing here opens an audio device. Two levels of evidence:
//!
//! * **Command level** - the exact sequence of [`Command`]s with their sample positions, checked
//!   against numbers computed from the timeline rather than from the runner. This is where "no
//!   superfluous command" and "exactly `bars` bars" are decided.
//! * **Audio level** - the same run driven through `Sim`, which is the real `EngineCore` fed with
//!   synthetic buffers. That is what proves the commands really produce loops of the length the
//!   score asked for, aligned to the sample.

use std::time::Duration;

use super::*;
use crate::engine::command::{Command, Refusal};
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Sim, SimSpec, TrackSpec, mono};
use crate::engine::timeline::TimeSignature;
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;
/// Roundtrip measured in phase 0, the value `SimSpec` defaults to.
const R: u64 = 827;

/// Henry's own score - the one the definition of done names.
const HENRY: &str = include_str!("../../../../examples/henry-3-4.rust.yaml");

fn runner_for(yaml: &str) -> Runner {
    let score = compile_score(yaml).expect("die Partitur uebersetzt");
    let timeline = Timeline::new(RATE, score.bpm, score.signature());
    Runner::new(score, timeline, BLOCK).expect("Runner")
}

/// Start a runner and walk it forward, collecting every command it produces.
///
/// `tick` is called on a coarse grid on purpose: the runner must not depend on being asked at the
/// exact sample a boundary falls on, only on being asked often enough - which is all the control
/// loop of a front end can promise.
struct Drive {
    runner: Runner,
    commands: Vec<Command>,
    pos: u64,
    /// Bar the first section begins on. Everything else is counted from here, because where the
    /// count-in ends depends on the head start and is not a number a test should hard-code.
    first_bar: u64,
}

impl Drive {
    fn new(yaml: &str) -> Self {
        let mut runner = runner_for(yaml);
        runner.start(0);
        let commands = runner.take_commands();
        let first_bar = runner.armed_at().expect("die erste Sektion ist armiert").1;
        Self {
            runner,
            commands,
            pos: 0,
            first_bar,
        }
    }

    fn timeline(&self) -> Timeline {
        self.runner.scheduler().timeline
    }

    /// First sample of the section that begins `bars` bars after the score's first bar.
    fn at_bar(&self, bars: u64) -> u64 {
        self.timeline().bar_start(self.first_bar + bars)
    }

    /// Advance to `pos`, ticking every 2000 samples - about 40 ms, twice as coarse as the control
    /// loop of the CLI.
    fn to(&mut self, pos: u64) {
        while self.pos < pos {
            self.pos = (self.pos + 2_000).min(pos);
            self.runner.tick(self.pos);
            self.commands.extend(self.runner.take_commands());
        }
    }

    fn release(&mut self) -> String {
        let message = self.runner.next(self.pos);
        self.commands.extend(self.runner.take_commands());
        message
    }

    fn goto(&mut self, index: usize) -> String {
        let message = self.runner.goto(index, self.pos);
        self.commands.extend(self.runner.take_commands());
        message
    }

    fn stop_all(&mut self) -> String {
        let message = self.runner.stop_all(self.pos);
        self.commands.extend(self.runner.take_commands());
        message
    }

    fn section(&self) -> Option<usize> {
        self.runner.view(self.pos).section
    }

    /// Every **timed** command addressing one track, in order. Monitoring carries no timestamp and
    /// is checked separately, by [`Drive::monitor_of`].
    fn of(&self, track: usize) -> Vec<Command> {
        self.commands
            .iter()
            .copied()
            .filter(|c| c.track() == Some(track) && c.at().is_some())
            .collect()
    }

    fn monitor_of(&self, track: usize) -> Vec<bool> {
        self.commands
            .iter()
            .filter_map(|c| match c {
                Command::SetMonitor { track: t, on } if *t == track => Some(*on),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The count-in
// ---------------------------------------------------------------------------------------------

/// It runs before the first section, it is one whole bar, and it happens exactly once - no later
/// section gets one.
#[test]
fn the_count_in_runs_once_before_the_first_section() {
    let mut drive = Drive::new(HENRY);
    let timeline = drive.timeline();
    let guard = drive.runner.scheduler().guard;

    assert_eq!(drive.runner.phase(), Phase::CountIn);
    let armed = drive.runner.view(0).armed.expect("die erste Sektion ist armiert");
    assert!(armed.count_in, "der Einzaehler ist als solcher erkennbar");
    assert!(armed.text().contains("Einzaehler"), "{}", armed.text());

    // Definition: the count-in occupies the first bar the command queue can still be told about in
    // time, and the section starts one bar after that. So the lead is a full bar plus the rest of
    // the bar the button was pressed in - never less than a bar.
    let anchor = timeline.bar_start_at_or_after(guard);
    let start = drive.at_bar(0);
    assert_eq!(start, timeline.bar_start(timeline.bar_index_at(anchor) + 1));
    assert!(start - 0 >= timeline.span_bars(0, 1), "mindestens ein ganzer Takt");

    // The first take starts with the section, not before it.
    let first_record = drive
        .commands
        .iter()
        .find_map(|c| match c {
            Command::StartRecord { at, .. } => Some(*at),
            _ => None,
        })
        .expect("die erste Sektion nimmt auf");
    assert_eq!(first_record, start);

    drive.to(start - 1);
    assert_eq!(drive.runner.phase(), Phase::CountIn);
    drive.to(start);
    assert_eq!(drive.runner.phase(), Phase::Running);
    assert_eq!(drive.section(), Some(0));

    // From here on no view ever reports a count-in again, however far the score runs.
    for section in 1..4u64 {
        drive.to(drive.at_bar(8 * section));
        let view = drive.runner.view(drive.pos);
        assert_eq!(view.section, Some(section as usize));
        assert!(
            view.armed.as_ref().is_none_or(|a| !a.count_in),
            "Sektion {section} bekommt keinen Einzaehler"
        );
    }
}

/// A longer count-in is a flag, and the lead is never shorter than what was asked for even when the
/// button is pressed in the middle of a bar.
#[test]
fn the_count_in_length_is_configurable_and_never_undershoots() {
    let mut runner = runner_for(HENRY).with_count_in(2);
    let timeline = runner.scheduler().timeline;
    let est = timeline.bar_start(0) + timeline.samples_per_beat() as u64;
    runner.start(est);
    let (start, _) = runner.armed_at().expect("armiert");
    let anchor = timeline.bar_start_at_or_after(est + runner.scheduler().guard);
    assert_eq!(start, timeline.bar_start(timeline.bar_index_at(anchor) + 2));
    assert!(
        start - est > timeline.span_bars(0, 2),
        "zwei volle Takte plus der Rest des angefangenen"
    );
}

// ---------------------------------------------------------------------------------------------
// Autorelease
// ---------------------------------------------------------------------------------------------

/// `autorelease: true` changes on its own after exactly `bars` bars, to the sample.
#[test]
fn autorelease_changes_after_exactly_the_written_number_of_bars() {
    let mut drive = Drive::new(HENRY);
    let timeline = drive.timeline();
    let starts: Vec<u64> = (0..4).map(|i| drive.at_bar(8 * i)).collect();

    for (index, &start) in starts.iter().enumerate() {
        drive.to(start - 1);
        assert_eq!(
            drive.section(),
            index.checked_sub(1),
            "eine Sample vor der Grenze laeuft noch die vorige Sektion"
        );
        drive.to(start);
        assert_eq!(drive.section(), Some(index), "und ab der Grenze die neue");
    }

    // The guitar take covers section 0 exactly, and its length is the timeline's own eight bars.
    assert_eq!(
        drive.of(0),
        vec![
            Command::StartRecord { track: 0, at: starts[0] },
            Command::StopRecord { track: 0, at: starts[1] },
            Command::StartPlay { track: 0, at: starts[1] },
        ],
    );
    assert_eq!(starts[1] - starts[0], timeline.span_bars(drive.first_bar, 8));
}

/// The property the design turns on: a track that stands on `play` across several sections gets no
/// command at all.
#[test]
fn a_track_that_keeps_playing_gets_no_further_command() {
    let mut drive = Drive::new(HENRY);
    // Sections 1, 2 and 3 all have the guitar on `play`; section 3 is where autorelease stops.
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(drive.section(), Some(3));
    assert_eq!(
        drive.of(0).len(),
        3,
        "nur der eine Take, kein StartPlay je Sektionsgrenze: {:?}",
        drive.of(0)
    );
}

/// A track that stands on `stop` twice in a row is just as quiet the second time, and gets nothing.
#[test]
fn a_track_that_stays_silent_gets_no_command() {
    let yaml = "\
bpm: 120
time_signature: 4/4
tracks:
  a: {input: 1}
  b: {input: 2}
sections:
  - id: eins
    bars: 2
    autorelease: true
    tracks: {a: record, b: stop}
  - id: zwei
    bars: 2
    autorelease: true
    tracks: {a: play, b: stop}
  - id: drei
    bars: 2
    autorelease: true
    tracks: {a: play, b: stop}
";
    let mut drive = Drive::new(yaml);
    drive.to(drive.at_bar(20));
    assert!(
        drive.of(1).is_empty() && drive.monitor_of(1).is_empty(),
        "ein durchgehend stiller Track bekommt kein einziges Kommando: {:?}",
        drive.of(1)
    );
}

// ---------------------------------------------------------------------------------------------
// Release, pending and quantisation
// ---------------------------------------------------------------------------------------------

/// `autorelease: false` loops until the button is pressed. With `quantize: bar` the change takes
/// effect on the next bar line, and it is visibly armed until it does.
#[test]
fn without_autorelease_the_section_loops_until_the_release_button() {
    let yaml = "\
bpm: 120
time_signature: 4/4
tracks:
  a: {input: 1}
sections:
  - id: loop_hier
    bars: 4
    autorelease: false
    quantize: bar
    tracks: {a: record}
  - id: danach
    bars: 4
    autorelease: true
    tracks: {a: play}
";
    let mut drive = Drive::new(yaml);

    // Three passes later the section is still the current one and nothing new was scheduled.
    drive.to(drive.at_bar(12) + 5_000);
    assert_eq!(drive.section(), Some(0));
    assert_eq!(drive.runner.view(drive.pos).pass, 4, "vierter Durchlauf");
    assert_eq!(drive.runner.view(drive.pos).bar, 1);
    assert_eq!(drive.of(0).len(), 3, "nur der Take des ersten Durchlaufs");
    assert!(drive.runner.view(drive.pos).armed.is_none());

    // Release inside bar 13 (counted from the first section): the change lands on bar 14.
    let message = drive.release();
    assert!(message.contains("armiert"), "{message}");
    let armed = drive.runner.view(drive.pos).armed.expect("armiert");
    assert!(armed.text().starts_with("Wechsel armiert"), "{}", armed.text());
    let (at, _) = drive.runner.armed_at().expect("armiert");
    assert_eq!(at, drive.at_bar(13), "naechste Taktgrenze");

    drive.to(at);
    assert_eq!(drive.section(), Some(1));
    // The loop keeps playing across the boundary, so nothing at all happens at that sample. (The
    // fourth command the track picks up here is the silence at the *end* of the score, which
    // section 1 arms as soon as it starts because it has `autorelease`.)
    assert!(
        drive.of(0).iter().all(|c| c.at() != Some(at)),
        "an der Sektionsgrenze passiert nichts: {:?}",
        drive.of(0)
    );
    assert_eq!(
        drive.of(0).last(),
        Some(&Command::StopPlay {
            track: 0,
            at: drive.at_bar(17)
        }),
        "nur das Ende der Partitur ist noch geplant"
    );
}

/// With `quantize: loop` the change waits for the end of the running pass, not for the next bar.
#[test]
fn loop_quantisation_changes_at_the_end_of_the_pass() {
    let yaml = "\
bpm: 120
time_signature: 4/4
tracks:
  a: {input: 1}
sections:
  - id: eins
    bars: 4
    autorelease: false
    quantize: loop
    tracks: {a: record}
  - id: zwei
    bars: 4
    autorelease: true
    tracks: {a: play}
";
    let mut drive = Drive::new(yaml);
    // Bar 6 counted from the first section - the middle of the second pass, bars 5 to 8.
    drive.to(drive.at_bar(5) + 3_000);
    assert_eq!(drive.runner.view(drive.pos).pass, 2);
    drive.release();
    let armed = drive.runner.view(drive.pos).armed.expect("armiert");
    assert_eq!(armed.bars, 3, "drei Takte bis zum Ende des Durchlaufs");
    let (at, _) = drive.runner.armed_at().unwrap();
    assert_eq!(at, drive.at_bar(8));

    drive.to(at - 1);
    assert_eq!(drive.section(), Some(0));
    drive.to(at);
    assert_eq!(
        drive.section(),
        Some(1),
        "am Ende des Durchlaufs, nicht an der naechsten Taktgrenze"
    );
}

/// Pressing the release button twice must not skip a section.
#[test]
fn pressing_release_twice_skips_nothing() {
    let mut drive = Drive::new(HENRY);
    // Section 3 (voice_3) is the first one without autorelease.
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(drive.section(), Some(3));

    let first = drive.release();
    assert!(first.contains("armiert"), "{first}");
    let after_first = drive.commands.len();
    let second = drive.release();
    assert!(
        second.contains("schon ein Wechsel"),
        "der zweite Druck meldet den armierten Wechsel: {second}"
    );
    assert_eq!(
        drive.commands.len(),
        after_first,
        "und schickt kein einziges Kommando"
    );

    drive.to(drive.at_bar(8 * 4));
    assert_eq!(
        drive.section(),
        Some(4),
        "genau eine Sektion weiter, nicht zwei"
    );
}

/// A release during a take does not cut it: the change waits for the take to finish and says so.
#[test]
fn a_change_never_cuts_a_running_take() {
    let yaml = "\
bpm: 120
time_signature: 4/4
tracks:
  a: {input: 1}
sections:
  - id: aufnahme
    bars: 8
    autorelease: false
    quantize: bar
    tracks: {a: record}
  - id: danach
    bars: 4
    autorelease: true
    tracks: {a: stop}
";
    let mut drive = Drive::new(yaml);
    let timeline = drive.timeline();
    let start = drive.at_bar(0);
    let take_end = drive.at_bar(8);

    // Bar 4 of the recording - plain bar quantisation would put the change on bar 5.
    drive.to(drive.at_bar(3) + 1_000);
    let message = drive.release();
    assert!(
        message.contains("laufenden Aufnahme"),
        "die Meldung erklaert die Verschiebung: {message}"
    );
    let (at, _) = drive.runner.armed_at().expect("armiert");
    assert_eq!(at, take_end, "der Wechsel wartet auf das Ende des Takes");

    drive.to(drive.at_bar(9));
    assert_eq!(
        drive.of(0),
        vec![
            Command::StartRecord { track: 0, at: start },
            Command::StopRecord { track: 0, at: take_end },
            Command::StartPlay { track: 0, at: take_end },
            Command::StopPlay { track: 0, at: take_end },
        ],
        "acht Takte Take, danach still - kein abgeschnittener Loop"
    );
    assert_eq!(
        take_end - start,
        timeline.span_bars(drive.first_bar, 8),
        "der Loop hat die geschriebene Laenge, nicht die bis zum Tastendruck"
    );
}

/// `goto` jumps, and it is refused while a change is armed - the armed commands are already in the
/// engine's waiting room and cannot be recalled.
#[test]
fn goto_jumps_and_is_refused_while_something_is_armed() {
    let mut drive = Drive::new(HENRY);
    // During an `autorelease` section the follow-up is already armed.
    drive.to(drive.at_bar(1));
    let refused = drive.goto(4);
    assert!(refused.contains("schon ein Wechsel"), "{refused}");

    // In the looping section 3 nothing is armed, so the jump goes through.
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(drive.section(), Some(3));
    let ok = drive.goto(5);
    assert!(ok.contains("ende"), "springt zur letzten Sektion: {ok}");
    drive.to(drive.at_bar(8 * 4));
    assert_eq!(drive.section(), Some(5), "Sektion 4 wurde uebersprungen");

    // An index the score does not have is refused by name.
    let bad = drive.goto(99);
    assert!(bad.contains("gibt es nicht"), "{bad}");
}

/// Stopping ends the run and silences every track that was sounding.
#[test]
fn stop_all_silences_everything_and_ends_the_run() {
    let mut drive = Drive::new(HENRY);
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(drive.section(), Some(3));
    assert!(drive.runner.armed_at().is_none(), "hier ist nichts armiert");

    let before = drive.commands.len();
    let message = drive.stop_all();
    assert!(message.contains("Alles gestoppt"), "{message}");
    assert_eq!(drive.runner.phase(), Phase::Stopped);
    assert!(drive.commands.len() > before, "es wurde etwas gestoppt");
    assert!(
        drive.commands[before..].iter().all(|c| matches!(
            c,
            Command::StopPlay { .. } | Command::StopRecord { .. } | Command::SetMonitor { on: false, .. }
        )),
        "nur Stopps und Mithoeren aus: {:?}",
        &drive.commands[before..]
    );
    assert!(drive.release().contains("zu Ende"));
}

/// Stopping while a change is armed cannot recall the armed commands, so it neutralises them where
/// they land - and says so.
#[test]
fn stopping_while_a_change_is_armed_neutralises_it_where_it_lands() {
    let mut drive = Drive::new(HENRY);
    // Inside section 1, which has `autorelease` - so section 2 is already armed and its overdub is
    // already in the engine.
    drive.to(drive.at_bar(8) + 1_000);
    assert_eq!(drive.section(), Some(1));
    let (armed_at, _) = drive.runner.armed_at().expect("Sektion 2 ist armiert");

    let before = drive.commands.len();
    let message = drive.stop_all();
    assert!(message.contains("armierte Wechsel"), "{message}");
    let after: Vec<Command> = drive.commands[before..].to_vec();
    assert!(
        after.contains(&Command::ClearTrack {
            track: 1,
            at: armed_at
        }),
        "die schon abgeschickte Ebene wird an ihrer eigenen Position unschaedlich gemacht: {after:?}"
    );
    assert!(after.contains(&Command::StopPlay {
        track: 0,
        at: armed_at
    }));
    assert_eq!(drive.runner.phase(), Phase::Stopped);
}

// ---------------------------------------------------------------------------------------------
// Monitoring
// ---------------------------------------------------------------------------------------------

/// Monitoring follows the target state - on for `hear_through` and the takes, off otherwise - it is
/// switched at the boundary rather than when the section is armed, and never twice in a row.
#[test]
fn monitoring_follows_the_target_state_and_switches_at_the_boundary() {
    let mut drive = Drive::new(HENRY);
    let voice = 1usize;

    // Section 0 wants `hear_through` on the voice, but the section is only armed - nothing sent.
    assert!(
        drive.monitor_of(voice).is_empty(),
        "das Mithoeren wird nicht Takte im Voraus umgelegt"
    );
    drive.to(drive.at_bar(0));
    assert_eq!(drive.monitor_of(voice), vec![true], "an der Sektionsgrenze");

    // hear_through -> record -> overdub -> overdub: it stays on, so nothing more is sent.
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(
        drive.monitor_of(voice),
        vec![true],
        "kein Umschalten zwischen Sektionen, die dasselbe wollen"
    );

    // Section 4 puts the voice on `play`: the live input goes quiet again.
    drive.release();
    drive.to(drive.at_bar(8 * 4));
    assert_eq!(drive.monitor_of(voice), vec![true, false]);

    // The guitar goes the other way round: on for its take, off as soon as it only plays back.
    assert_eq!(drive.monitor_of(0), vec![true, false]);
}

/// A track the score forbids monitoring on never gets it switched on.
#[test]
fn a_track_with_monitor_false_is_never_monitored() {
    let yaml = "\
bpm: 120
tracks:
  a: {input: 1, monitor: false}
sections:
  - id: eins
    bars: 2
    autorelease: true
    tracks: {a: hear_through}
  - id: zwei
    bars: 2
    autorelease: true
    tracks: {a: record}
";
    let mut drive = Drive::new(yaml);
    drive.to(drive.at_bar(8));
    assert!(
        drive.monitor_of(0).is_empty(),
        "monitor: false schlaegt jeden Sollzustand"
    );
}

// ---------------------------------------------------------------------------------------------
// Odd time signatures - Henry's score is in 3/4
// ---------------------------------------------------------------------------------------------

/// The same proofs at three awkward metres: boundaries on the timeline's own grid, takes exactly
/// `bars` bars long, an overdub of exactly one pass, and a release quantised to bar or to pass.
#[test]
fn the_same_holds_at_odd_time_signatures() {
    for (bpm, sig, bars) in [(141.0, "3/4", 8u32), (137.0, "7/8", 5), (93.5, "5/4", 3)] {
        let yaml = format!(
            "bpm: {bpm}\ntime_signature: {sig}\ntracks:\n  a: {{input: 1}}\n  b: {{input: 2}}\n\
             sections:\n\
             \x20 - id: eins\n    bars: {bars}\n    autorelease: true\n    tracks: {{a: record, b: hear_through}}\n\
             \x20 - id: zwei\n    bars: {bars}\n    autorelease: false\n    quantize: loop\n    tracks: {{a: play, b: record}}\n\
             \x20 - id: drei\n    bars: {bars}\n    autorelease: false\n    quantize: bar\n    tracks: {{a: play, b: overdub}}\n"
        );
        let mut drive = Drive::new(&yaml);
        let timeline = drive.timeline();
        let n = bars as u64;
        let (s0, s1, s2) = (drive.at_bar(0), drive.at_bar(n), drive.at_bar(2 * n));

        drive.to(s1);
        assert_eq!(
            drive.of(0),
            vec![
                Command::StartRecord { track: 0, at: s0 },
                Command::StopRecord { track: 0, at: s1 },
                Command::StartPlay { track: 0, at: s1 },
            ],
            "{sig}: der Take deckt genau {bars} Takte"
        );
        assert_eq!(s1 - s0, timeline.span_bars(drive.first_bar, bars));

        // Section 1 loops; `quantize: loop` puts the release on the end of the running pass.
        drive.to(s1 + timeline.span_bars(drive.first_bar + n, 1) + 1_000);
        drive.release();
        assert_eq!(drive.runner.armed_at().map(|a| a.0), Some(s2), "{sig}");
        drive.to(s2);
        assert_eq!(drive.section(), Some(2), "{sig}");

        // The overdub of section 2 is exactly one pass of b's loop, on b's own grid.
        let b_loop = s2 - s1;
        let all = drive.of(1);
        let overdub: Vec<Command> = all[all.len() - 3..].to_vec();
        assert_eq!(
            overdub,
            vec![
                Command::StartOverdub { track: 1, at: s2 },
                Command::StopRecord { track: 1, at: s2 + b_loop },
                Command::StartPlay { track: 1, at: s2 + b_loop },
            ],
            "{sig}: eine Ebene, genau ein Durchlauf"
        );
        // The first track has not been touched since its take, two sections ago.
        assert_eq!(drive.of(0).len(), 3, "{sig}");
    }
}

// ---------------------------------------------------------------------------------------------
// The definition of done: Henry's score, end to end, through the real engine
// ---------------------------------------------------------------------------------------------

/// One simulated run of a score: the engine, the runner, and a hand on the release button.
///
/// Returns the simulation, the runner and every command that was sent, in order.
fn play(yaml: &str, tail_blocks: u64) -> (Sim, Runner, Vec<Command>, u64) {
    let score = compile_score(yaml).expect("die Partitur uebersetzt");
    let timeline = Timeline::new(RATE, score.bpm, score.signature());
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
    let lengths: Vec<u64> = score.sections.iter().map(|s| u64::from(s.bars)).collect();
    let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
    let mut sim = Sim::new(
        SimSpec {
            bpm: score.bpm,
            signature: score.signature(),
            bars: max_bars,
            block: BLOCK as usize,
            // Track 0 is the guitar on input 2, track 1 the voice on input 1 - as written.
            tracks: vec![TrackSpec::on(1), TrackSpec::on(0)],
            input_channels: 2,
            // No loopback cable: a looper records what is played, not what it produced.
            loopback_channel: None,
            layer_frames: Some(loop_capacity(&timeline, max_bars)),
            ..Default::default()
        },
        mono(fingerprint),
    );
    let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
    runner.start(0);
    let first_bar = runner.armed_at().expect("armiert").1;

    // Where every section begins, in bars from the first one.
    let mut offsets = vec![0u64];
    for length in &lengths {
        offsets.push(offsets.last().unwrap() + length);
    }
    // A little past the last bar, so the tick that ends the score really happens.
    let end = timeline.bar_start(first_bar + offsets[lengths.len()]) + 4 * BLOCK as u64;

    let mut sent = Vec::new();
    let mut released = vec![false; lengths.len()];
    while sim.pos() < end {
        for cmd in runner.take_commands() {
            sent.push(cmd);
            sim.send(cmd);
        }
        runner.tick(sim.pos());
        if let Some(index) = runner.view(sim.pos()).section {
            // The release button, pressed inside the **last** bar of a section that waits for it,
            // so the section gets its written length whichever grid it quantises to.
            let last_bar =
                timeline.bar_start(first_bar + offsets[index] + lengths[index].saturating_sub(1));
            if waits[index] && !released[index] && sim.pos() >= last_bar {
                released[index] = true;
                runner.next(sim.pos());
            }
        }
        sim.run_blocks(1);
    }
    for cmd in runner.take_commands() {
        sent.push(cmd);
        sim.send(cmd);
    }
    sim.run_blocks(tail_blocks);
    (sim, runner, sent, first_bar)
}

/// A signal whose value identifies the input frame it came from.
fn fingerprint(k: u64) -> f32 {
    ((k % 30_011) as f32) / 30_011.0 - 0.5
}

/// Henry's score - four written sections, six after `repeat:` is unrolled - played from the first
/// bar to the last with nothing touched but the release button.
///
/// The expectations are the whole command stream with its timestamps, computed from the timeline
/// rather than read back from the runner, plus the loop geometry the engine really ends up with.
#[test]
fn henrys_score_plays_from_start_to_finish() {
    let score = compile_score(HENRY).expect("Henrys Partitur uebersetzt");
    assert_eq!(score.bpm, 141.0);
    assert_eq!(score.time_signature, "3/4");
    assert_eq!(score.beats_per_bar, 3);
    assert_eq!(score.sections.len(), 6, "vier geschriebene, sechs aufgeloeste");
    assert_eq!(score.tracks.len(), 2);

    let (mut sim, runner, sent, first_bar) = play(HENRY, 8);
    let timeline = runner.scheduler().timeline;
    let s: Vec<u64> = [0u64, 8, 16, 24, 32, 40]
        .iter()
        .map(|b| timeline.bar_start(first_bar + b))
        .collect();

    assert_eq!(
        runner.phase(),
        Phase::Finished,
        "die Partitur laeuft von Anfang bis Ende durch"
    );

    // ---- the command stream, timestamp by timestamp ----
    let guitar: Vec<Command> = sent.iter().copied().filter(|c| c.track() == Some(0)).collect();
    assert_eq!(
        guitar,
        vec![
            // Section 0: the loop-defining take, eight bars.
            Command::StartRecord { track: 0, at: s[0] },
            Command::StopRecord { track: 0, at: s[1] },
            Command::StartPlay { track: 0, at: s[1] },
            Command::SetMonitor { track: 0, on: true },
            // Section 1: `play`. Only the monitoring changes; the loop simply keeps running,
            // through sections 1, 2, 3 and 4 without a single further command.
            Command::SetMonitor { track: 0, on: false },
            // Section 5 (`ende`): silence.
            Command::StopPlay { track: 0, at: s[5] },
        ],
        "die Gitarre: ein Take, dann vier Sektionen lang nichts, dann still"
    );

    let voice: Vec<Command> = sent.iter().copied().filter(|c| c.track() == Some(1)).collect();
    let loop_len = s[2] - s[1];
    assert_eq!(
        voice,
        vec![
            // Section 0: hear_through - audible, not recorded, and no take.
            Command::SetMonitor { track: 1, on: true },
            // Section 1: the loop-defining take.
            Command::StartRecord { track: 1, at: s[1] },
            Command::StopRecord { track: 1, at: s[2] },
            Command::StartPlay { track: 1, at: s[2] },
            // Section 2: layer two, exactly one pass on the voice's **own** grid. The section's
            // bar line is s[2]; here they coincide.
            Command::StartOverdub { track: 1, at: s[1] + loop_len },
            Command::StopRecord { track: 1, at: s[1] + 2 * loop_len },
            Command::StartPlay { track: 1, at: s[1] + 2 * loop_len },
            // Section 3: layer three, released by hand. Its bar line s[3] and the loop grid differ
            // by one sample here, because every bar boundary is rounded on its own - and the layer
            // follows the loop, never the bar. Off by that one sample it would be a frame short.
            Command::StartOverdub { track: 1, at: s[1] + 2 * loop_len },
            Command::StopRecord { track: 1, at: s[1] + 3 * loop_len },
            Command::StartPlay { track: 1, at: s[1] + 3 * loop_len },
            // Section 4: all three layers play and the singer sings live against them, so the
            // monitor path goes quiet and nothing else is scheduled.
            Command::SetMonitor { track: 1, on: false },
            // Section 5: silence.
            Command::StopPlay { track: 1, at: s[5] },
        ],
        "die Stimme: mithoeren, ein Loop, zwei Ebenen darauf, dann still"
    );

    // ---- and what the engine made of it ----
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.tracks()[0].layers, 1, "Gitarre: ein Loop");
    assert_eq!(status.tracks()[1].layers, 3, "Stimme: drei Ebenen");
    assert_eq!(
        status.tracks()[0].loop_len,
        s[1] - s[0],
        "der Gitarrenloop ist genau die acht Takte der Sektion"
    );
    assert_eq!(status.tracks()[1].loop_len, loop_len);
    assert_eq!(status.tracks()[0].origin, s[0]);
    assert_eq!(status.tracks()[1].origin, s[1]);
    assert!(!status.tracks()[0].playing, "am Ende ist alles still");
    assert!(!status.tracks()[1].playing);
    assert!(!status.tracks()[1].monitor);
    assert_eq!(status.refusal, Refusal::None);
    assert_eq!(status.ignored_commands, 0, "die Engine hat nichts abgelehnt");
}

/// The three layers the score records really are three passes of the same loop, aligned to the
/// sample - the property the whole overdub design rests on, here produced by a written score
/// instead of by key presses.
#[test]
fn the_layers_a_score_records_share_one_grid() {
    // Enough tail for the last take: its final frames only arrive R frames after its end.
    let (sim, _runner, _sent, _first_bar) = play(HENRY, 16);
    let track = sim.core.track(1);
    assert_eq!(track.layer_count(), 3);
    let origin = track.origin();
    let len = track.loop_len();

    for layer in 0..3usize {
        let content = track.layer(layer).expect("Ebene").content(len);
        assert_eq!(content.len() as u64, len);
        for index in [0u64, 1, len / 3, len / 2, len - 1] {
            // Loop index `i` of layer `n` holds the input frame the musician played at musical
            // position `origin + n*len + i`, which arrived R frames later.
            let expect = fingerprint(origin + layer as u64 * len + index + R);
            assert!(
                (content[index as usize] - expect).abs() < 1e-6,
                "Ebene {layer}, Loop-Index {index}: {} statt {expect}",
                content[index as usize]
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Loading against a running engine
// ---------------------------------------------------------------------------------------------

/// A score whose tracks differ from the ones an engine is already running with is refused, with a
/// German sentence that says what to do instead. Nothing is reconfigured silently.
#[test]
fn a_score_is_refused_against_a_different_running_track_layout() {
    let score = compile_score(HENRY).expect("uebersetzt");
    let matching = vec![TrackDef::mono("gitarre", 1), TrackDef::mono("voice", 0)];
    check_tracks(&score, &matching).expect("dieselben Tracks sind in Ordnung");

    let too_few = vec![TrackDef::mono("gitarre", 1)];
    let err = check_tracks(&score, &too_few).expect_err("eine andere Anzahl");
    assert!(err.contains("2 Tracks"), "{err}");
    assert!(err.contains("beenden"), "die Meldung sagt, was zu tun ist: {err}");

    let renamed = vec![TrackDef::mono("gitarre", 1), TrackDef::mono("stimme", 0)];
    let err = check_tracks(&score, &renamed).expect_err("anderer Name");
    assert!(err.contains("voice"), "{err}");

    let rewired = vec![TrackDef::mono("gitarre", 1), TrackDef::mono("voice", 3)];
    let err = check_tracks(&score, &rewired).expect_err("anderer Eingang");
    assert!(err.contains("Eingang"), "{err}");
    assert!(err.contains("falsche Quelle"), "{err}");
}

/// Impossible scores are refused before anything is scheduled.
#[test]
fn impossible_scores_are_refused_at_construction() {
    let timeline = Timeline::new(RATE, 120.0, TimeSignature::new(4, 4));
    let mut score = compile_score(HENRY).expect("uebersetzt");
    score.sections.clear();
    let err = Runner::new(score, timeline, BLOCK)
        .err()
        .expect("ohne Sektion");
    assert!(err.contains("keine einzige Sektion"), "{err}");

    // The compiler already refuses more than `MAX_TRACKS` tracks; the runner carries the same
    // ceiling so a score reaching it from anywhere else cannot overflow the status snapshot.
    let mut score = compile_score(HENRY).expect("uebersetzt");
    while score.tracks.len() <= MAX_TRACKS {
        let mut extra = score.tracks[0].clone();
        extra.index = score.tracks.len();
        extra.name = format!("t{}", score.tracks.len());
        score.tracks.push(extra);
    }
    let err = Runner::new(score, timeline, BLOCK)
        .err()
        .expect("zu viele Tracks");
    assert!(err.contains("hoechstens"), "{err}");
}

/// The position estimate the runner hands to the scheduler is the scheduler's own - one piece of
/// arithmetic, not two.
#[test]
fn the_position_estimate_is_the_schedulers() {
    let runner = runner_for(HENRY);
    assert_eq!(runner.estimated_pos(None), 0);
    assert_eq!(
        runner.estimated_pos(Some((48_000, Duration::from_millis(100)))),
        48_000 + 4_800
    );
}

/// The buttons say something sensible before the first start and after the last section.
#[test]
fn the_buttons_answer_in_german_before_the_start_and_after_the_end() {
    let mut runner = runner_for(HENRY);
    assert!(runner.next(0).contains("noch nicht"));
    assert!(runner.goto(2, 0).contains("noch nicht"));
    assert!(runner.take_commands().is_empty());

    runner.start(0);
    assert!(runner.start(0).contains("laeuft schon"));
    assert_eq!(runner.phase(), Phase::CountIn);
}
