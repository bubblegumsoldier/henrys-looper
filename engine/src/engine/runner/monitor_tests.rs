//! Monitoring that a score controls beyond "this section records or hears through":
//! `monitor: always` (heard from the load on, yielding on a shared input) and `monitor_fade`
//! (the monitoring comes up before its section and goes down after it). On the real engine core
//! (`Sim`) with a constant input, so a bus sample *is* the monitor gain.

use super::*;
use crate::engine::bus::Bus;
use crate::engine::command::TrackStatus;
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Sim, SimSpec, TrackSpec};
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;
/// What every input channel carries.
const LEVEL: f32 = 0.5;

struct Run {
    sim: Sim,
    runner: Runner,
    first_bar: u64,
    offsets: Vec<u64>,
    waits: Vec<bool>,
    released: Vec<bool>,
    /// (output position, every track's status) after every block.
    seen: Vec<(u64, Vec<TrackStatus>)>,
    started: bool,
}

impl Run {
    /// Loaded, not started: the setup is applied and a few blocks run.
    fn load(yaml: &str) -> Self {
        let score = compile_score(yaml).expect("die Partitur uebersetzt");
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
        let mut offsets = vec![0u64];
        for s in &score.sections {
            offsets.push(offsets.last().unwrap() + u64::from(s.bars));
        }
        let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
        let tracks = score
            .tracks
            .iter()
            .map(|t| TrackSpec {
                input: t.track_input(),
                ..TrackSpec::on(0)
            })
            .collect();
        let mut sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                block: BLOCK as usize,
                tracks,
                input_channels: 2,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 6,
                ..Default::default()
            },
            Box::new(|_, _| LEVEL),
        );
        for cmd in setup_commands(&score) {
            sim.send(cmd);
        }
        let runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        let released = vec![false; waits.len()];
        let mut run = Self { sim, runner, first_bar: 0, offsets, waits, released, seen: Vec::new(), started: false };
        let to = run.sim.pos() + 8 * u64::from(BLOCK);
        run.run_to(to);
        run
    }

    fn start(&mut self) {
        self.runner.start(self.sim.pos());
        self.first_bar = self.runner.armed_at().expect("armiert").1;
        self.started = true;
        self.pump();
    }

    fn pump(&mut self) {
        for cmd in self.runner.take_commands() {
            self.sim.send(cmd);
        }
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    fn section(&self, index: usize) -> u64 {
        self.bar(self.offsets[index])
    }

    fn beat(&self) -> u64 {
        self.runner.scheduler().timeline.samples_per_beat() as u64
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.runner.tick(self.sim.pos());
            if self.started
                && let Some(index) = self.runner.view(self.sim.pos()).section
            {
                let last = self.bar(self.offsets[index + 1].saturating_sub(1));
                if self.waits[index] && !self.released[index] && self.sim.pos() >= last {
                    self.released[index] = true;
                    self.runner.next(self.sim.pos());
                }
            }
            self.pump();
            self.sim.step();
            let n = self.runner.score().tracks.len();
            let status = self.sim.latest_status().expect("Status").tracks()[..n].to_vec();
            self.seen.push((self.sim.pos(), status));
        }
    }

    fn monitors_at(&self, pos: u64) -> Vec<bool> {
        let (_, s) = self.seen.iter().find(|(p, _)| *p >= pos).expect("so weit gelaufen");
        s.iter().map(|t| t.monitor).collect()
    }

    /// Both buses, left, at `pos` (with two outputs the device collapses them into one mix).
    fn heard(&self, pos: u64) -> f32 {
        self.sim.bus(Bus::Main, pos).l + self.sim.bus(Bus::Monitor, pos).l
    }
}

// ---------------------------------------------------------------------------------------------
// monitor: always
// ---------------------------------------------------------------------------------------------

/// Five guitar tracks on input 2 in the owner's score; two are enough to show it: `guitar` is
/// always heard, `bass` takes the input over while it records - never both at once.
const ALWAYS: &str = "\
bpm: 120
tracks:
  guitar: {input: 2, monitor: always}
  bass: {input: 2}
  mic: {input: 1}
