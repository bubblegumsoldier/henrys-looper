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
        Self::of(SCORE, played, mixer)
    }

    fn of(yaml: &str, played: Played, mixer: bool) -> Self {
        let score = compile_score(yaml).expect("die Partitur uebersetzt");
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
    // theme, theme_b, bass, bass_b, chorus_guitar, mic, backing_vocals and its overdub.
    assert_eq!(checked, 8, "takes seen: {:?}", stage.takes);
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

// ---------------------------------------------------------------------------------------------
// No clicks
// ---------------------------------------------------------------------------------------------

/// Frames of one declicking fade: 10 ms. Written out here rather than taken from the engine, so the
/// bound below is the test's own.
const FADE: f64 = RATE as f64 * 0.010;

/// The partials of the two inputs: (frequency in Hz, amplitude). The guitar is what the piezo
/// gave in the recording - most of its energy at 41 and 73 Hz - plus a slow drift; the voice
/// likewise low. All incommensurate, so no two passes of a loop are alike and a loop's first and
/// last sample practically never meet.
const GUITAR: [(f64, f64); 3] = [(41.0, 0.25), (73.0, 0.15), (0.37, 0.05)];
const VOICE: [(f64, f64); 3] = [(55.0, 0.2), (97.0, 0.1), (0.23, 0.05)];

fn partials(ch: usize) -> &'static [(f64, f64); 3] {
    if ch == 0 { &VOICE } else { &GUITAR }
}

fn low(k: u64, ch: usize) -> f32 {
    let t = k as f64 / f64::from(RATE);
    partials(ch)
        .iter()
        .enumerate()
        .map(|(n, (f, a))| a * (std::f64::consts::TAU * f * t + n as f64).sin())
        .sum::<f64>() as f32
}

/// What one signal on the bus may do from one sample to the next, derived from the input itself.
///
/// A sum of partials `a sin(2 pi f t)` changes by at most `sum 2 pi f a / rate` per sample (slope)
/// and its slope by at most `sum (2 pi f / rate)^2 a` (curvature). A raised-cosine fade of `F`
/// frames over a difference of at most `2A` (`A` the peak) adds `pi A / F` of slope and
/// `(pi / F)^2 A + (pi / F) slope` of curvature. The bus carries at most `signals` such signals
/// at once. A click - a step of size `s` between two samples - shows as `s` in both differences, so
/// it is caught down to the curvature bound: a few ten-thousandths of full scale.
fn bounds(signals: usize) -> (f64, f64) {
    let mut worst = (0.0f64, 0.0f64);
    for ch in 0..2 {
        let rate = f64::from(RATE);
        let peak: f64 = partials(ch).iter().map(|(_, a)| a).sum();
        let slope: f64 = partials(ch)
            .iter()
            .map(|(f, a)| std::f64::consts::TAU * f * a / rate)
            .sum();
        let curve: f64 = partials(ch)
            .iter()
            .map(|(f, a)| (std::f64::consts::TAU * f / rate).powi(2) * a)
            .sum();
        let w = std::f64::consts::PI / FADE;
        let s = slope + w * peak;
        let c = curve + w * w * peak + w * slope;
        worst = (worst.0.max(s), worst.1.max(c));
    }
    (worst.0 * signals as f64, worst.1 * signals as f64)
}

/// Every audio track plus both monitored inputs: the most signals the Main bus can sum.
fn signals(stage: &Stage) -> usize {
    stage.score().tracks.iter().filter(|t| !t.is_instrument()).count() + 2
}

/// The largest first and second difference of the Main bus (both sides) in `from..to`, with where.
fn roughness(stage: &Stage, from: u64, to: u64) -> ((f64, u64), (f64, u64)) {
    let mut step = (0.0f64, 0u64);
    let mut curve = (0.0f64, 0u64);
    for side in &stage.sim.buses[Bus::Main.index()] {
        for p in from.max(2)..to {
            let (a, b, c) = (
                f64::from(side[p as usize - 2]),
                f64::from(side[p as usize - 1]),
                f64::from(side[p as usize]),
            );
            if (c - b).abs() > step.0 {
                step = ((c - b).abs(), p);
            }
            if (c - 2.0 * b + a).abs() > curve.0 {
                curve = ((c - 2.0 * b + a).abs(), p);
            }
        }
    }
    (step, curve)
}

