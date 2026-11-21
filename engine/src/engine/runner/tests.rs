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

/// It runs before the first section, it is the default number of whole bars (two), and it happens
/// exactly once - no later section gets one.
#[test]
fn the_count_in_runs_once_before_the_first_section() {
    let mut drive = Drive::new(HENRY);
    let timeline = drive.timeline();
    let guard = drive.runner.scheduler().guard;

    assert_eq!(drive.runner.phase(), Phase::CountIn);
    let armed = drive.runner.view(0).armed.expect("die erste Sektion ist armiert");
    assert!(armed.count_in, "der Einzaehler ist als solcher erkennbar");
    assert!(armed.text().contains("Count-in"), "{}", armed.text());

    // Definition: the count-in begins the moment Start is pressed, plus the head start every timed
    // command needs; that moment is bar 1 of the run, and the section starts exactly
    // `DEFAULT_COUNT_IN_BARS` bars after it.
    assert_eq!(drive.runner.count_in_bars(), DEFAULT_COUNT_IN_BARS);
    assert_eq!(DEFAULT_COUNT_IN_BARS, 2, "zwei Takte - die Vorgabe des Besitzers");
    assert_eq!(drive.runner.start_lead(), guard);
    assert_eq!(timeline.zero(), guard, "Takt 1 des Laufs ist der Druck plus der Vorlauf");
    assert_eq!(drive.first_bar, u64::from(DEFAULT_COUNT_IN_BARS));
    let start = drive.at_bar(0);
    assert_eq!(start, guard + timeline.span_bars(0, DEFAULT_COUNT_IN_BARS));

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

/// A longer count-in is a flag, and the lead is exactly that many bars plus the fixed head start -
/// wherever in a bar of the idling clock the button was pressed.
#[test]
fn the_count_in_length_is_configurable_and_never_undershoots() {
    for bars in 1..=MAX_COUNT_IN_BARS {
        let mut runner = runner_for(HENRY).with_count_in(bars);
        let idle = runner.scheduler().timeline;
        let est = idle.bar_start(7) + idle.samples_per_beat() as u64 + 123;
        runner.start(est);
        let timeline = runner.scheduler().timeline;
        let lead = runner.start_lead();
        let (start, first_bar) = runner.armed_at().expect("armiert");
        assert_eq!(timeline.zero(), est + lead);
        assert_eq!(first_bar, u64::from(bars));
        assert_eq!(start, est + lead + timeline.span_bars(0, bars), "{bars} Takte");
    }
}

/// **Start means now.** Pressed anywhere in a bar of the clock that idles under a loaded score -
/// forty different places, spread over more than a bar - the count-in's first beat is exactly the
/// head start after the press, and the first section exactly `count_in` bars after that. The whole
/// command stream of the run, measured from its own bar 1, is the same every time.
#[test]
fn the_count_in_starts_right_away_wherever_the_button_is_pressed() {
    let reference = {
        let mut drive = Drive::new(HENRY);
        drive.to(drive.at_bar(20));
        let zero = drive.timeline().zero();
        shifted(&drive.commands, zero)
    };
    let idle = runner_for(HENRY).scheduler().timeline;
    let bar = idle.span_bars(0, 1);
    for press in 0..40u64 {
        let est = idle.bar_start(3) + press * (bar + 37) / 40;
        let mut runner = runner_for(HENRY);
        runner.start(est);
        let lead = runner.start_lead();
        let timeline = runner.scheduler().timeline;
        let first_bar = runner.armed_at().expect("armiert").1;
        let commands = runner.take_commands();
        // The first beat: the window opens and the clock gets its bar 1 at the press plus the lead.
        assert_eq!(
            commands[..2],
            [
                Command::TransportStart { at: est + lead },
                Command::ClickGate {
                    at: est + lead,
                    open: true,
                    bar1: timeline.bar_start(first_bar),
                },
            ],
            "Druck {press}"
        );
        assert!(lead <= u64::from(idle.sample_rate()) / 20, "ein fester, kleiner Vorlauf: {lead}");
        assert_eq!(
            timeline.bar_start(first_bar) - (est + lead),
            timeline.span_bars(0, DEFAULT_COUNT_IN_BARS),
            "Druck {press}: Takt 1 genau zwei Takte nach dem ersten Schlag"
        );
        let mut drive = Drive { runner, commands, pos: est, first_bar };
        drive.to(drive.at_bar(20));
        assert_eq!(
            shifted(&drive.commands, est + lead),
            reference,
            "Druck {press}: derselbe Lauf, nur verschoben"
        );
    }
}

/// Every command with its position measured from `zero` - and the absolute numbers in its text
/// replaced by that relative one.
fn shifted(commands: &[Command], zero: u64) -> Vec<(Option<u64>, String)> {
    commands
        .iter()
        .map(|c| {
            let mut text = format!("{c:?}");
            if let Some(at) = c.at() {
                text = text.replace(&format!("at: {at}"), "at: _");
            }
            if let Command::ClickGate { bar1, open: true, .. } = c {
                text = text.replace(&format!("bar1: {bar1}"), &format!("bar1: +{}", bar1 - zero));
            }
            (c.at().map(|at| at - zero), text)
        })
        .collect()
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
    assert!(message.contains("armed"), "{message}");
    let armed = drive.runner.view(drive.pos).armed.expect("armiert");
    assert!(armed.text().starts_with("Switch armed"), "{}", armed.text());
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
    assert!(first.contains("armed"), "{first}");
    let after_first = drive.commands.len();
    let second = drive.release();
    assert!(
        second.contains("already armed"),
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
        message.contains("still recording"),
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
    assert!(refused.contains("already armed"), "{refused}");

    // In the looping section 3 nothing is armed, so the jump goes through.
    drive.to(drive.at_bar(8 * 3) + 1_000);
    assert_eq!(drive.section(), Some(3));
    let ok = drive.goto(5);
    assert!(ok.contains("\"ende\""), "springt zur letzten Sektion: {ok}");
    drive.to(drive.at_bar(8 * 4));
    assert_eq!(drive.section(), Some(5), "Sektion 4 wurde uebersprungen");

    // An index the score does not have is refused by name.
    let bad = drive.goto(99);
    assert!(bad.contains("no section 100"), "{bad}");
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
    assert!(message.contains("Stopped all"), "{message}");
    assert_eq!(drive.runner.phase(), Phase::Stopped);
    assert!(drive.commands.len() > before, "es wurde etwas gestoppt");
    assert!(
        drive.commands[before..].iter().all(|c| matches!(
            c,
            Command::DropWaiting { .. }
                | Command::StopPlay { .. }
                | Command::StopRecord { .. }
                | Command::SetMonitor { on: false, .. }
                | Command::ClickGate { open: false, .. }
        )),
        "nur Stopps, Mithoeren aus und der Klick aus: {:?}",
        &drive.commands[before..]
    );
    assert!(drive.release().contains("has ended"));
}

/// Stopping while a change is armed calls the armed commands back - all of them, with one
/// `DropWaiting` at the sample the stop takes effect, before anything else it sends - and says so.
/// Nothing is answered where it would have landed any more: the stop sends nothing timed for the
/// armed boundary.
#[test]
fn stopping_while_a_change_is_armed_calls_it_back() {
    let mut drive = Drive::new(HENRY);
    // Inside section 1, which has `autorelease` - so section 2 is already armed and its overdub is
    // already in the engine.
    drive.to(drive.at_bar(8) + 1_000);
    assert_eq!(drive.section(), Some(1));
    let (armed_at, _) = drive.runner.armed_at().expect("Sektion 2 ist armiert");

    let before = drive.commands.len();
    let soon = drive.runner.scheduler().soon(drive.pos);
    let message = drive.stop_all();
    assert!(message.contains("is called back"), "{message}");
    let after: Vec<Command> = drive.commands[before..].to_vec();
    assert_eq!(after[0], Command::DropWaiting { at: soon }, "{after:?}");
    assert!(
        after.iter().all(|c| c.at().is_none_or(|at| at < armed_at)),
        "nichts mehr fuer die armierte Grenze: {after:?}"
    );
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
    // The start made its own moment bar 1: every bar below is on the run's grid.
    let timeline = runner.scheduler().timeline;
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
    assert!(err.contains("2 tracks"), "{err}");
    assert!(err.contains("stop the engine"), "die Meldung sagt, was zu tun ist: {err}");

    let renamed = vec![TrackDef::mono("gitarre", 1), TrackDef::mono("stimme", 0)];
    let err = check_tracks(&score, &renamed).expect_err("anderer Name");
    assert!(err.contains("voice"), "{err}");

    let rewired = vec![TrackDef::mono("gitarre", 1), TrackDef::mono("voice", 3)];
    let err = check_tracks(&score, &rewired).expect_err("anderer Eingang");
    assert!(err.contains("input"), "{err}");
    assert!(err.contains("wrong source"), "{err}");
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
    assert!(err.contains("no sections"), "{err}");

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
    assert!(err.contains("at most"), "{err}");
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
fn the_buttons_answer_before_the_start_and_after_the_end() {
    let mut runner = runner_for(HENRY);
    assert!(runner.next(0).contains("not running yet"));
    assert!(runner.goto(2, 0).contains("not running yet"));
    assert!(runner.take_commands().is_empty());

    runner.start(0);
    assert!(runner.start(0).contains("already running"));
    assert_eq!(runner.phase(), Phase::CountIn);
}

// ---------------------------------------------------------------------------------------------
// Stopping in the middle of a take, and starting over
// ---------------------------------------------------------------------------------------------

/// One section that records and then waits for the release button - so while its take runs,
/// nothing is armed, and the take's own `StartPlay` at its end is already in the engine.
const ONE_TAKE: &str = "\
bpm: 120
tracks:
  a: {input: 1}
sections:
  - id: rec
    bars: 4
    autorelease: false
    tracks:
      a: record
";

/// A runner and the real engine core, driven together block by block.
struct Live {
    sim: Sim,
    runner: Runner,
    first_bar: u64,
}

impl Live {
    fn new(yaml: &str) -> Self {
        Self::with(yaml, mono(fingerprint))
    }

    /// The same with a signal of the test's choosing on the one input.
    fn with(yaml: &str, played: crate::engine::sim::Played) -> Self {
        let score = compile_score(yaml).expect("uebersetzt");
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
        let sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                block: BLOCK as usize,
                tracks: vec![TrackSpec::on(0)],
                input_channels: 1,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                ..Default::default()
            },
            played,
        );
        let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        runner.start(0);
        let first_bar = runner.armed_at().expect("armiert").1;
        let mut live = Self { sim, runner, first_bar };
        live.pump();
        live
    }

    fn pump(&mut self) {
        for cmd in self.runner.take_commands() {
            self.sim.send(cmd);
        }
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.runner.tick(self.sim.pos());
            self.pump();
            self.sim.run_blocks(1);
        }
    }

    fn track(&mut self) -> crate::engine::command::TrackStatus {
        self.sim.latest_status().expect("Status").tracks()[0]
    }
}