sections:
  - id: bass_take
    bars: 2
    autorelease: true
    tracks:
      bass: record
  - id: play
    bars: 2
    autorelease: true
    tracks:
      bass: play
  - id: end
    bars: 1
    autorelease: true
";

#[test]
fn an_always_track_is_heard_from_the_load_and_yields_to_a_take_on_its_input() {
    let mut run = Run::load(ALWAYS);
    // Ready, before Start: heard - he can check his sound.
    assert_eq!(run.monitors_at(run.sim.pos()), vec![true, false, false]);
    run.start();
    let count_in = run.sim.pos() + 2_000;
    let take = run.section(0);
    let play = run.section(1);
    let end = run.section(3) + 4_000;
    run.run_to(end);
    assert_eq!(run.monitors_at(count_in), vec![true, false, false], "Einzaehler");
    assert_eq!(run.monitors_at(take + 4 * u64::from(BLOCK)), vec![false, true, false], "der Bass-Take");
    assert_eq!(run.monitors_at(play + 4 * u64::from(BLOCK)), vec![true, false, false], "wieder die Gitarre");
    assert_eq!(run.monitors_at(end), vec![true, false, false], "nach dem Ende");
    // Never the input twice: in no block are guitar and bass monitored together.
    assert!(run.seen.iter().all(|(_, s)| !(s[0].monitor && s[1].monitor)));
    // And it sounds once: the monitor bus carries the input at single level in the take.
    assert!((run.heard(take + 1_000) - LEVEL).abs() < 1e-6, "{}", run.heard(take + 1_000));
}

#[test]
fn stop_all_switches_an_always_track_off_and_back_to_start_on_again() {
    let mut run = Run::load(ALWAYS);
    run.start();
    let to = run.section(1) + 10_000;
    run.run_to(to);
    run.runner.stop_all(run.sim.pos());
    run.pump();
    let to = run.sim.pos() + 4_000;
    run.run_to(to);
    assert_eq!(run.monitors_at(to), vec![false, false, false]);
    let (_, commands) = run.runner.rewind(run.sim.pos()).expect("rewind");
    for cmd in commands {
        run.sim.send(cmd);
    }
    // Once the clear has acted (it lands a head start later), the host puts the setup back.
    let to = run.sim.pos() + run.runner.scheduler().guard + 4 * u64::from(BLOCK);
    run.run_to(to);
    for cmd in setup_commands(run.runner.score()) {
        run.sim.send(cmd);
    }
    let to = run.sim.pos() + 4 * u64::from(BLOCK);
    run.run_to(to);
    assert_eq!(run.monitors_at(to), vec![true, false, false]);
}

#[test]
fn always_is_refused_where_it_cannot_mean_anything() {
    let err = |text: &str| match compile_score(text) {
        Ok(_) => String::new(),
        Err(e) => e.issues.iter().map(|i| i.message.clone()).collect::<Vec<_>>().join(" | "),
    };
    let text = "bpm: 100\ntracks:\n  keys: {input: instrument, monitor: always}\nsections:\n  - id: s\n    bars: 1\n";
    assert!(err(text).contains("does nothing on the instrument track"), "{}", err(text));
    let text = "bpm: 100\ntracks:\n  a: {input: 1, monitor: sometimes}\nsections:\n  - id: s\n    bars: 1\n";
    assert!(err(text).contains("must be true, false or always"), "{}", err(text));
    let doc = crate::score::doc::parse_doc(ALWAYS).expect("parses");
    assert!(doc.tracks[0].monitor_always);
    assert!(crate::score::doc::render_doc(&doc).contains("guitar: {input: 2, monitor: always}"));
}

// ---------------------------------------------------------------------------------------------
// monitor_fade
// ---------------------------------------------------------------------------------------------

/// The mic comes in with a take after two silent bars and goes out after it; the score fades it in
/// over two beats and out over one.
fn faded(first_waits: bool) -> String {
    format!(
        "bpm: 120\nmonitor_fade: {{in: 2, out: 1}}\ntracks:\n  mic: {{input: 1}}\nsections:\n  - id: before\n    bars: 2\n    autorelease: {}\n  - id: take\n    bars: 2\n    autorelease: true\n    tracks:\n      mic: record\n  - id: after\n    bars: 2\n    autorelease: true\n",
        !first_waits
    )
}