/// Positions in `from..to` where the Main bus bends by more than `bound` on either side.
fn clicks(stage: &Stage, from: u64, to: u64, bound: f64) -> usize {
    (from.max(2)..to)
        .filter(|&p| {
            stage.sim.buses[Bus::Main.index()].iter().any(|side| {
                let (a, b, c) = (
                    f64::from(side[p as usize - 2]),
                    f64::from(side[p as usize - 1]),
                    f64::from(side[p as usize]),
                );
                (c - 2.0 * b + a).abs() > bound
            })
        })
        .count()
}

/// The owner's score with `edits` applied - each `(from, to)` replaces one exact piece of text.
fn variant(edits: &[(&str, &str)]) -> String {
    let mut yaml = SCORE.to_string();
    for (from, to) in edits {
        assert_eq!(yaml.matches(from).count(), 1, "edit {from:?} must hit exactly once");
        yaml = yaml.replacen(from, to, 1);
    }
    yaml
}

/// Play `yaml` to two bars into its last section with low-frequency material and hold Main to the
/// bound. Returns the stage for further checks.
fn assert_no_click(yaml: &str, label: &str) -> Stage {
    let mut stage = Stage::of(yaml, Box::new(low), false);
    let from = stage.section(0);
    stage.run_all();
    let to = stage.sim.pos();
    let (max_step, max_curve) = bounds(signals(&stage));
    let ((step, at_step), (curve, at_curve)) = roughness(&stage, from, to);
    let clicks = clicks(&stage, from, to, max_curve);
    eprintln!(
        "{label}: step {step:.5} at {at_step} (bound {max_step:.5}), curvature {curve:.6} at \
         {at_curve} (bound {max_curve:.6}), {clicks} positions over the bound"
    );
    assert!(step <= max_step, "{label}: step {step} at {at_step} over {max_step}");
    assert!(curve <= max_curve, "{label}: click, curvature {curve} at {at_curve} over {max_curve}");
    stage
}

/// The owner's whole score, played with low-frequency material: every take start and end, every
/// wrap of every loop, every start and stop at a section line, the re-anchored comebacks (theme
/// and bass in verse 1 and both choruses, `restart: true` in verse 2, the backing vocals' overdub
/// in chorus 3 on their returning loop), the monitoring switched on and off by the sections, and
/// the end where everything stops. The Main bus never steps or bends by more than the input itself
/// and a fade can make it.
#[test]
fn the_owners_score_plays_without_a_single_click() {
    let stage = assert_no_click(SCORE, "score");
    // And the material really is the kind that clicks: without a fade, the wrap of the theme
    // alone - its last recorded sample against its first - is many times what the bus may do.
    let (_, max_curve) = bounds(signals(&stage));
    let theme = stage.sim.core.track(0);
    let len = theme.loop_len();
    let raw = theme.layer(0).unwrap().channel(0, len);
    let jump = f64::from((raw[len as usize - 1] - raw[0]).abs());
    assert!(jump > 20.0 * max_curve, "the theme's raw seam is only {jump}");
}

/// `{state: play, monitor: true}`: a loop playing with its input heard on top - switched on and
/// off at section lines, handed over between tracks on the same input.
#[test]
fn play_with_monitor_in_the_owners_score_does_not_click() {
    let yaml = variant(&[
        ("      backing_vocals: play\n", "      backing_vocals: {state: play, monitor: true}\n"),
        (
            "      theme: {state: play, restart: true}\n",
            "      theme: {state: play, restart: true, monitor: true}\n",
        ),
        (
            "      theme_b: play\n      bass_b: play\n",
            "      theme_b: {state: play, monitor: true}\n      bass_b: play\n",
        ),
    ]);
    assert_no_click(&yaml, "play+monitor");
}