/// **The gap, nailed down.** "Stop all" in the middle of a take, in a section that waits for the
/// release button (so nothing is armed): the take is closed early, but the `StartPlay` it scheduled
/// for its own end was already in the engine. Without a counter-command the track would start
/// playing again on its own, bars after the musician asked for silence.
#[test]
fn stop_all_during_a_take_keeps_the_track_silent_after_the_takes_planned_end() {
    let mut live = Live::new(ONE_TAKE);
    live.run_to(live.bar(1) + 1_000);
    assert_eq!(live.runner.view(live.sim.pos()).section, Some(0));
    assert!(live.runner.armed_at().is_none(), "nichts armiert - genau der Fall");

    let message = live.runner.stop_all(live.sim.pos());
    live.pump();
    assert!(message.contains("Stopped all"), "{message}");

    // Well past the bar the take would have ended on.
    let past = live.bar(6);
    live.run_to(past);
    let track = live.track();
    assert!(!track.playing, "der Track laeuft wieder an: {track:?}");
    assert_eq!(live.sim.latest_status().unwrap().refusal, Refusal::None);
}

/// The same when the take ends between the press and the next bar line: the take is not cut, but
/// its `StartPlay` comes after the stop and must not undo it.
#[test]
fn stop_all_just_before_a_take_ends_keeps_the_track_silent() {
    let mut live = Live::new(ONE_TAKE);
    let end = live.bar(4);
    let guard = live.runner.scheduler().guard;
    live.run_to(end - guard - 2 * BLOCK as u64);
    live.runner.stop_all(live.sim.pos());
    live.pump();
    live.run_to(live.bar(7));
    assert!(!live.track().playing);
}

