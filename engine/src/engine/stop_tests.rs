//! Free looping's stop button, offline: the stop that waits for the end of the bar, taking it back,
//! the cut mid-bar, and what a newer press on the same track does to a stop that waits. The plans
//! come from the very [`Scheduler`] the app and the terminal use; the engine is the simulated one.
//! MIDI loops are in `midi_tests.rs` (`a_stop_at_the_bar_end_*`).

use super::command::Command;
use super::schedule::{Quantize, Scheduler, StopAt};
use super::sim::{Sim, SimSpec, mono};
use super::timeline::{TimeSignature, Timeline};
use super::track::{TrackState, edge_frames, fade_out};

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const R: u64 = 827;
/// A tempo whose bar lines do not fall on block boundaries, so "on the bar line" cannot pass by
/// accident of the block size.
const BPM: f64 = 101.0;

fn timeline() -> Timeline {
    Timeline::new(RATE, BPM, TimeSignature::new(4, 4))
}

/// Never silent, so a zero in the output can only mean "stopped".
fn signal(k: u64) -> f32 {
    0.1 + (k % 1_000) as f32 / 10_000.0
}

struct Looping {
    sim: Sim,
    sched: Scheduler,
    /// Where the loop begins and how long it is.
    start: u64,
    len: u64,
}

impl Looping {
    /// A take over bars 2 to 1+`bars` (index 1..1+bars), from `end` on it plays.
    fn take(bars: u64, play: bool) -> Self {
        let tl = timeline();
        let start = tl.bar_start(1);
        let end = tl.bar_start(1 + bars);
        let spec = SimSpec {
            bpm: BPM,
            signature: TimeSignature::new(4, 4),
            bars: bars as u32,
            latency: R,
            roundtrip: R,
            block: BLOCK,
            ..Default::default()
        };
        let mut sim = Sim::new(spec, mono(signal));
        sim.send(Command::StartRecord { track: 0, at: start });
        sim.send(Command::StopRecord { track: 0, at: end });
        sim.send(Command::StartPlay { track: 0, at: end });
        if play {
            sim.run_to(end + R + 4 * BLOCK as u64);
        } else {
            sim.run_to(start + R + 4 * BLOCK as u64);
        }
        Looping {
            sim,
            sched: Scheduler::new(tl, bars as u32, BLOCK as u32, Quantize::Loop),
            start,
            len: end - start,
        }
    }

    /// A two-bar loop, playing.
    fn playing() -> Self {
        Self::take(2, true)
    }

    fn send(&mut self, commands: &[Command]) {
        for c in commands {
            self.sim.send(*c);
        }
    }

    /// The stop button, pressed while the engine stands at `est` (the control side's estimate is
    /// exact here).
    fn stop(&mut self, est: u64, state: TrackState, when: StopAt) -> Vec<Command> {
        self.sim.run_to(est);
        let plan = self.sched.stop(0, "t", est, state, when, true);
        self.send(&plan.commands);
        plan.commands
    }

    fn heard(&self, p: u64) -> f32 {
        self.sim.out_l[p as usize]
    }

    /// What the loop is heard as at `p` while it plays steadily.
    fn steady(&self, p: u64) -> f32 {
        self.sim
            .core
            .track(0)
            .steady_layer(0, (p - self.start) % self.len)
            .l
    }

    /// Every frame of `from..to` is the loop at full level.
    fn assert_plays(&self, from: u64, to: u64, why: &str) {
        for p in from..to {
            assert_eq!(self.heard(p), self.steady(p), "{why}: Frame {p}");
            assert_ne!(self.heard(p), 0.0, "{why}: Frame {p} still");
        }
    }

    fn assert_silent(&self, from: u64, to: u64, why: &str) {
        for p in from..to {
            assert_eq!(self.heard(p), 0.0, "{why}: Frame {p}");
        }
    }
}

/// The default: the loop plays to the end of the bar it is in and is silent from exactly that bar
/// line on - the last fade length before it fades out, the same fade every timed stop has.
#[test]
fn a_stop_at_bar_end_is_silent_from_exactly_the_bar_line() {
    let tl = timeline();
    let mut l = Looping::playing();
    // Mid-bar in bar index 5; its end, bar index 6, is not a seam of the two-bar loop from bar 1.
    let est = tl.bar_start(5) + 30_001;
    let bar_end = tl.bar_start(6);
    assert_ne!(bar_end % BLOCK as u64, 0, "Testaufbau: Taktgrenze mitten im Puffer");
    assert_ne!((bar_end - l.start) % l.len, 0, "Testaufbau: keine Naht");

    let sent = l.stop(est, TrackState::Playing, StopAt::BarEnd);
    assert_eq!(sent, vec![Command::StopPlay { track: 0, at: bar_end }]);
    l.sim.run_to(bar_end + 20_000);

    let edge = edge_frames(RATE);
    l.assert_plays(est, bar_end - edge, "bis zur Blende spielt der Loop voll");
    for j in 0..edge {
        let p = bar_end - edge + j;
        assert_eq!(l.heard(p), l.steady(p) * fade_out(j, edge), "Ausblendung, Frame {j}");
    }
    l.assert_silent(bar_end, bar_end + 20_000, "ab der Taktgrenze still");
    assert!(!l.sim.core.track(0).playing());
    assert_eq!(l.sim.core.track(0).layer_count(), 1, "der Loop bleibt erhalten");
}