/// `skip_end`: takes whose end is not recorded. On playback the loop runs into the silence through
/// the cut's own fade, and out of it through its seam fade into the pre-roll - no click at the cut
/// and none at the seam behind it.
#[test]
fn skip_end_in_the_owners_score_does_not_click_on_playback() {
    let yaml = variant(&[
        ("      theme: record\n", "      theme: {state: record, skip_end: 0.5}\n"),
        (
            "      chorus_guitar: {state: record, bars: 2}\n",
            "      chorus_guitar: {state: record, bars: 2, skip_end: 1}\n",
        ),
        ("      mic: record\n", "      mic: {state: record, skip_end: 2}\n"),
        ("      backing_vocals: overdub\n", "      backing_vocals: {state: overdub, skip_end: 1}\n"),
    ]);
    let stage = assert_no_click(&yaml, "skip_end");
    // The cut is really there: the theme's last half beat is silence.
    let theme = stage.sim.core.track(0);
    let len = theme.loop_len();
    let raw = theme.layer(0).unwrap().channel(0, len);
    assert_eq!(raw[len as usize - 100], 0.0, "the skipped end is silent");
}

/// `monitor: always` and `monitor_fade`: the guitar and the voice heard from the load on, giving
/// way to every take on their input and coming back, with the configured fades (quadratic, beats
/// long) and with plain switches (the 10 ms ramp) on the other tracks.
#[test]
fn monitor_always_and_monitor_fade_in_the_owners_score_do_not_click() {
    let yaml = variant(&[
        ("time_signature: 4/4\n", "time_signature: 4/4\nmonitor_fade: {in: 1, out: 0.5}\n"),
        ("  mic: {input: 1}\n", "  mic: {input: 1, monitor: always, monitor_fade: {in: 2, out: 1}}\n"),
        ("  chorus_guitar: {input: 2}\n", "  chorus_guitar: {input: 2, monitor: always}\n"),
    ]);
    assert_no_click(&yaml, "monitor always + fade");
    // The same without any configured fade: every hand-over is the 10 ms ramp.
    let yaml = variant(&[
        ("  mic: {input: 1}\n", "  mic: {input: 1, monitor: always}\n"),
        ("  chorus_guitar: {input: 2}\n", "  chorus_guitar: {input: 2, monitor: always}\n"),
    ]);
    assert_no_click(&yaml, "monitor always");
}

/// `stack` - a new layer in every pass, each with its own seam and its pre-roll taken from the
/// end of the pass before - on a two-bar loop that comes back re-anchored after a one-bar pause,
/// plus an overdub cut short by its section: none of it clicks either.
#[test]
fn stacked_and_cut_short_layers_play_without_a_click() {
    let yaml = "\
bpm: 94
tracks:
  guitar: {input: 2}
  voice: {input: 1}
sections:
  - id: take
    bars: 2
    autorelease: true
    tracks:
      guitar: record
  - id: stack
    bars: 6
    autorelease: true
    tracks:
      guitar: stack
      voice: record
  - id: pause
    bars: 1
    autorelease: true
    tracks:
      voice: play
  - id: back
    bars: 3
    autorelease: true
    tracks:
      guitar: play
      voice: {state: overdub, restart: true}
  - id: end
    bars: 2
    autorelease: false
    tracks:
      guitar: play
";
    let mut stage = Stage::of(yaml, Box::new(low), false);
    let from = stage.section(0);
    let to = stage.section(4) + (stage.bar(2) - stage.bar(0));
    stage.run_to(to);
    assert!(stage.sim.core.track(0).layer_count() >= 3, "stacked layers");
    let (max_step, max_curve) = bounds(4);
    let ((step, at_step), (curve, at_curve)) = roughness(&stage, from, to);
    let clicks = clicks(&stage, from, to, max_curve);
    eprintln!("stack: step {step:.5} at {at_step}, curvature {curve:.6} at {at_curve}, {clicks} over");
    assert!(step <= max_step, "step {step} at {at_step}");
    assert!(curve <= max_curve, "click: curvature {curve} at {at_curve}");
}