/// Back to the start: the run is over, the tracks are cleared, and the same score plays again from
/// its first section - with exactly the command stream of a first run, shifted in time.
#[test]
fn a_finished_score_can_be_rewound_and_played_again() {
    let mut live = Live::new(ONE_TAKE);
    live.run_to(live.bar(1));
    assert!(live.runner.start(live.sim.pos()).contains("already running"));
    live.runner.stop_all(live.sim.pos());
    live.pump();
    assert!(
        live.runner.start(live.sim.pos()).contains("Back to start"),
        "nach dem Ende sagt Start, wie es weitergeht"
    );

    let est = live.sim.pos();
    let (message, commands) = live.runner.rewind(est).expect("zurueckspulen");
    assert!(message.contains("Back to start"), "{message}");
    assert!(
        commands.iter().any(|c| matches!(c, Command::ClearAll { .. })),
        "zurueck auf Anfang leert die Tracks: {commands:?}"
    );
    for cmd in commands {
        live.sim.send(cmd);
    }
    assert_eq!(live.runner.phase(), Phase::Idle);

    // And again, from the top.
    live.runner.start(live.sim.pos());
    let (start, first_bar) = live.runner.armed_at().expect("wieder armiert");
    live.first_bar = first_bar;
    live.pump();
    live.run_to(live.bar(4) + 4 * BLOCK as u64);
    let track = live.track();
    assert_eq!(track.layers, 1, "ein frischer Take, kein alter Rest");
    assert_eq!(track.origin, start);
    assert_eq!(track.loop_len, live.bar(4) - start, "genau vier Takte");
    assert!(track.playing);
    assert_eq!(live.sim.latest_status().unwrap().refusal, Refusal::None);
}

/// While the score plays there is no rewinding - the armed commands would outlive it.
#[test]
fn rewinding_a_playing_score_is_refused() {
    let mut runner = runner_for(HENRY);
    assert!(runner.rewind(0).is_ok(), "vor dem Start ist das harmlos");
    runner.start(0);
    let refusal = runner.rewind(0).expect_err("laeuft");
    assert!(refusal.contains("Stop all"), "{refusal}");
}

/// "Clear all" throws away what was scheduled before it, so a far-future command of a stopped run
/// cannot reach into the next one.
#[test]
fn clear_all_drops_what_was_scheduled_before_it() {
    let mut live = Live::new(ONE_TAKE);
    live.run_to(live.bar(0) + 1_000);
    // A stray command far in the future, as a stopped run could leave behind.
    let late = live.bar(6);
    live.sim.send(Command::StartPlay { track: 0, at: late });
    let now = live.sim.pos();
    live.sim.send(Command::ClearAll { at: now + 1_000 });
    // Sent after the clear: survives it.
    let (from, to) = (live.bar(2), live.bar(3));
    live.sim.send(Command::StartRecord { track: 0, at: from });
    live.sim.send(Command::StopRecord { track: 0, at: to });
    live.run_to(live.bar(7));
    let track = live.track();
    assert!(!track.playing, "der alte StartPlay wurde mit geleert");
    assert_eq!(track.layers, 1, "was nach dem Leeren geschickt wurde, gilt");
    assert_eq!(track.loop_len, to - from);
}

/// Section boundaries and the end of the score produce a sentence of their own - nobody pressed a
/// button there, so no reply would ever carry it. Each one exactly once, in order.
#[test]
fn every_section_start_and_the_end_are_announced_once() {
    let mut drive = Drive::new(HENRY);
    assert!(drive.runner.take_notices().is_empty(), "der Start selbst antwortet per Rueckgabe");
    let mut notices = Vec::new();
    for section in 0..6u64 {
        // Sections 3 and 4 wait for the release button.
        drive.to(drive.at_bar(8 * section) + 1_000);
        notices.extend(drive.runner.take_notices());
        if drive.runner.armed_at().is_none() {
            drive.release();
        }
    }
    drive.to(drive.at_bar(8 * 5 + 2) + 1_000);
    notices.extend(drive.runner.take_notices());
    assert_eq!(notices.len(), 7, "{notices:#?}");
    assert!(notices[0].starts_with("Section 1/6 \"gitarre_rec\""), "{}", notices[0]);
    assert!(notices[3].contains("loops until Release"), "{}", notices[3]);
    assert!(notices[5].starts_with("Section 6/6 \"ende\": 1 bar,"), "{}", notices[5]);
    assert!(notices[6].starts_with("End of the score"), "{}", notices[6]);
    assert!(drive.runner.take_notices().is_empty(), "abgeholt ist abgeholt");
}

/// The count-in can be set before the start, is at least one bar and is reported in the view;
/// once the score runs it stays as it was.
#[test]
fn the_count_in_is_set_before_the_start_and_never_zero() {
    let mut runner = runner_for(HENRY);
    assert_eq!(runner.view(0).count_in_bars, DEFAULT_COUNT_IN_BARS);
    assert!(runner.set_count_in(3).contains("3 bars"));
    assert_eq!(runner.count_in_bars(), 3);
    let message = runner.set_count_in(0);
    assert_eq!(runner.count_in_bars(), 1, "{message}");
    assert!(message.contains("at least one bar"), "{message}");
    assert_eq!(runner_for(HENRY).with_count_in(0).count_in_bars(), 1);
    runner.set_count_in(2);
    runner.start(0);
    assert!(runner.set_count_in(4).contains("before the score starts"));
    assert_eq!(runner.view(0).count_in_bars, 2);
}

// ---------------------------------------------------------------------------------------------
// What a score says about its tracks
// ---------------------------------------------------------------------------------------------

const SETUP: &str = "\
bpm: 120
tracks:
  stimme: {input: 1, pan: -0.5, latency: 300, latency_trim: -20, bus: main, monitor_bus: none}
  keys: {input: [2, 3], pan: 0.25, bus: monitor}
sections:
  - id: a
    bars: 2
";