/// Pressed again before the bar line, the stop is taken back: nothing changes in the sound.
#[test]
fn a_second_press_before_the_bar_line_takes_the_stop_back() {
    let tl = timeline();
    let mut l = Looping::playing();
    let est = tl.bar_start(5) + 30_001;
    let bar_end = tl.bar_start(6);
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    let again = l.stop(est + 20_000, TrackState::Playing, StopAt::BarEnd);
    assert_eq!(
        again,
        vec![Command::CancelStop {
            track: 0,
            at: bar_end,
            take: false
        }]
    );
    l.sim.run_to(bar_end + 20_000);
    l.assert_plays(est, bar_end + 20_000, "der Stopp ist zurueckgenommen");
    assert!(l.sim.core.track(0).playing());
}

/// The small half while a stop waits: silent one head start after the press, and the stop that
/// waited is gone - a play pressed before its bar line is not cut by it.
#[test]
fn stop_now_while_a_stop_waits_cuts_at_once() {
    let tl = timeline();
    let mut l = Looping::playing();
    let est = tl.bar_start(5) + 30_001;
    let bar_end = tl.bar_start(6);
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    let est2 = est + 10_000;
    let now = est2 + l.sched.guard;
    let sent = l.stop(est2, TrackState::Playing, StopAt::Now);
    assert_eq!(
        sent,
        vec![
            Command::CancelStop {
                track: 0,
                at: bar_end,
                take: false
            },
            Command::StopPlay { track: 0, at: now },
        ]
    );
    l.sim.run_to(now + 20_000);
    let edge = edge_frames(RATE);
    l.assert_plays(est, now - edge, "bis zur Blende vor dem Schnitt");
    l.assert_silent(now, now + 20_000, "mitten im Takt still");

    // Play again, before the old bar line: it starts on that bar line and keeps playing past it.
    let est3 = now + 20_000;
    let plan = l.sched.play(0, "t", est3);
    assert_eq!(plan.commands, vec![Command::StartPlay { track: 0, at: bar_end }]);
    l.send(&plan.commands);
    l.sim.run_to(bar_end + 30_000);
    l.assert_plays(bar_end, bar_end + 30_000, "spielt wieder, nichts stoppt ihn");
}

/// Play on a track whose stop waits takes the stop back (and the loop simply keeps playing); an
/// overdub does the same, and the loop is heard up to the overdub's start and on.
#[test]
fn play_and_overdub_take_a_waiting_stop_back() {
    let tl = timeline();
    let est = tl.bar_start(5) + 30_001;
    let bar_end = tl.bar_start(6);

    let mut l = Looping::playing();
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    let est2 = est + 5_000;
    let mut sent = l.sched.supersede_stop(0, est2);
    sent.extend(l.sched.play(0, "t", est2).commands);
    assert_eq!(
        sent,
        vec![
            Command::CancelStop {
                track: 0,
                at: bar_end,
                take: false
            },
            Command::StartPlay { track: 0, at: bar_end },
        ]
    );
    l.send(&sent);
    l.sim.run_to(bar_end + 20_000);
    l.assert_plays(est, bar_end + 20_000, "Play nimmt den Stopp zurueck");

    // An overdub on the loop's own grid (bar index 7): the loop plays through the bar line the
    // stop waited for, all the way to the overdub.
    let mut l = Looping::playing();
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    let mut sent = l.sched.supersede_stop(0, est2);
    let overdub = l.sched.overdub(0, "t", est2, l.start, l.len, 1);
    assert_eq!(overdub.commands[0], Command::StartOverdub { track: 0, at: tl.bar_start(7) });
    sent.extend(overdub.commands);
    l.send(&sent);
    l.sim.run_to(tl.bar_start(7));
    l.assert_plays(est, tl.bar_start(7) - 1, "Overdub nimmt den Stopp zurueck");
}

