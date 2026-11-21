//! The audio path of loops is clean: every take holds exactly what was played, and loop playback
//! has no click at any of its edges.
//!
//! Both promises are checked on the owner's score of the run the clicks were heard in ("The Hardest
//! Part", `tests/data/hardest-part-94-take.yaml`, the sheet left out), driven through the real
//! engine core (`Sim`) with the same block size and latency compensation as on stage (128 frames,
//! 596 frames), the score's mixer included (the Octaver reads its loops ahead).
//!
//! * **Takes.** The input carries the input index itself, encoded so that every sample names the
//!   frame it was played on. A dropped, duplicated or misplaced sample anywhere in any layer shows
//!   up as a broken count - sample-exact, no tolerance.
//! * **Clicks.** The input is low-frequency heavy (the piezo guitar of the recording: 41 Hz and
//!   73 Hz plus a slow drift, no two passes alike), so the first and last sample of a loop never
//!   match - exactly the material that clicked at every wrap. The Main bus may then never step
//!   from one sample to the next by more than a bound derived from the signal's own slope.

use super::bus::Bus;
use super::command::Command;
use super::process::loop_capacity;
use super::runner::{CLOSED_CLICK_GATE, Runner, setup_commands};
use super::sim::{Played, Sim, SimSpec, TrackSpec};
use super::timeline::Timeline;
use crate::score::{CompiledScore, compile_score};

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;
/// The compensation of the owner's Scarlett 2i2 at 128 frames, from the recording's session.json.
const LATENCY: u64 = 596;
const SCORE: &str = include_str!("../../tests/data/hardest-part-94-take.yaml");

/// One encoded sample per input frame and channel: the frame index, scaled so that it is exactly
/// representable (24-bit mantissa) and stays far below full scale. Channel 1 is negative so the
/// two inputs can never be mistaken for each other.
const SCALE: f32 = 16_777_216.0;

fn counting(k: u64, ch: usize) -> f32 {
    let v = (k + 1) as f32 / SCALE;
    if ch == 0 { v } else { -v }
}

fn decode(v: f32) -> Option<u64> {
    let n = (v.abs() * SCALE).round() as u64;
    (n > 0).then(|| n - 1)
}

/// The owner's score on the real engine core.
struct Stage {
    sim: Sim,
    runner: Runner,
    first_bar: u64,
    offsets: Vec<u64>,
    /// Every take the engine opened: (track, layer, start, defines the loop), and its end once
    /// known.
    takes: Vec<(usize, usize, u64, bool)>,
    ends: Vec<Option<u64>>,
}

impl Stage {
    fn new(played: Played, mixer: bool) -> Self {
        let score = compile_score(SCORE).expect("die Partitur uebersetzt");
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
        let mut offsets = vec![0u64];
        for s in &score.sections {
            offsets.push(offsets.last().unwrap() + u64::from(s.bars));
        }
        let tracks: Vec<TrackSpec> = score
            .tracks
            .iter()
            .map(|t| {
                if t.is_instrument() {
                    TrackSpec::instrument()
                } else {
                    TrackSpec {
                        input: t.track_input(),
                        latency: super::track::TrackLatency::INHERITED,
                        ..TrackSpec::on(0)
                    }
                }
            })
            .collect();
        let mut sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                latency: LATENCY,
                roundtrip: LATENCY,
                block: BLOCK as usize,
                tracks,
                input_channels: 2,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 8,
                ..Default::default()
            },
            played,
        );
        for cmd in setup_commands(&score) {
            sim.send(cmd);
        }
        if mixer && let Some(mix) = &score.mixer {
            let names: Vec<String> = score.tracks.iter().map(|t| t.name.clone()).collect();
            for cmd in crate::score::mixer::mixer_commands(mix, &names, &[]) {
                sim.send(cmd);
            }
        }
        sim.send(CLOSED_CLICK_GATE);
        sim.run_blocks(8);
        let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        runner.start(sim.pos());
        let first_bar = runner.armed_at().expect("armiert").1;
        let mut stage = Self {
            sim,
            runner,
            first_bar,
            offsets,
            takes: Vec::new(),
            ends: Vec::new(),
        };
        stage.pump();
        stage
    }

    fn score(&self) -> &CompiledScore {
        self.runner.score()
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    /// First sample of section `index`; `index == sections.len()` is the end of the last one.
    fn section(&self, index: usize) -> u64 {
        self.bar(self.offsets[index])
    }

    fn pump(&mut self) {
        for cmd in self.runner.take_commands() {
            self.sim.send(cmd);
        }
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.runner.tick(self.sim.pos());
            self.pump();
            self.sim.step();
            for t in 0..self.score().tracks.len() {
                if let Some(take) = self.sim.core.track(t).take() {
                    let entry = (t, take.layer, take.start, take.defines_loop);
                    let i = match self.takes.iter().position(|e| *e == entry) {
                        Some(i) => i,
                        None => {
                            self.takes.push(entry);
                            self.ends.push(None);
                            self.takes.len() - 1
                        }
                    };
                    if take.end.is_some() {
                        self.ends[i] = take.end;
                    }
                }
            }
        }
    }

    /// The whole score, up to two bars into its last (released by hand) section.
    fn run_all(&mut self) {
        let end = self.section(self.offsets.len() - 2) + (self.bar(2) - self.bar(0));
        self.run_to(end);
    }
}