/// Pan, both bus assignments and the latency compensation of every track arrive in the engine,
/// and monitoring starts off - the state the runner's picture begins with.
#[test]
fn a_score_sets_up_pan_buses_and_latency_of_every_track() {
    use crate::engine::bus::BusSend;
    use crate::engine::track::TrackLatency;

    let score = compile_score(SETUP).expect("uebersetzt");
    let mut sim = Sim::new(
        SimSpec {
            tracks: vec![TrackSpec::on(0).monitoring(), TrackSpec::stereo(1, 2)],
            input_channels: 3,
            loopback_channel: None,
            ..Default::default()
        },
        mono(fingerprint),
    );
    for command in setup_commands(&score) {
        assert!(command.at().is_none(), "Einrichtung ist kein musikalisches Ereignis: {command:?}");
        sim.send(command);
    }
    sim.run_blocks(2);
    let status = sim.latest_status().expect("Status");
    let [stimme, keys] = [status.tracks()[0], status.tracks()[1]];
    assert_eq!(stimme.pan, -0.5);
    assert_eq!(stimme.loop_send, BusSend::parse("main").unwrap());
    assert_eq!(stimme.monitor_send, BusSend::parse("none").unwrap());
    assert_eq!(stimme.latency, TrackLatency { measured: Some(300), trim: -20 });
    assert_eq!(stimme.latency_frames, 280);
    assert!(!stimme.monitor, "Mithoeren beginnt aus, auch wenn der Track so gestartet wurde");
    assert_eq!(keys.pan, 0.25);
    assert_eq!(keys.loop_send, BusSend::MONITOR);
    assert_eq!(keys.monitor_send, BusSend::MONITOR, "nicht genannt heisst: Vorgabe");
    assert_eq!(keys.latency, TrackLatency::INHERITED);
    assert_eq!(status.refusal, Refusal::None);
}

/// Henry's score, started at many different points of the engine's timeline: in bar 3 of the
/// looping section "voice_3" (overdub on top of an eight-bar loop), Release must arm the switch for
/// the end of the *first* pass - six bars away - and the overdub must start with the section.
///
/// Regression: the loop grid `origin + n * len` and the section's bar line are rounded separately.
/// When the grid point fell a sample *before* the bar line, the overdub was put on the next grid
/// point - a whole loop late - and Release was pushed behind that late take: "in 14 bars" in an
/// eight-bar section (screenshot live-score-armed, ticker "Takt 44, in 15 Takten").
///
/// Since a start makes its own moment bar 1, the press position no longer shifts the run against
/// the rounding of the bar lines - the count-in length does (the first section begins on bar
/// `count_in` of the run). So every count-in from 1 to 8 is played, each from ten presses spread
/// over the idling clock.
#[test]
fn release_in_a_looping_overdub_section_lands_at_the_end_of_the_first_pass() {
    let timeline = runner_for(HENRY).scheduler().timeline;
    for start_bar in 0..80u64 {
        let count_in = 1 + (start_bar % u64::from(MAX_COUNT_IN_BARS)) as u32;
        let mut runner = runner_for(HENRY).with_count_in(count_in);
        let est = timeline.bar_start(start_bar) + start_bar * 1_009 % timeline.span_bars(0, 1);
        runner.start(est);
        let first_bar = runner.armed_at().expect("armed").1;
        let mut drive = Drive { commands: runner.take_commands(), first_bar, runner, pos: est };
        let section_start = drive.at_bar(24);
        drive.to(drive.at_bar(26) + 100);
        assert_eq!(drive.section(), Some(3), "start {start_bar}");

        let overdubs: Vec<u64> = drive
            .of(1)
            .into_iter()
            .filter_map(|c| match c {
                Command::StartOverdub { at, .. } => Some(at),
                _ => None,
            })
            .collect();
        let last = *overdubs.last().expect("voice_3 overdubs");
        assert!(
            last.abs_diff(section_start) < 16,
            "start {start_bar}: die Ebene beginnt mit der Sektion ({last} vs {section_start})"
        );

        let message = drive.release();
        let (_, at_bar) = drive.runner.armed_at().expect("armed");
        assert_eq!(at_bar, first_bar + 32, "start {start_bar}: {message}");
        let armed = drive.runner.view(drive.pos).armed.expect("armed");
        assert_eq!((armed.bars, armed.beats), (6, 0), "start {start_bar}");
        assert_eq!(
            message,
            "Switch to \"alles\" armed at the end of pass 1 of \"voice_3\", in 6 bars.",
            "start {start_bar}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The click window: nothing of the clock is heard while the score does not play
// ---------------------------------------------------------------------------------------------

/// Two bars that end the score on their own - the shortest run that reaches "End of the score".
const SHORT: &str = "\
bpm: 120
time_signature: 4/4
tracks:
  a: {input: 1}
sections:
  - id: rec
    bars: 2
    autorelease: true
    tracks:
      a: record
";

/// A loaded score on the real engine with the click **switched on**, the way the app loads it:
/// the click window closed at once. Silence on the input, so whatever the Monitor bus carries is
/// the click.
fn loaded_with_click(yaml: &str, count_in: u32) -> (Sim, Runner) {
    let score = compile_score(yaml).expect("uebersetzt");
    let timeline = Timeline::new(RATE, score.bpm, score.signature());
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
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
            click: true,
            ..Default::default()
        },
        crate::engine::sim::silence(),
    );
    sim.send(CLOSED_CLICK_GATE);
    let runner = Runner::new(score, timeline, BLOCK)
        .expect("Runner")
        .with_count_in(count_in);
    (sim, runner)
}

/// Engine and runner together up to `pos`, the runner ticked once per block.
fn advance(sim: &mut Sim, runner: &mut Runner, pos: u64) {
    while sim.pos() < pos {
        runner.tick(sim.pos());
        for cmd in runner.take_commands() {
            sim.send(cmd);
        }
        sim.run_blocks(1);
    }
}

/// Loudest sample of both buses in `[from, to)` - the click lives on Monitor, or on Main when the
/// two share a socket. Nothing else sounds here: the input is silent.
fn click_peak(sim: &Sim, from: u64, to: u64) -> f32 {
    use crate::engine::bus::Bus;
    (from..to)
        .flat_map(|p| [sim.bus(Bus::Monitor, p), sim.bus(Bus::Main, p)])
        .map(|f| f.l.abs().max(f.r.abs()))
        .fold(0.0, f32::max)
}

/// Loaded, not started: the clock runs underneath (the runner plans on it), but not one click is
/// heard - however long the musician waits.
#[test]
fn no_click_while_a_loaded_score_waits_for_its_start() {
    let (mut sim, mut runner) = loaded_with_click(ONE_TAKE, 2);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, 6 * bar);
    assert_eq!(runner.phase(), Phase::Idle);
    assert_eq!(click_peak(&sim, 0, sim.pos()), 0.0, "kein Klick vor dem Start");
}