/// "Stop all" pressed mid-chorus, mid-loop, mid-take: everything is silent from the sample it
/// takes effect on, and the way there is a fade, not a cut.
#[test]
fn stop_all_fades_out_and_is_silent_on_its_sample() {
    for (section, offset) in [(5usize, 12_345u64), (8, 70_001), (11, 3_333)] {
        let mut stage = Stage::new(Box::new(low), false);
        let press = stage.section(section) + offset;
        stage.run_to(press);
        let est = stage.sim.pos();
        let soon = stage.runner.scheduler().soon(est);
        stage.runner.stop_all(est);
        stage.pump();
        stage.run_to(soon + 4 * u64::from(BLOCK) * 10);
        let (max_step, max_curve) = bounds(signals(&stage));
        let ((step, at_step), (curve, at_curve)) =
            roughness(&stage, stage.section(0), stage.sim.pos());
        assert!(step <= max_step, "{section}: step {step} at {at_step}");
        assert!(curve <= max_curve, "{section}: curvature {curve} at {at_curve}");
        let loud_before = (soon - 2_000..soon - 500)
            .any(|p| stage.sim.bus(Bus::Main, p).l.abs() > 0.01);
        assert!(loud_before, "{section}: something was playing");
        for p in soon..stage.sim.pos() {
            let f = stage.sim.bus(Bus::Main, p);
            assert_eq!((f.l, f.r), (0.0, 0.0), "{section}: sound at {p}, stop at {soon}");
        }
    }
}

