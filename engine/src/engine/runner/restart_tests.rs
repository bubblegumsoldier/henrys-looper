//! A loop that comes back in starts from its top.
//!
//! A loop is read as `(position - origin) mod loop_len`, so without help it stays phase-locked to
//! the song from its first take on, and a section whose length is not a multiple of the loop
//! length leaves it out of phase for everything after it. These tests drive the real engine core
//! through `Sim`, the way `tests.rs` does, and look at what is actually heard.

use super::*;
use crate::engine::bus::Bus;
use crate::engine::command::{Command, Refusal};
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Played, Sim, SimSpec, TrackSpec};
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;

/// A signal whose value identifies the input frame it came from - on every input channel, so two
/// tracks on different inputs record distinguishable content too.
fn fingerprint(k: u64) -> f32 {
    ((k % 30_011) as f32) / 30_011.0 - 0.5
}

fn everywhere() -> Played {
    Box::new(|k, _| fingerprint(k))
}

/// A runner and the engine, driven together block by block, with the release button pressed in the
/// last bar of every section that waits for it.
struct Run {
    sim: Sim,
    runner: Runner,
    sent: Vec<Command>,
    first_bar: u64,
    /// Bar offsets of every section's start from the first one, plus the end.
    offsets: Vec<u64>,
    waits: Vec<bool>,
    released: Vec<bool>,
}