/// The click begins exactly on the first beat of the count-in - the press plus the fixed head
/// start, not the next bar line of the idling clock - counts both bars of it and goes on into the
/// first section.
#[test]
fn the_click_begins_with_the_count_in() {
    let (mut sim, mut runner) = loaded_with_click(ONE_TAKE, 2);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, 3 * bar + 1_000);
    let est = sim.pos();
    runner.start(est);
    let timeline = runner.scheduler().timeline;
    let (first, first_bar) = runner.armed_at().expect("armiert");
    let anchor = timeline.bar_start(first_bar - 2);
    assert_eq!(anchor, est + runner.start_lead(), "der erste Schlag: jetzt");
    assert_eq!(first - anchor, timeline.span_bars(0, 2), "Takt 1 zwei Takte danach");
    advance(&mut sim, &mut runner, first + 2 * bar);

    assert_eq!(click_peak(&sim, 0, anchor), 0.0, "nichts vor dem Einzaehler");
    for beat in 0..8u64 {
        let at = timeline.beat_start(timeline.beat_index_at(anchor) + beat);
        assert!(click_peak(&sim, at, at + 200) > 0.0, "Schlag {beat} des Einzaehlers");
    }
    assert!(click_peak(&sim, first, first + 200) > 0.0, "und weiter in die Sektion");
    assert_eq!(runner.phase(), Phase::Running);
}

/// First sample at or after `from` on which the click sounds. The click's waveform is a sine that
/// starts at zero, so that is the sample *after* the beat it belongs to.
fn first_click(sim: &Sim, from: u64) -> Option<u64> {
    (from..sim.pos()).find(|&p| click_peak(sim, p, p + 1) > 0.0)
}

/// **Start means now, through the real engine.** Forty presses spread over more than a bar of the
/// clock that idles under the loaded score: every time the first click sample comes no later than
/// the fixed head start after the press - never after the rest of a bar - and the first section
/// begins exactly `count_in` bars after that first click.
#[test]
fn the_first_click_follows_the_press_by_the_fixed_head_start_only() {
    let fast = ONE_TAKE.replace("bpm: 120", "bpm: 200");
    for press in 0..40u64 {
        let (mut sim, mut runner) = loaded_with_click(&fast, 2);
        let bar = runner.scheduler().timeline.span_bars(0, 1);
        advance(&mut sim, &mut runner, bar + press * (bar + 37) / 40);
        let est = sim.pos();
        runner.start(est);
        let lead = runner.start_lead();
        let timeline = runner.scheduler().timeline;
        let (first, first_bar) = runner.armed_at().expect("armiert");
        advance(&mut sim, &mut runner, first + 200);
        let click = first_click(&sim, est).expect("der Einzaehler klickt");
        let beat = click - 1;
        assert!(beat - est <= lead, "Druck {press}: erster Klick {} Samples danach", beat - est);
        assert_eq!(beat, timeline.bar_start(0), "Druck {press}: auf dem ersten Schlag");
        assert_eq!(click_peak(&sim, 0, click), 0.0, "Druck {press}: vorher still");
        assert_eq!(first_bar, 2);
        assert_eq!(first - beat, timeline.span_bars(0, 2), "Druck {press}: Takt 1 zwei Takte danach");
        assert_eq!(runner.view(sim.pos()).section, Some(0));
    }
}

/// While the count-in runs the display counts exactly the bars that were set - not a bar more for
/// the rest of the bar the button was pressed in - and before the start nothing counts at all.
#[test]
fn the_countdown_shows_exactly_the_count_in() {
    let (mut sim, mut runner) = loaded_with_click(ONE_TAKE, 3);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, bar + bar / 3);
    let idle = runner.view(sim.pos());
    assert_eq!((idle.phase, idle.beat, idle.armed.clone()), (Phase::Idle, 1, None));
    let est = sim.pos();
    runner.start(est);
    // Pressed, the first beat not yet sounding: the full count-in ahead.
    let view = runner.view(est);
    let armed = view.armed.expect("Einzaehler");
    assert!(armed.count_in);
    assert_eq!((armed.bars, armed.beats, view.beat), (3, 0, 1));
    let timeline = runner.scheduler().timeline;
    // Half a beat into the second beat: two bars and two beats to go.
    let pos = timeline.beat_start(1) + 100;
    let armed = runner.view(pos).armed.expect("Einzaehler");
    assert_eq!((armed.bars, armed.beats), (2, 3));
    assert_eq!(runner.view(pos).beat, 2);
}

/// The score played to its end, "Back to start", Start: the second count-in begins right away too,
/// and records exactly like the first run, measured from its own bar 1.
#[test]
fn a_start_after_the_end_and_back_to_start_counts_in_right_away() {
    let (mut sim, mut runner) = loaded_with_click(SHORT, 2);
    runner.start(sim.pos());
    let first_zero = runner.scheduler().timeline.zero();
    let (first_start, _) = runner.armed_at().expect("armiert");
    let end = runner.scheduler().timeline.bar_start(4);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, end + bar / 2);
    assert_eq!(runner.phase(), Phase::Finished);
    let first_layers = sim.latest_status().unwrap().tracks()[0];

    let (_, commands) = runner.rewind(sim.pos()).expect("zurueck");
    for cmd in commands {
        sim.send(cmd);
    }
    // Somewhere in a bar of the old grid, on purpose.
    let later = sim.pos() + bar + bar / 7;
    advance(&mut sim, &mut runner, later);
    let est = sim.pos();
    runner.start(est);
    let timeline = runner.scheduler().timeline;
    assert_eq!(timeline.zero(), est + runner.start_lead());
    let (start, _) = runner.armed_at().expect("wieder armiert");
    assert_eq!(start - timeline.zero(), first_start - first_zero, "derselbe Abstand");
    advance(&mut sim, &mut runner, timeline.bar_start(4) + bar / 2);
    let click = first_click(&sim, est).expect("klickt wieder");
    assert_eq!(click - 1, est + runner.start_lead(), "der erste Schlag, sofort");
    let track = sim.latest_status().unwrap().tracks()[0];
    assert_eq!(track.origin, start);
    assert_eq!(track.loop_len, first_layers.loop_len);
    assert_eq!(runner.phase(), Phase::Finished);
}