#[test]
fn the_monitor_comes_up_before_its_section_and_goes_down_after_it() {
    let mut run = Run::load(&faded(false));
    run.start();
    let (take, after) = (run.section(1), run.section(2));
    let beat = run.beat();
    run.run_to(run.section(3));
    // Silent until two beats before the take, full on its first sample, and up in between -
    // quadratically: half way it is a quarter.
    assert_eq!(run.heard(take - 2 * beat - 10), 0.0);
    let half = run.heard(take - beat);
    assert!((half - LEVEL * 0.25).abs() < 1e-3, "{half}");
    assert!((run.heard(take) - LEVEL).abs() < 1e-3, "{}", run.heard(take));
    assert!((run.heard(take + 1_000) - LEVEL).abs() < 1e-6);
    // Monotonic on the way up.
    let mut last = 0.0;
    for p in (take - 2 * beat..take).step_by(97) {
        assert!(run.heard(p) + 1e-7 >= last, "faellt bei {}", take - p);
        last = run.heard(p);
    }
    // Down over one beat after the take, then off.
    assert!((run.heard(after) - LEVEL).abs() < 1e-3);
    assert!(run.heard(after + beat / 2) < LEVEL * 0.3);
    assert_eq!(run.heard(after + beat + 10), 0.0);
    assert_eq!(run.monitors_at(after + beat + 2 * u64::from(BLOCK)), vec![false]);
    // The take is dry and at full level from its first frame: the fade is only what is heard.
    let track = run.sim.core.track(0);
    let content = track.layer(0).expect("Take").content(track.loop_len());
    assert!(content.iter().all(|v| (v - LEVEL).abs() < 1e-6), "Take nicht voll: {:?}", &content[..4]);
}

#[test]
fn after_a_release_the_fade_starts_when_armed_and_is_full_at_the_boundary() {
    let mut run = Run::load(&faded(true));
    run.start();
    run.run_to(run.section(2));
    let take = run.section(1);
    // Released in the last bar (the harness): the switch is armed about a bar ahead, so the full
    // two beats fit - and the level is full on the boundary either way.
    assert!((run.heard(take) - LEVEL).abs() < 1e-3, "{}", run.heard(take));
    assert!(run.heard(take - 3 * run.beat()) == 0.0);
}

#[test]
fn a_fade_longer_than_the_count_in_starts_with_it() {
    let text = "bpm: 120\nmonitor_fade: {in: 16}\ntracks:\n  mic: {input: 1}\nsections:\n  - id: take\n    bars: 2\n    autorelease: true\n    tracks:\n      mic: hear_through\n";
    let mut run = Run::load(text);
    let press = run.sim.pos();
    run.start();
    let take = run.section(0);
    run.run_to(take + 1_000);
    assert_eq!(run.heard(press), 0.0);
    assert!((run.heard(take) - LEVEL).abs() < 1e-3);
    assert!(run.heard(take - run.beat()) < LEVEL);
}

#[test]
fn stop_all_cancels_a_running_fade() {
    let mut run = Run::load(&faded(false));
    run.start();
    let take = run.section(1);
    let beat = run.beat();
    run.run_to(take - beat);
    assert!(run.heard(take - beat - 200) > 0.0, "der Fade laeuft");
    run.runner.stop_all(run.sim.pos());
    run.pump();
    let soon = run.sim.pos() + run.runner.scheduler().guard;
    run.run_to(take + 4 * beat);
    for p in soon..take + 4 * beat {
        assert_eq!(run.heard(p), 0.0, "{} nach dem Stopp", p - soon);
    }
}

#[test]
fn monitor_fade_round_trips_and_is_checked() {
    let text = faded(false);
    let doc = crate::score::doc::parse_doc(&text).expect("parses");
    let rendered = crate::score::doc::render_doc(&doc);
    assert!(rendered.contains("monitor_fade: {in: 2, out: 1}"), "{rendered}");
    let score = compile_score(&text.replace("mic: {input: 1}", "mic: {input: 1, monitor_fade: {out: 0.5}}")).unwrap();
    assert_eq!(score.fade_of(0), (2000, 500));
    let bad = text.replace("in: 2", "in: 20");
    assert!(compile_score(&bad).is_err());
}