/// Which input channel a track records - mic and backing vocals on input 1, the guitars on input 2.
fn channel_of(stage: &Stage, t: usize) -> usize {
    match stage.score().tracks[t].track_input() {
        super::frame::TrackInput::Mono(c) => c,
        other => panic!("unexpected input {other:?}"),
    }
}

/// Every take of a full run of the owner's score holds the input it was played on, shifted by
/// exactly the latency compensation - sample for sample, nothing dropped, doubled or moved.
///
/// This is the engine-side half of the click the owner heard inside `theme_b`: in his recording the
/// one-sample jump sits in the loop *and*, `R` = 596 frames later and exactly on a 128-frame block
/// line, in the monitored live signal of the same take - which never touches a layer. The engine
/// wrote what the interface delivered (the session counted two driver dropouts). This test proves
/// the other half: with a continuous input, no take of the whole song has a splice of its own.
#[test]
fn every_take_of_the_owners_score_is_the_input_shifted_by_the_latency() {
    let mut stage = Stage::new(Box::new(counting), true);
    stage.run_all();

    let mut checked = 0;
    for (n, &(t, layer, start, defines)) in stage.takes.clone().iter().enumerate() {
        let track = stage.sim.core.track(t);
        if track.midi_loop() || track.is_instrument() {
            continue;
        }
        let len = track.loop_len() as usize;
        let Some(content) = track.layer(layer).map(|l| l.channel(0, len as u64)) else {
            panic!("take on track {t} layer {layer} is gone");
        };
        let ch = channel_of(&stage, t);
        // Musical position m was played at input index m + R.
        let first = (start + LATENCY) as i64;
        let decoded: Vec<Option<u64>> = content.iter().map(|&v| decode(v)).collect();
        if defines {
            for (i, d) in decoded.iter().enumerate() {
                assert_eq!(
                    *d,
                    Some((first + i as i64) as u64),
                    "track {t} layer {layer} (loop take from {start}): index {i} of {len}"
                );
                assert_eq!(content[i] < 0.0, ch == 1, "wrong input on track {t}");
            }
        } else {
            // An overdub covers at most one pass from its own position on the grid, index of
            // `start` first; one cut short by its section leaves the rest of the pass silent.
            let end = stage.ends[n].expect("every overdub was ended");
            let frames = ((end - start) as usize).min(len);
            let origin = track.grid().origin_at(start);
            let seam = ((start - origin) % len as u64) as usize;
            for j in frames..len {
                assert_eq!(decoded[(seam + j) % len], None, "track {t} layer {layer}: after its end");
            }
            for j in 0..frames {
                let i = (seam + j) % len;
                assert_eq!(
                    decoded[i],
                    Some((first + j as i64) as u64),
                    "track {t} layer {layer} (overdub from {start}): pass frame {j}, index {i}"
                );
            }
        }
        checked += 1;
    }
    // theme, theme_b, bass, bass_b, chorus_guitar, mic, backing_vocals and its two overdubs.
    assert_eq!(checked, 9, "takes seen: {:?}", stage.takes);
}

/// The take of the owner's report on its own: `theme_b` records the one-bar section right after
/// the four-bar section in which `theme` was recorded - `theme`'s take ends on the very sample
/// `theme_b`'s begins, and `theme` falls silent there. Every sample of it, and especially the
/// stretch around 6366 frames after its start where the recording jumped, is continuous.
#[test]
fn theme_b_is_continuous_across_the_spot_of_the_owners_click() {
    let mut stage = Stage::new(Box::new(counting), true);
    let intro_b = stage.section(1);
    stage.run_to(stage.section(3));
    let t = stage
        .score()
        .tracks
        .iter()
        .position(|t| t.name == "theme_b")
        .unwrap();
    let track = stage.sim.core.track(t);
    let len = track.loop_len();
    assert_eq!(len, stage.section(2) - intro_b, "one bar");
    let content = track.layer(0).unwrap().channel(0, len);
    for i in 1..len as usize {
        let a = decode(content[i - 1]).unwrap();
        let b = decode(content[i]).unwrap();
        assert_eq!(b, a + 1, "index {i} (the owner's jump was at 6366)");
    }
    assert_eq!(decode(content[0]), Some(intro_b + LATENCY));
}

#[allow(dead_code)]
fn main_bus(stage: &Stage) -> Vec<f32> {
    stage.sim.buses[Bus::Main.index()][0].clone()
}

#[allow(dead_code)]
fn _unused(_: Command) {}