/// "Stop all" silences the click with the loops; after "Back to start" it stays silent until the
/// next Start, which counts in again and records exactly like the first run.
#[test]
fn stop_all_silences_the_click_and_a_second_start_counts_in_again() {
    let (mut sim, mut runner) = loaded_with_click(ONE_TAKE, 2);
    let timeline = runner.scheduler().timeline;
    let bar = timeline.span_bars(0, 1);
    runner.start(sim.pos());
    let (first, _) = runner.armed_at().expect("armiert");
    advance(&mut sim, &mut runner, first + bar + 1_000);

    let est = sim.pos();
    runner.stop_all(est);
    let quiet = runner.scheduler().soon(est) + BLOCK as u64;
    advance(&mut sim, &mut runner, quiet + 4 * bar);
    assert_eq!(runner.phase(), Phase::Stopped);
    assert_eq!(click_peak(&sim, quiet, sim.pos()), 0.0, "nach Stop all: still");

    let (_, commands) = runner.rewind(sim.pos()).expect("zurueck");
    for cmd in commands {
        sim.send(cmd);
    }
    let rewound = sim.pos();
    advance(&mut sim, &mut runner, rewound + 3 * bar);
    assert_eq!(click_peak(&sim, rewound, sim.pos()), 0.0, "nach Back to start: still");

    // The second run: right away again, on a grid of its own.
    let est = sim.pos();
    runner.start(est);
    let timeline = runner.scheduler().timeline;
    let (start, first_bar) = runner.armed_at().expect("wieder armiert");
    let anchor = timeline.bar_start(first_bar - 2);
    assert_eq!(anchor, est + runner.start_lead());
    advance(&mut sim, &mut runner, timeline.bar_start(first_bar + 4) + 4 * BLOCK as u64);
    assert_eq!(click_peak(&sim, rewound, anchor), 0.0);
    assert!(click_peak(&sim, anchor, anchor + 200) > 0.0, "der zweite Einzaehler klickt");
    let track = sim.latest_status().expect("Status").tracks()[0];
    assert_eq!(track.layers, 1);
    assert_eq!(track.origin, start);
    assert_eq!(track.loop_len, timeline.bar_start(first_bar + 4) - start);
    assert_eq!(sim.latest_status().unwrap().refusal, Refusal::None);
}

/// "Stop all" pressed during the count-in, before the opening of the window has even reached the
/// engine: the close still lands after it, so the click does not start anyway.
#[test]
fn stop_all_right_after_start_keeps_the_click_silent() {
    let (mut sim, mut runner) = loaded_with_click(ONE_TAKE, 2);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, bar / 2);
    runner.start(sim.pos());
    runner.stop_all(sim.pos());
    let later = sim.pos() + 4 * bar;
    advance(&mut sim, &mut runner, later);
    assert_eq!(click_peak(&sim, 0, sim.pos()), 0.0);
}

/// The run from the app, step by step: two sections that move on by themselves, "Stop all" while
/// the second one is already armed, "Back to start", Start again - the second count-in clicks.
#[test]
fn a_second_start_after_stop_all_with_an_armed_switch_clicks_again() {
    let two = SHORT.replace(
        "      a: record\n",
        "      a: stop\n  - id: zwei\n    bars: 2\n    autorelease: true\n    tracks:\n      a: stop\n",
    );
    let (mut sim, mut runner) = loaded_with_click(&two, 2);
    let timeline = runner.scheduler().timeline;
    let bar = timeline.span_bars(0, 1);
    advance(&mut sim, &mut runner, 2 * bar + 500);
    runner.start(sim.pos());
    let (first, _) = runner.armed_at().expect("armiert");
    advance(&mut sim, &mut runner, first + bar / 2);
    assert!(runner.armed_at().is_some(), "zwei ist schon armiert");
    runner.stop_all(sim.pos());
    let later = sim.pos() + bar;
    advance(&mut sim, &mut runner, later);
    let (_, commands) = runner.rewind(sim.pos()).expect("zurueck");
    for cmd in commands {
        sim.send(cmd);
    }
    let later = sim.pos() + bar;
    advance(&mut sim, &mut runner, later);
    let quiet_from = sim.pos();
    runner.start(sim.pos());
    let timeline = runner.scheduler().timeline;
    let (start, first_bar) = runner.armed_at().expect("wieder armiert");
    let anchor = timeline.bar_start(first_bar - 2);
    assert_eq!(anchor, quiet_from + runner.start_lead());
    advance(&mut sim, &mut runner, start + 3 * bar);
    assert_eq!(click_peak(&sim, quiet_from, anchor), 0.0);
    assert!(click_peak(&sim, anchor, anchor + 200) > 0.0, "der zweite Einzaehler klickt");
    assert!(click_peak(&sim, start, start + 200) > 0.0, "und die Sektion");
}

/// At the end of the score the click stops on the very sample the score ends - the downbeat that
/// would follow is not clicked any more.
#[test]
fn the_click_ends_with_the_score() {
    let (mut sim, mut runner) = loaded_with_click(SHORT, 2);
    let bar = runner.scheduler().timeline.span_bars(0, 1);
    runner.start(sim.pos());
    let timeline = runner.scheduler().timeline;
    let (first, first_bar) = runner.armed_at().expect("armiert");
    let end = timeline.bar_start(first_bar + 2);
    advance(&mut sim, &mut runner, end + 4 * bar);
    assert_eq!(runner.phase(), Phase::Finished);
    assert!(click_peak(&sim, end - bar, end) > 0.0, "der letzte Takt klickt");
    assert!(click_peak(&sim, first, first + 200) > 0.0);
    assert_eq!(click_peak(&sim, end, sim.pos()), 0.0, "nach dem Ende: still");
}

// ---------------------------------------------------------------------------------------------
// `stack`: a new layer every pass until the section ends
// ---------------------------------------------------------------------------------------------

/// One `stack` section on an empty track that waits for Release, then `play`.
const STACK: &str = "\
bpm: 120
tracks:
  a: {input: 1}