/// The fades, spelled out on one loop: which samples they touch and what they blend with.
///
/// A one-bar loop of the counting input (every sample names its input frame) is recorded, played,
/// re-anchored in the middle and stopped on its seam. Checked against the formula, independently
/// of the engine's own code:
///
/// * the pre-roll is exactly the input of the `edge` frames before the take's first frame;
/// * a start on the seam fades in the pre-roll over the `edge` frames before it - nothing before
///   that - and the first frame plays untouched;
/// * every pass is the take sample for sample, except its last `edge` frames, which crossfade
///   (raised cosine, amplitude-complementary) into the pre-roll; the next pass begins untouched;
/// * a re-anchoring fades the old position out and the new top's pre-roll in over the frames
///   before it, and the new top plays untouched;
/// * a stop on the seam lets the pass fade out through its own seam fade and is silent from the
///   seam on - the pre-roll of a pass nobody hears is not added.
#[test]
fn the_fades_touch_only_the_frames_before_each_edge() {
    use super::timeline::TimeSignature;
    use super::track::{edge_frames, fade_in, fade_out};
    let timeline = Timeline::new(RATE, 94.0, TimeSignature::new(4, 4));
    let len = timeline.span_bars(0, 1);
    let edge = edge_frames(RATE);
    assert_eq!(edge as f64, FADE);
    let mut sim = Sim::new(
        SimSpec {
            bpm: 94.0,
            bars: 1,
            latency: LATENCY,
            roundtrip: LATENCY,
            tracks: vec![TrackSpec::on(1)],
            input_channels: 2,
            loopback_channel: None,
            ..Default::default()
        },
        Box::new(counting),
    );
    let start = timeline.bar_start(2);
    let end = start + len;
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.send(Command::StartPlay { track: 0, at: end });
    let restart = end + 2 * len + 1_234;
    sim.send(Command::RestartLoop {
        track: 0,
        at: restart,
        play: false,
    });
    let stop = restart + 2 * len;
    sim.send(Command::StopPlay { track: 0, at: stop });
    sim.run_to(stop + len);

    let track = sim.core.track(0);
    assert_eq!(track.loop_len(), len);
    let take = track.layer(0).unwrap().channel(0, len);
    // Musical position m was played at input frame m + R; the pre-roll precedes the take's start.
    let pre: Vec<f32> = (0..edge).map(|j| counting(start - edge + j + LATENCY, 1)).collect();
    for (j, &p) in pre.iter().enumerate() {
        assert_eq!(track.pre_roll(0, j).l, p, "pre-roll frame {j}");
    }
    let out = |p: u64| sim.bus(Bus::Main, p).l;
    // A pass's own sample at index i, with its seam crossfade.
    let heard = |i: u64| -> f32 {
        if i >= len - edge {
            let d = i - (len - edge);
            take[i as usize] * fade_out(d, edge) + pre[d as usize] * fade_in(d, edge)
        } else {
            take[i as usize]
        }
    };

    // The start on the seam at `end`.
    assert_eq!(out(end - edge - 1), 0.0, "silent before the fade-in");
    for d in 0..edge {
        assert_eq!(out(end - edge + d), pre[d as usize] * fade_in(d, edge), "fade-in {d}");
    }
    // Two passes, every sample.
    for p in end..restart - edge {
        assert_eq!(out(p), heard((p - end) % len), "pass sample at {p}");
    }
    assert_eq!(out(end), take[0], "the first frame untouched");
    assert_eq!(out(end + len), take[0], "every pass begins untouched");

    // The re-anchoring: old position fades out, the new top's pre-roll fades in.
    for d in 0..edge {
        let p = restart - edge + d;
        let old = heard((p - end) % len) * fade_out(d, edge);
        let new = pre[d as usize] * fade_in(d, edge);
        let got = out(p);
        assert!((got - (old + new)).abs() < 1e-6, "re-anchor fade {d}: {got} vs {}", old + new);
    }
    for p in restart..stop - edge {
        assert_eq!(out(p), heard((p - restart) % len), "new grid at {p}");
    }
    assert_eq!(out(restart), take[0], "the new top untouched");

    // The stop on the seam: the last pass runs out through its seam fade - without the pre-roll.
    for d in 0..edge {
        let i = len - edge + d;
        assert_eq!(out(stop - edge + d), take[i as usize] * fade_out(d, edge), "fade-out {d}");
    }
    for p in stop..stop + len {
        assert_eq!(out(p), 0.0, "silent from the stop on, at {p}");
    }
}

/// The recording's stems are as clean as the room: the dry stem reads the loop through the same
/// faded playback as the mix, and the live part of a stem fades in and out with monitoring and at
/// a take's edges. Every stem of the whole score, dry and processed, stays inside the bound for
/// the few signals one stem carries (loop, live input, and a fade between them).
#[test]
fn the_stems_of_the_owners_score_have_no_click_either() {
    use crate::capture::{CaptureSpec, Layout, Read, capture_channel, capture_pair};
    let mut stage = Stage::new(Box::new(low), false);
    let from = stage.section(0);
    let layout = Layout {
        tracks: stage
            .score()
            .tracks
            .iter()
            .map(|t| t.track_input().channels())
            .collect(),
        dry: true,
    };
    let (session, mut reader) = capture_pair(&CaptureSpec {
        start: from,
        layout: layout.clone(),
        sample_rate: RATE,
        max_block: BLOCK as usize,
        max_latency: LATENCY,
        queue_seconds: 400.0,
    });
    let (mut port, endpoint) = capture_channel();
    stage.sim.core.attach_capture(endpoint);
    port.install(session).unwrap();
    stage.run_all();
    let mut frames = Vec::new();
    while let Read::Frames(_) = reader.read(&mut frames, 1 << 20) {}
    let width = layout.width();
    let total = frames.len() / width;
    assert!(total as u64 > stage.section(12) - from - 100_000, "{total} frames captured");

    let (max_step, max_curve) = bounds(3);
    let mut worst = (0.0f64, String::new());
    for (t, track) in stage.score().tracks.iter().enumerate() {
        if track.is_instrument() {
            continue;
        }
        let stem = layout.stem_offset(t);
        let dry = layout.dry_offset(t).unwrap();
        for (what, offset) in [("stem", stem), ("stem R", stem + 1), ("dry", dry)] {
            let v = |i: usize| f64::from(frames[i * width + offset]);
            for i in 2..total {
                let step = (v(i) - v(i - 1)).abs();
                let curve = (v(i) - 2.0 * v(i - 1) + v(i - 2)).abs();
                assert!(step <= max_step, "{} {what}: step {step} at frame {i}", track.name);
                if curve > worst.0 {
                    worst = (curve, format!("{} {what} at frame {i}", track.name));
                }
            }
        }
    }
    eprintln!("stems: largest curvature {:.6} ({}), bound {max_curve:.6}", worst.0, worst.1);
    assert!(worst.0 <= max_curve, "click in {}: {}", worst.1, worst.0);
}