/// Stop during a take ends it on the bar line, exactly as the record end always did: the loop is
/// as long as the take got, and it plays on from there. Pressed again before that bar line, the
/// take runs to its own end.
#[test]
fn a_stop_during_a_take_ends_it_on_the_bar_line_or_lets_it_run() {
    let tl = timeline();
    let start = tl.bar_start(1);

    let mut l = Looping::take(8, false);
    let est = tl.bar_start(3) + 20_001;
    let sent = l.stop(est, TrackState::Recording, StopAt::BarEnd);
    let bar_end = tl.bar_start(4);
    assert_eq!(
        sent,
        vec![
            Command::StopRecord { track: 0, at: bar_end },
            Command::StartPlay { track: 0, at: bar_end },
        ]
    );
    // Past the take's own end too: its own `StopRecord` there must not change anything any more.
    l.sim.run_to(tl.bar_start(10) + R + 1_000);
    let track = l.sim.core.track(0);
    assert_eq!(track.loop_len(), bar_end - start, "drei Takte");
    assert!(track.playing());
    assert_eq!(track.layer_count(), 1);

    // Taken back: the full eight bars.
    let mut l = Looping::take(8, false);
    l.stop(est, TrackState::Recording, StopAt::BarEnd);
    let again = l.stop(est + 10_000, TrackState::Recording, StopAt::BarEnd);
    assert_eq!(
        again,
        vec![Command::CancelStop {
            track: 0,
            at: bar_end,
            take: true
        }]
    );
    l.sim.run_to(tl.bar_start(10) + R + 1_000);
    let track = l.sim.core.track(0);
    assert_eq!(track.loop_len(), tl.bar_start(9) - start, "acht Takte");
    assert!(track.playing());

    // The small half during a take does not cut it: it still ends on the bar line.
    let mut l = Looping::take(8, false);
    l.stop(est, TrackState::Recording, StopAt::BarEnd);
    let now = l.stop(est + 10_000, TrackState::Recording, StopAt::Now);
    assert!(now.is_empty(), "{now:?}");
    l.sim.run_to(tl.bar_start(5) + R + 1_000);
    assert_eq!(l.sim.core.track(0).loop_len(), bar_end - start);
}

/// Pressed in the last bar of a take, the early end falls on the take's own end - two identical
/// pairs wait there. Taking the stop back removes the newer pair only; the take still ends.
#[test]
fn taking_back_a_stop_on_the_takes_own_end_leaves_the_takes_end_alone() {
    let tl = timeline();
    let start = tl.bar_start(1);
    let end = tl.bar_start(3);
    let mut l = Looping::take(2, false);
    let est = tl.bar_start(2) + 20_001;
    let sent = l.stop(est, TrackState::Recording, StopAt::BarEnd);
    assert_eq!(sent[0], Command::StopRecord { track: 0, at: end });
    l.stop(est + 10_000, TrackState::Recording, StopAt::BarEnd);
    l.sim.run_to(end + R + 20_000);
    let track = l.sim.core.track(0);
    assert_eq!(track.loop_len(), end - start);
    assert!(track.playing(), "the take's own StartPlay is still there");
}

/// No bar line to wait for while the clock stands: the stop acts at once.
#[test]
fn with_the_clock_standing_the_stop_acts_at_once() {
    let tl = timeline();
    let mut l = Looping::playing();
    let est = tl.bar_start(5) + 30_001;
    l.sim.run_to(est);
    let plan = l.sched.stop(0, "t", est, TrackState::Playing, StopAt::BarEnd, false);
    let now = est + l.sched.guard;
    assert_eq!(plan.commands, vec![Command::StopPlay { track: 0, at: now }]);
    l.send(&plan.commands);
    l.sim.run_to(now + 10_000);
    l.assert_silent(now, now + 10_000, "sofort still");
}

/// Both kinds of cancel act on the audio thread: a scan of the waiting room and a memmove, nothing
/// allocated or freed inside the callback.
#[test]
fn cancelling_a_stop_runs_without_an_allocation_in_the_callback() {
    let tl = timeline();
    let mut l = Looping::playing();
    let est = tl.bar_start(5) + 30_001;
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    l.stop(est + 5_000, TrackState::Playing, StopAt::BarEnd);
    l.stop(est + 10_000, TrackState::Playing, StopAt::BarEnd);
    l.stop(est + 15_000, TrackState::Playing, StopAt::Now);
    l.sim.run_to(tl.bar_start(7));

    let mut t = Looping::take(8, false);
    let est = tl.bar_start(3) + 20_001;
    t.stop(est, TrackState::Recording, StopAt::BarEnd);
    t.stop(est + 5_000, TrackState::Recording, StopAt::BarEnd);
    t.sim.run_to(tl.bar_start(10));

    for sim in [&l.sim, &t.sim] {
        assert!(sim.rt.is_zero(), "{:?} {:?}", sim.rt, sim.rt_first);
        let applied = crate::load::BlockProfile {
            command_mask: sim.applied,
            ..Default::default()
        }
        .command_names();
        assert!(applied.contains(&"CancelStop"), "{applied:?}");
    }
}

/// A cancel for a stop that has already acted changes nothing: never half taken back.
#[test]
fn a_cancel_that_comes_too_late_does_nothing() {
    let tl = timeline();
    let mut l = Looping::playing();
    let est = tl.bar_start(5) + 30_001;
    let bar_end = tl.bar_start(6);
    l.stop(est, TrackState::Playing, StopAt::BarEnd);
    l.sim.run_to(bar_end + 1_000);
    l.sim.send(Command::CancelStop {
        track: 0,
        at: bar_end,
        take: false,
    });
    l.sim.run_to(bar_end + 20_000);
    l.assert_silent(bar_end, bar_end + 20_000, "bleibt gestoppt");
    assert!(!l.sim.core.track(0).playing());
}