sections:
  - id: drums
    bars: 2
    autorelease: false
    quantize: loop
    tracks:
      a: stack
  - id: groove
    bars: 2
    autorelease: false
    tracks:
      a: play
";

impl Live {
    /// Release pressed at `pos`, the commands it produced handed to the engine.
    fn release_at(&mut self, pos: u64) -> String {
        self.run_to(pos);
        let message = self.runner.next(self.sim.pos());
        self.pump();
        message
    }

    fn layers(&self) -> usize {
        self.sim.core.track(0).layer_count()
    }

    /// Every sample of layer `layer` against the input that was played in pass `pass`.
    fn assert_layer_is_pass(&self, layer: usize, pass: u64, played: &dyn Fn(u64) -> f32) {
        let track = self.sim.core.track(0);
        let (origin, len) = (track.origin(), track.loop_len());
        let content = track.layer(layer).expect("Ebene").content(len);
        for (i, &v) in content.iter().enumerate() {
            let expect = played(origin + pass * len + i as u64 + R);
            assert!(
                (v - expect).abs() < 1e-6,
                "Ebene {layer} (Durchlauf {pass}), Index {i}: {v} statt {expect}"
            );
        }
    }
}

/// Where the first section of [`STACK`] begins, and the grid of the run - the same for every run
/// started at 0 (the start makes its own moment bar 1, so the grid is the run's, not the clock's).
fn stack_first_bar() -> (u64, Timeline) {
    let live = Live::new(STACK);
    (live.first_bar, live.runner.scheduler().timeline)
}

/// Three passes on an empty track, Release in the middle of the third: three layers, each exactly
/// the input of its own pass - no sample lost or doubled at a seam, an impulse on the boundary in
/// exactly one layer at index 0 - the third pass recorded to its end, and the change on that end.
#[test]
fn three_passes_of_stack_on_an_empty_track_are_three_seamless_layers() {
    let (first_bar, timeline) = stack_first_bar();
    let seam = timeline.bar_start(first_bar + 2);
    let played = move |k: u64| if k == seam + R { 1.0 } else { fingerprint(k) * 0.5 };
    let mut live = Live::with(STACK, mono(played));
    assert_eq!(live.first_bar, first_bar);
    let (s0, s2, s4, s6) = (live.bar(0), live.bar(2), live.bar(4), live.bar(6));

    let message = live.release_at(s4 + (s6 - s4) / 2);
    assert_eq!(
        live.runner.armed_at().expect("armed").1,
        first_bar + 6,
        "der Wechsel greift am Ende des laufenden Durchlaufs: {message}"
    );
    live.run_to(live.bar(10) + 2 * R);

    let track = live.sim.core.track(0);
    assert_eq!(live.layers(), 3, "drei Durchlaeufe, drei Ebenen");
    assert_eq!(track.origin(), s0, "der erste Durchlauf hat den Loop definiert");
    assert_eq!(track.loop_len(), s2 - s0, "die Sektionslaenge ist die Looplaenge");
    assert!(track.take().is_none(), "nach dem dritten Durchlauf kein vierter");
    assert!(track.playing());
    for layer in 0..3 {
        live.assert_layer_is_pass(layer, layer as u64, &played);
    }
    // The impulse: index 0 of layer 1, and nowhere in layer 0.
    let len = track.loop_len();
    assert_eq!(track.layer(1).unwrap().content(len)[0], 1.0);
    assert!(track.layer(0).unwrap().content(len).iter().all(|&v| v != 1.0));
    let status = live.sim.latest_status().unwrap();
    assert_eq!(status.ignored_commands, 0, "die Engine hat nichts abgelehnt");
    assert_eq!(live.runner.view(live.sim.pos()).section, Some(1));
}

/// The command stream of `stack` -> `play`: the latch on with the first pass, the first pass as a
/// record, then nothing but the latch going off in the middle of the last pass. No `StartPlay`, no
/// `StopPlay`, no take at the boundary - the loop simply keeps running.
#[test]
fn stack_then_play_sends_only_the_latch_off() {
    let mut drive = Drive::new(STACK);
    let (s0, s2, s4, s6) = (drive.at_bar(0), drive.at_bar(2), drive.at_bar(4), drive.at_bar(6));
    drive.to(s4 + 1_000);
    drive.release();
    drive.to(drive.at_bar(10));
    let len = s2 - s0;
    assert_eq!(
        drive.of(0),
        vec![
            Command::LatchOverdub { track: 0, on: true, at: s0 },
            Command::StartRecord { track: 0, at: s0 },
            Command::StopRecord { track: 0, at: s2 },
            Command::StartPlay { track: 0, at: s2 },
            Command::LatchOverdub { track: 0, on: false, at: s0 + 2 * len + len / 2 },
        ]
    );
    assert!(s6.abs_diff(s0 + 3 * len) <= 2);
    assert_eq!(drive.monitor_of(0), vec![true, false], "mithoeren wie bei overdub");
}

/// An empty pass in the middle is dropped; the passes around it keep their grid positions.
#[test]
fn an_empty_pass_in_a_stack_is_dropped() {
    let (first_bar, timeline) = stack_first_bar();
    let (q0, q1) = (timeline.bar_start(first_bar + 2), timeline.bar_start(first_bar + 4));
    let played = move |k: u64| {
        if (q0 + R..q1 + R).contains(&k) { 0.0 } else { fingerprint(k) * 0.5 }
    };
    let mut live = Live::with(STACK, mono(played));
    live.release_at(live.bar(4) + 1_000);
    live.run_to(live.bar(8) + 2 * R);
    assert_eq!(live.layers(), 2, "der stille zweite Durchlauf ist verworfen");
    live.assert_layer_is_pass(0, 0, &played);
    live.assert_layer_is_pass(1, 2, &played);
}