impl Run {
    fn new(yaml: &str, tracks: Vec<TrackSpec>) -> Self {
        let score = compile_score(yaml).expect("die Partitur uebersetzt");
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
        let mut offsets = vec![0u64];
        for s in &score.sections {
            offsets.push(offsets.last().unwrap() + u64::from(s.bars));
        }
        let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
        let input_channels = 2;
        let sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                block: BLOCK as usize,
                tracks,
                input_channels,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 6,
                ..Default::default()
            },
            everywhere(),
        );
        let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        runner.start(0);
        let first_bar = runner.armed_at().expect("armiert").1;
        let released = vec![false; waits.len()];
        Self {
            sim,
            runner,
            sent: Vec::new(),
            first_bar,
            offsets,
            waits,
            released,
        }
    }

    /// First sample of the bar `bars` bars after the first section's start.
    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    /// First sample of section `index`.
    fn section(&self, index: usize) -> u64 {
        self.bar(self.offsets[index])
    }

    fn pump(&mut self) {
        for cmd in self.runner.take_commands() {
            self.sent.push(cmd);
            self.sim.send(cmd);
        }
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.pump();
            self.runner.tick(self.sim.pos());
            self.pump();
            if let Some(index) = self.runner.view(self.sim.pos()).section {
                let last = self.bar(self.offsets[index + 1].saturating_sub(1));
                if self.waits[index] && !self.released[index] && self.sim.pos() >= last {
                    self.released[index] = true;
                    self.runner.next(self.sim.pos());
                }
            }
            self.sim.run_blocks(1);
        }
    }

    /// Run past the end of the score.
    fn finish(&mut self) {
        let end = self.section(self.offsets.len() - 1) + 4 * BLOCK as u64;
        self.run_to(end);
        self.pump();
        self.sim.run_blocks(16);
    }

    fn of(&self, track: usize) -> Vec<Command> {
        self.sent
            .iter()
            .copied()
            .filter(|c| c.track() == Some(track) && c.at().is_some())
            .collect()
    }

    /// Whether `track` reads loop index `index` at `pos`: what it reads is compared with its first
    /// layer over a run of 64 frames. A loop a bar out of phase reads entirely different values.
    fn reads_index(&self, track: usize, pos: u64, index: u64) -> bool {
        let t = self.sim.core.track(track);
        let len = t.loop_len();
        let Some(layer) = t.layer(0) else {
            return false;
        };
        let content = layer.content(len);
        (0..64u64).all(|j| {
            let i = ((index + j) % len) as usize;
            (content[i] - t.read(pos + j).l).abs() < 1e-7
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The owner's case: a one-bar section between two-bar loops
// ---------------------------------------------------------------------------------------------

/// "The Hardest Part", as far as the problem reaches: `theme` and `bass` are two-bar loops in phase
/// with bar 0; the one-bar `verse_1_b` silences them; `chorus_1_record` brings them back - and
/// they have to begin with their first bar there, not with their second.
const HARDEST: &str = "\
title: The Hardest Part (excerpt)
bpm: 92
time_signature: 4/4
tracks:
  theme: {input: 2}
  bass: {input: 2}
  theme_b: {input: 2}
  chorus_guitar: {input: 2}
  mic: {input: 1}
sections:
  - id: intro_record
    bars: 2
    autorelease: true
    tracks:
      theme: record
  - id: intro
    bars: 2
    autorelease: true
    tracks:
      theme: play
  - id: verse_1
    bars: 4
    autorelease: true
    tracks:
      theme: play
      mic: hear_through
  - id: verse_1_bass
    bars: 2
    autorelease: true
    tracks:
      theme: play
      bass: record
      mic: hear_through
  - id: verse_1_end
    bars: 2
    autorelease: true
    tracks:
      theme: play
      bass: play
      mic: hear_through
  - id: verse_1_b
    bars: 1
    autorelease: true
    tracks:
      theme_b: record
      mic: hear_through
  - id: chorus_1_record
    bars: 2
    autorelease: true
    tracks:
      theme: play
      bass: play
      chorus_guitar: record
      mic: hear_through
  - id: chorus_1
    bars: 4
    autorelease: true
    tracks:
      theme: play
      bass: play
      chorus_guitar: play
      mic: hear_through
  - id: verse_2
    bars: 8
    autorelease: true
    tracks:
      theme: play
      bass: play
      mic: hear_through
  - id: verse_2_b
    bars: 1
    autorelease: true
    tracks:
      theme_b: play
      mic: hear_through
  - id: chorus_2
    bars: 6
    autorelease: true
    tracks:
      theme: play
      bass: play
      chorus_guitar: play
      mic: hear_through
  - id: end
    bars: 1
    autorelease: true
";

fn hardest_tracks() -> Vec<TrackSpec> {
    vec![
        TrackSpec::on(1),
        TrackSpec::on(1),
        TrackSpec::on(1),
        TrackSpec::on(1),
        TrackSpec::on(0),
    ]
}

#[test]
fn a_loop_that_comes_back_after_a_one_bar_section_starts_from_its_top() {
    let mut run = Run::new(HARDEST, hardest_tracks());
    let chorus = run.section(6);
    // A few blocks into the chorus: everything for its start has happened.
    run.run_to(chorus + 8 * BLOCK as u64);
    for track in [0usize, 1] {
        assert!(run.sim.core.track(track).playing(), "Track {track} spielt im Refrain");
        for i in [0u64, 1, 100, 5_000] {
            assert!(
                run.reads_index(track, chorus + i, i),
                "Track {track} beginnt den Refrain mit seinem ersten Takt (Index {i})"
            );
        }
    }

    // Chorus 2 comes after verse 2 (8 bars, theme and bass keep playing, chorus_guitar silent) and
    // the second one-bar section: every loop that comes back starts from its top again - so theme,
    // bass and the chorus guitar, all re-anchored on the same sample, are in phase with each other.
    let chorus_2 = run.section(10);
    run.run_to(chorus_2 + 8 * BLOCK as u64);
    for track in [0usize, 1, 3] {
        for i in [0u64, 1, 7_777] {
            assert!(
                run.reads_index(track, chorus_2 + i, i),
                "Track {track} beginnt Refrain 2 mit seinem ersten Takt (Index {i})"
            );
        }
    }
    run.finish();
    let status = run.sim.latest_status().expect("Status");
    assert_eq!(status.refusal, Refusal::None);
    assert_eq!(status.ignored_commands, 0, "die Engine hat nichts abgelehnt");
}

/// The same, heard: what comes out of the main bus at the top of the chorus is the first bar of
/// both loops, summed - not their second bar.
#[test]
fn the_chorus_sounds_with_the_first_bar_of_both_loops() {
    let yaml = "\
bpm: 92
tracks:
  theme: {input: 2}
  bass: {input: 1}
sections:
  - id: theme
    bars: 2
    autorelease: true
    tracks:
      theme: record
  - id: bass
    bars: 2
    autorelease: true
    tracks:
      theme: play
      bass: record
  - id: b
    bars: 1
    autorelease: true
  - id: chorus
    bars: 2
    autorelease: true
    tracks:
      theme: play
      bass: play
  - id: end
    bars: 1
    autorelease: true
";
    let mut run = Run::new(yaml, vec![TrackSpec::on(1), TrackSpec::on(0)]);
    let chorus = run.section(3);
    run.run_to(chorus + 2_000 * BLOCK as u64 / 100);
    let theme = run.sim.core.track(0);
    let bass = run.sim.core.track(1);
    let (lt, lb) = (theme.loop_len(), bass.loop_len());
    let (ct, cb) = (
        theme.layer(0).unwrap().content(lt).to_vec(),
        bass.layer(0).unwrap().content(lb).to_vec(),
    );
    for i in [0u64, 1, 2, 64, 127, 128, 2_000] {
        let heard = run.sim.bus(Bus::Main, chorus + i).l;
        let want = ct[i as usize] + cb[i as usize];
        assert!(
            (heard - want).abs() < 1e-5,
            "Refrain, Frame {i}: {heard} statt {want} (erster Takt beider Loops)"
        );
    }
    // And the bar before the chorus is silent - the one-bar section has nothing playing - up to the
    // last fade length, in which both loops fade in what leads into their first frame (their
    // pre-roll), so the chorus does not begin with a click.
    let edge = crate::engine::track::edge_frames(48_000);
    let before = run.sim.bus(Bus::Main, chorus - edge - 1).l;
    assert!(before.abs() < 1e-6, "{before}");
}


/// Only the loops that come back are re-anchored, each exactly once per comeback, and only where
/// the section does not begin on their grid anyway: theme and bass at chorus 1, theme, bass and
/// the chorus guitar at chorus 2. The one-bar `theme_b` is always on its grid; a track that keeps
/// playing (theme and bass through verse 2) gets nothing.
#[test]
fn only_loops_that_come_back_off_their_grid_are_re_anchored() {
    let mut run = Run::new(HARDEST, hardest_tracks());
    run.finish();
    let restarts: Vec<(usize, u64)> = run
        .sent
        .iter()
        .filter_map(|c| match *c {
            Command::RestartLoop { track, at, play } => {
                assert!(play, "jeder dieser Neuanfaenge startet die Spur");
                Some((track, at))
            }
            _ => None,
        })
        .collect();
    let (c1, c2) = (run.section(6), run.section(10));
    assert_eq!(restarts, vec![(0, c1), (1, c1), (0, c2), (1, c2), (3, c2)]);
    // The `StartPlay` of a comeback is still the one it always was, on the same sample.
    for track in [0usize, 1] {
        assert!(run.of(track).contains(&Command::StartPlay { track, at: c1 }));
    }
}

/// The owner's second wish: a four-bar loop through a six-bar chorus ("one and a half loops"),
/// and the next section begins with the loop's top again - asked for with `restart: true`,
/// because the track plays across the boundary. Without it the loop carries on in its middle,
/// exactly as before.
#[test]
fn restart_true_starts_a_loop_that_keeps_playing_from_its_top() {
    let score = |restart: &str| {
        format!(
            "\
bpm: 92
tracks:
  drums: {{input: 1}}
sections:
  - id: groove
    bars: 4
    autorelease: true
    tracks:
      drums: record
  - id: chorus
    bars: 6
    autorelease: true
    tracks:
      drums: play
  - id: verse
    bars: 4
    autorelease: true
    tracks:
      drums: {restart}
  - id: end
    bars: 1
    autorelease: true
"
        )
    };
    let mut run = Run::new(&score("{state: play, restart: true}"), vec![TrackSpec::on(0)]);
    let verse = run.section(2);
    run.run_to(verse + 8 * BLOCK as u64);
    assert!(run.sim.core.track(0).playing());
    for i in [0u64, 1, 4_000] {
        assert!(run.reads_index(0, verse + i, i), "Neuanfang, Index {i}");
    }
    // Just before the boundary it was 64 frames short of the middle of the loop: ten bars after
    // its origin, two and a half passes.
    let len = run.sim.core.track(0).loop_len();
    let middle = (verse - run.section(0)) % len;
    assert!(middle.abs_diff(len / 2) <= 2, "{middle} / {len}");
    assert!(run.reads_index(0, verse - 64, middle - 64));
    run.finish();
    assert_eq!(
        run.of(0)
            .iter()
            .filter(|c| matches!(c, Command::RestartLoop { .. }))
            .count(),
        1
    );
    assert_eq!(run.sim.latest_status().unwrap().ignored_commands, 0);

    // Without `restart` the loop runs on from where it is: no command, the middle of the loop.
    let mut plain = Run::new(&score("play"), vec![TrackSpec::on(0)]);
    let verse = plain.section(2);
    plain.run_to(verse + 8 * BLOCK as u64);
    assert!(plain.reads_index(0, verse, middle));
    plain.finish();
    assert!(!plain.sent.iter().any(|c| matches!(c, Command::RestartLoop { .. })));
}

/// Release sections and loop quantisation: the re-anchoring lands on the sample the section
/// really begins on, wherever the release put it - here after the first pass of a three-bar
/// section that loops until Release, with a two-bar loop coming back.
#[test]
fn a_released_section_re_anchors_where_it_really_begins() {
    let yaml = "\
bpm: 120
tracks:
  theme: {input: 1}
  voice: {input: 2}
sections:
  - id: rec
    bars: 2
    autorelease: true
    tracks:
      theme: record
  - id: alone
    bars: 3
    autorelease: false
    quantize: loop
    tracks:
      voice: hear_through
  - id: back
    bars: 2
    autorelease: true
    tracks:
      theme: play
  - id: end
    bars: 1
    autorelease: true
";
    let mut run = Run::new(yaml, vec![TrackSpec::on(0), TrackSpec::on(1)]);
    // Released in the last bar of its first pass: `back` begins after three bars.
    let back = run.section(2);
    run.run_to(back + 8 * BLOCK as u64);
    assert_eq!(run.runner.view(run.sim.pos()).section, Some(2));
    for i in [0u64, 1, 333] {
        assert!(run.reads_index(0, back + i, i), "Index {i}");
    }
}

// ---------------------------------------------------------------------------------------------
// Scores whose sections are multiples of their loops: nothing changes
// ---------------------------------------------------------------------------------------------

/// Every example score plays without a single re-anchoring - the rule only acts where a loop would
/// otherwise come back off its grid, and these never do. No new command means the engine runs the
/// same code path, sample for sample: with no re-anchoring armed, `origin_at` is `origin` and
/// `sounds_at` is `playing`.
#[test]
fn scores_with_whole_loops_send_no_re_anchoring() {
    let scores = [
        include_str!("../../../../examples/henry-3-4.rust.yaml"),
        include_str!("../../../../examples/waltz-three-tracks-3-4.yaml"),
        include_str!("../../../../examples/solo-loop-4-4.yaml"),
        include_str!("../../../../examples/piano-loop-4-4.yaml"),
    ];
    for yaml in scores {
        let score = compile_score(yaml).expect("uebersetzt");
        let title = score.title.clone();
        let timeline = Timeline::new(RATE, score.bpm, score.signature());
        let lengths: Vec<u64> = score.sections.iter().map(|s| u64::from(s.bars)).collect();
        let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
        let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
        runner.start(0);
        let first = runner.armed_at().unwrap().1;
        let timeline = runner.scheduler().timeline;
        let mut starts = vec![first];
        for l in &lengths {
            starts.push(starts.last().unwrap() + l);
        }
        let end = timeline.bar_start(*starts.last().unwrap()) + 1;
        let mut sent = runner.take_commands();
        let mut released = vec![false; lengths.len()];
        let mut pos = 0;
        while pos < end + 10_000 && !runner.phase().is_over() {
            pos += 1_000;
            runner.tick(pos);
            if let Some(i) = runner.view(pos).section
                && waits[i]
                && !released[i]
                && pos >= timeline.bar_start(starts[i + 1] - 1)
            {
                released[i] = true;
                runner.next(pos);
            }
            sent.extend(runner.take_commands());
        }
        assert!(runner.phase().is_over(), "{title}: laeuft durch");
        assert!(
            !sent.iter().any(|c| matches!(c, Command::RestartLoop { .. })),
            "{title}: kein Neuanfang"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// A take shorter than its section: `{state: record, bars: 2}`
// ---------------------------------------------------------------------------------------------

/// The owner's chorus: six bars, in which the chorus guitar records bars 1-2 and plays them as a
/// two-bar loop for bars 3-6, while the lead vocal records all six bars in one take.
fn chorus(autorelease: bool) -> String {
    format!(
        "\
bpm: 92
tracks:
  chorus_guitar: {{input: 2}}
  mic: {{input: 1}}
sections:
  - id: intro
    bars: 1
    autorelease: true
  - id: chorus
    bars: 6
    autorelease: {autorelease}
    tracks:
      chorus_guitar: {{state: record, bars: 2}}
      mic: record
  - id: chorus_2
    bars: 6
    autorelease: true
    tracks:
      chorus_guitar: play
      mic: play
  - id: end
    bars: 1
    autorelease: true
"
    )
}

/// Roundtrip of the simulated interface: what is played at `m` arrives at `m + R`.
const R: u64 = 827;

#[test]
fn a_short_take_and_a_whole_section_take_share_one_section() {
    for autorelease in [true, false] {
        let mut run = Run::new(&chorus(autorelease), vec![TrackSpec::on(1), TrackSpec::on(0)]);
        let c = run.section(1);
        let two = run.bar(3);
        let c2 = run.section(2);
        assert_eq!(two, run.bar(run.offsets[1] + 2));

        // The guitar's take ends after two bars and its loop plays from there; the vocal's runs on.
        run.run_to(two + 8 * BLOCK as u64);
        {
            let guitar = run.sim.core.track(0);
            assert!(guitar.playing(), "die Gitarre spielt ab Takt 3 ihren Loop");
            assert_eq!(guitar.origin(), c);
            assert_eq!(guitar.loop_len(), two - c, "genau zwei Takte");
            assert!(run.sim.core.track(1).take().is_some(), "die Stimme nimmt weiter auf");
        }
        for i in [0u64, 1, 900] {
            assert!(run.reads_index(0, two + i, i), "Takt 3 ist der Anfang des Loops ({i})");
        }

        run.run_to(c2 + 8 * BLOCK as u64);
        assert_eq!(run.runner.view(run.sim.pos()).section, Some(2));
        for (track, name) in [(0usize, "Gitarre"), (1, "Stimme")] {
            let t = run.sim.core.track(track);
            // Phase: the next section begins on the top of both loops (to the sample the grid
            // and the separately rounded bar line allow).
            let phase = (c2 - t.origin()) % t.loop_len();
            assert!(
                phase <= 1 || phase >= t.loop_len() - 1,
                "{name}: Refrain 2 beginnt am Loop-Anfang ({phase})"
            );
            // Sample accuracy: loop index `i` holds what was played at `c + i`.
            let content = t.layer(0).unwrap().content(t.loop_len());
            for i in [0u64, 1, t.loop_len() / 2, t.loop_len() - 1] {
                assert!(
                    (content[i as usize] - fingerprint(c + i + R)).abs() < 1e-7,
                    "{name}, Index {i}"
                );
            }
        }
        assert_eq!(run.sim.core.track(1).loop_len(), c2 - c, "die Stimme: sechs Takte");

        // The commands: the guitar's take is two bars long, the vocal's six; no re-anchoring is
        // needed, the next section is on both grids.
        assert_eq!(
            run.of(0)[..3],
            [
                Command::StartRecord { track: 0, at: c },
                Command::StopRecord { track: 0, at: two },
                Command::StartPlay { track: 0, at: two },
            ]
        );
        assert_eq!(
            run.of(1)[..3],
            [
                Command::StartRecord { track: 1, at: c },
                Command::StopRecord { track: 1, at: c2 },
                Command::StartPlay { track: 1, at: c2 },
            ]
        );
        run.finish();
        assert!(!run.sent.iter().any(|c| matches!(c, Command::RestartLoop { .. })));
        let status = run.sim.latest_status().unwrap();
        assert_eq!(status.ignored_commands, 0, "autorelease {autorelease}");
    }
}

/// A short take that does not divide its section: the loop simply runs on, and a following
/// section in which the track keeps playing carries on in its phase - unless it comes back after
/// a pause, or the clip says `restart: true`. Here it comes back after a pause.
#[test]
fn a_short_take_that_does_not_divide_its_section_runs_on() {
    let yaml = "\
bpm: 120
tracks:
  g: {input: 1}
sections:
  - id: a
    bars: 4
    autorelease: true
    tracks:
      g: {state: record, bars: 3}
  - id: rest
    bars: 1
    autorelease: true
  - id: back
    bars: 3
    autorelease: true
    tracks:
      g: play
  - id: end
    bars: 1
    autorelease: true
";
    let mut run = Run::new(yaml, vec![TrackSpec::on(0)]);
    let a = run.section(0);
    let three = run.bar(3);
    run.run_to(three + 64 * BLOCK as u64);
    // Bar 4 of the section is the loop's first bar again.
    assert!(run.reads_index(0, three + 10, 10));
    let back = run.section(2);
    run.run_to(back + 8 * BLOCK as u64);
    assert!(run.reads_index(0, back + 5, 5));
    assert_eq!(run.sim.core.track(0).loop_len(), three - a);
}