/// What loop playback costs the callback: eight mono tracks with four layers each, all playing
/// a one-bar loop of low-frequency material at 94 BPM (a seam crossfade every 2.55 s on every
/// layer), 128-frame blocks. Prints nanoseconds per frame for the whole engine block (the
/// simulation's own bookkeeping included, the same before and after) - run it in release:
///
/// ```text
/// cargo test --release -p looper-engine --lib bench_loop_playback -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn bench_loop_playback() {
    use super::timeline::TimeSignature;
    let timeline = Timeline::new(RATE, 94.0, TimeSignature::new(4, 4));
    let bar = timeline.span_bars(0, 1);
    let tracks = (0..8).map(|_| TrackSpec::on(1)).collect();
    let mut sim = Sim::new(
        SimSpec {
            bpm: 94.0,
            bars: 1,
            latency: LATENCY,
            roundtrip: LATENCY,
            tracks,
            input_channels: 2,
            loopback_channel: None,
            spare_slots: 8,
            ..Default::default()
        },
        Box::new(low),
    );
    let start = timeline.bar_start(1);
    for t in 0..8 {
        sim.send(Command::StartRecord { track: t, at: start });
        sim.send(Command::StopRecord { track: t, at: start + bar });
        sim.send(Command::StartPlay { track: t, at: start + bar });
    }
    for n in 1..4u64 {
        sim.run_to(start + n * bar - 4 * u64::from(BLOCK));
        for t in 0..8 {
            sim.send(Command::StartOverdub { track: t, at: start + n * bar });
            sim.send(Command::StopRecord { track: t, at: start + (n + 1) * bar });
        }
    }
    sim.run_to(start + 5 * bar);
    for t in 0..8 {
        assert_eq!(sim.core.track(t).layer_count(), 4);
    }
    let blocks = 4_000u64;
    let mut seam_blocks = Vec::new();
    let mut other_blocks = Vec::new();
    let t0 = std::time::Instant::now();
    for _ in 0..blocks {
        // Blocks that end in the last fade before the common seam of all 32 layers.
        let into = (sim.pos() + u64::from(BLOCK) - start) % bar;
        let at_seam = into >= bar - 480 || into < u64::from(BLOCK);
        let b0 = std::time::Instant::now();
        sim.run_blocks(1);
        let d = b0.elapsed().as_nanos() as f64;
        if at_seam { seam_blocks.push(d) } else { other_blocks.push(d) }
    }
    let ns = t0.elapsed().as_nanos() as f64 / (blocks * u64::from(BLOCK)) as f64;
    let budget = 1e9 / f64::from(RATE);
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2] / f64::from(BLOCK)
    };
    eprintln!(
        "loop playback, 8 tracks x 4 layers: {ns:.1} ns/frame = {:.2} % of the callback budget; \
         median block {:.1} ns/frame, in the fade before a seam of all 32 layers {:.1} ns/frame",
        ns / budget * 100.0,
        median(&mut other_blocks),
        median(&mut seam_blocks)
    );
}

#[allow(dead_code)]
fn _unused(_: Command) {}