/// The owner's way of starting the drums: the first pass left empty on an empty track. It leaves
/// no layer, the grid is still the section's (origin on its first bar, length `bars`), and the first
/// pass that was played is layer 1. Nothing played at all until Release: the track is empty, and
/// the `play` after it runs on without a refusal.
#[test]
fn an_empty_first_pass_of_a_stack_keeps_the_grid() {
    let (first_bar, timeline) = stack_first_bar();
    let q1 = timeline.bar_start(first_bar + 2);
    let played = move |k: u64| if k < q1 + R { 0.0 } else { fingerprint(k) * 0.5 };
    let mut live = Live::with(STACK, mono(played));
    live.release_at(live.bar(6) + 1_000);
    live.run_to(live.bar(10) + 2 * R);
    let track = live.sim.core.track(0);
    assert_eq!(track.layer_count(), 3, "Durchlauf 2, 3, 4");
    assert_eq!(track.origin(), live.bar(0));
    assert_eq!(track.loop_len(), live.bar(2) - live.bar(0));
    assert_eq!(track.muted_mask(), 0);
    for layer in 0..3 {
        live.assert_layer_is_pass(layer, layer as u64 + 1, &played);
    }

    let mut quiet = Live::with(STACK, crate::engine::sim::silence());
    quiet.release_at(quiet.bar(4) + 1_000);
    quiet.run_to(quiet.bar(12));
    let status = quiet.sim.latest_status().unwrap();
    assert_eq!(status.tracks()[0].layers, 0, "nichts gespielt, nichts da");
    assert_eq!(status.ignored_commands, 0);
    assert_eq!(quiet.runner.view(quiet.sim.pos()).section, Some(1));
}

/// `quantize: bar`: Release early in the third pass still lets that pass finish - a change never
/// lands in a running take - and says so.
#[test]
fn a_stack_with_bar_quantisation_finishes_the_running_pass() {
    let yaml = STACK.replace("quantize: loop", "quantize: bar");
    let mut drive = Drive::new(&yaml);
    drive.to(drive.at_bar(4) + 1_000);
    let message = drive.release();
    assert_eq!(drive.runner.armed_at().unwrap().1, drive.first_bar + 6, "{message}");
    assert!(message.contains("never cut short"), "{message}");
}

/// Two `stack` sections in a row are one continuous stack: no command at the boundary.
#[test]
fn stack_after_stack_keeps_going_without_a_command() {
    let yaml = STACK.replace("      a: play", "      a: stack");
    let mut drive = Drive::new(&yaml);
    drive.to(drive.at_bar(2) + 1_000);
    drive.release();
    drive.to(drive.at_bar(5));
    assert_eq!(drive.of(0).len(), 4, "nur der Anfang: {:?}", drive.of(0));
    assert_eq!(drive.section(), Some(1));
}

/// `stack` -> `overdub`: the last pass gets its `StopRecord`, so the overdub is queued behind it on
/// the very grid point - through the real engine, without a refusal.
#[test]
fn an_overdub_after_a_stack_follows_on_the_grid() {
    let yaml = STACK.replace("      a: play", "      a: overdub");
    let mut live = Live::new(&yaml);
    live.release_at(live.bar(2) + 1_000);
    live.run_to(live.bar(8) + 2 * R);
    let status = live.sim.latest_status().unwrap();
    assert_eq!(status.ignored_commands, 0, "{:?}", status.refusal);
    assert_eq!(live.layers(), 3, "zwei Stapel-Durchlaeufe und das Overdub");
    for layer in 0..3 {
        live.assert_layer_is_pass(layer, layer as u64, &fingerprint);
    }
}

/// "Stop all" in the middle of a stack: the latch goes off, the running pass ends on the bar line,
/// and the track stays silent.
#[test]
fn stop_all_ends_a_stack() {
    let mut live = Live::new(STACK);
    live.run_to(live.bar(3) + 1_000);
    live.runner.stop_all(live.sim.pos());
    live.pump();
    live.run_to(live.bar(10));
    let track = live.sim.core.track(0);
    assert!(track.take().is_none());
    assert!(!track.playing());
    assert_eq!(track.layer_count(), 2);
}

/// The regression of ce2eeb2, for `stack`: whatever bar the score is started on, the stack's passes
/// sit on the track's own grid `origin + n * len` (never on separately rounded bar lines), Release in
/// the third pass lands at its end, and the overdub after it starts on exactly that grid point.
#[test]
fn a_stack_follows_its_own_grid_from_every_start_position() {
    let yaml = "\
bpm: 141
time_signature: 3/4
tracks:
  voice: {input: 1}
sections:
  - id: rec
    bars: 8
    autorelease: true
    tracks:
      voice: record
  - id: layers
    bars: 8
    autorelease: false
    quantize: loop
    tracks:
      voice: stack
  - id: more
    bars: 8
    autorelease: true
    tracks:
      voice: overdub
";
    // As above: the count-in length is what moves the run against the rounding of the bar lines.
    let timeline = runner_for(yaml).scheduler().timeline;
    for start_bar in 0..80u64 {
        let count_in = 1 + (start_bar % u64::from(MAX_COUNT_IN_BARS)) as u32;
        let mut runner = runner_for(yaml).with_count_in(count_in);
        let est = timeline.bar_start(start_bar) + start_bar * 1_009 % timeline.span_bars(0, 1);
        runner.start(est);
        let first_bar = runner.armed_at().expect("armed").1;
        let mut drive = Drive { commands: runner.take_commands(), first_bar, runner, pos: est };
        let origin = drive.at_bar(0);
        let len = drive.at_bar(8) - origin;
        // The third pass of the stack: bars 24..32 after the first.
        drive.to(drive.at_bar(26) + 100);
        assert_eq!(drive.section(), Some(1), "start {start_bar}");
        let message = drive.release();
        assert_eq!(
            drive.runner.armed_at().unwrap().1,
            first_bar + 32,
            "start {start_bar}: {message}"
        );
        drive.to(drive.at_bar(34));
        let cmds = drive.of(0);
        let latch: Vec<(bool, u64)> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::LatchOverdub { on, at, .. } => Some((*on, *at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            latch,
            vec![(true, origin + len), (false, origin + 3 * len + len / 2)],
            "start {start_bar}"
        );
        let overdubs: Vec<u64> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::StartOverdub { at, .. } => Some(*at),
                _ => None,
            })
            .collect();
        assert_eq!(
            overdubs,
            vec![origin + len, origin + 4 * len],
            "start {start_bar}: erster Stapel-Durchlauf und das Overdub auf dem Loop-Raster"
        );
        assert!(
            cmds.contains(&Command::StopRecord { track: 0, at: origin + 4 * len }),
            "start {start_bar}: der letzte Durchlauf endet auf dem Raster"
        );
    }
}
