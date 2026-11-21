//! Scores whose sections are whole multiples of their loops sound **bit for bit** as they did before
//! loops were re-anchored at section starts.
//!
//! Each score is played through the real engine core (`Sim`) with a fingerprint on every input,
//! and a hash of every output sample of both buses is compared with the value the same test gave
//! on the code before the re-anchoring existed (`origin/octaver`, 2a135a0). A change in a single
//! sample of a single frame changes the hash.

use super::*;
use crate::engine::bus::Bus;
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Sim, SimSpec, TrackSpec};
use crate::score::compile_score;

const BLOCK: u32 = 128;

/// A two- and a four-bar loop that stop for four bars and come back on their grid, with an
/// overdub and a release section: everything the re-anchoring rule looks at, nothing it acts on.
/// (With a two-bar pause the four-bar bass would come back off its grid - and be re-anchored,
/// which is exactly the change of behaviour this file is not about.)
const WHOLE_LOOPS: &str = "\
bpm: 92
tracks:
  theme: {input: 2}
  bass: {input: 2, pan: 0.3}
  voice: {input: 1}
sections:
  - id: theme
    bars: 2
    autorelease: true
    tracks:
      theme: record
  - id: bass
    bars: 4
    autorelease: true
    tracks:
      theme: play
      bass: record
  - id: pause
    bars: 4
    autorelease: true
    tracks:
      voice: hear_through
  - id: back
    bars: 4
    autorelease: false
    quantize: loop
    tracks:
      theme: overdub
      bass: play
      voice: record
  - id: end
    bars: 2
    autorelease: true
    tracks:
      voice: play
";

/// Play `yaml` to its end and hash both buses, every frame.
fn sound_of(yaml: &str) -> u64 {
    sound_until(yaml, usize::MAX)
}

/// The same hash over the frames before section `cut` begins (all of them for `usize::MAX`).
fn sound_until(yaml: &str, cut: usize) -> u64 {
    let score = compile_score(yaml).expect("uebersetzt");
    let timeline = Timeline::new(48_000, score.bpm, score.signature());
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
    let lengths: Vec<u64> = score.sections.iter().map(|s| u64::from(s.bars)).collect();
    let waits: Vec<bool> = score.sections.iter().map(|s| !s.autorelease).collect();
    let tracks: Vec<TrackSpec> = score
        .tracks
        .iter()
        .map(|t| {
            let mut spec = TrackSpec::on(0);
            spec.input = t.track_input();
            spec
        })
        .collect();
    let mut sim = Sim::new(
        SimSpec {
            bpm: score.bpm,
            signature: score.signature(),
            bars: max_bars,
            block: BLOCK as usize,
            tracks,
            input_channels: 4,
            loopback_channel: None,
            layer_frames: Some(loop_capacity(&timeline, max_bars)),
            spare_slots: 6,
            ..Default::default()
        },
        Box::new(|k, c| ((k * 7 + c as u64 * 13_331) % 30_011) as f32 / 30_011.0 - 0.5),
    );
    for cmd in setup_commands(&score) {
        sim.send(cmd);
    }
    let mut runner = Runner::new(score, timeline, BLOCK).expect("Runner");
    runner.start(0);
    let timeline = runner.scheduler().timeline;
    let first = runner.armed_at().unwrap().1;
    let mut starts = vec![first];
    for l in &lengths {
        starts.push(starts.last().unwrap() + l);
    }
    let end = timeline.bar_start(*starts.last().unwrap()) + 64 * BLOCK as u64;
    let mut released = vec![false; lengths.len()];
    while sim.pos() < end {
        for cmd in runner.take_commands() {
            sim.send(cmd);
        }
        runner.tick(sim.pos());
        if let Some(i) = runner.view(sim.pos()).section
            && waits[i]
            && !released[i]
            && sim.pos() >= timeline.bar_start(starts[i + 1] - 1)
        {
            released[i] = true;
            runner.next(sim.pos());
        }
        sim.run_blocks(1);
    }
    // FNV-1a over the bits of every sample.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let stop = starts.get(cut).map_or(sim.pos(), |&b| timeline.bar_start(b));
    for pos in 0..stop {
        for bus in [Bus::Main, Bus::Monitor] {
            let f = sim.bus(bus, pos);
            for v in [f.l, f.r] {
                for b in v.to_bits().to_le_bytes() {
                    hash ^= u64::from(b);
                    hash = hash.wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
    }
    hash
}

#[test]
fn scores_with_whole_loops_sound_exactly_as_before() {
    let scores = [
        ("henry-3-4", include_str!("../../../../examples/henry-3-4.rust.yaml")),
        ("waltz", include_str!("../../../../examples/waltz-three-tracks-3-4.yaml")),
        ("solo-loop", include_str!("../../../../examples/solo-loop-4-4.yaml")),
        ("whole loops", WHOLE_LOOPS),
    ];
    let got: Vec<(&str, u64)> = scores.iter().map(|(n, y)| (*n, sound_of(y))).collect();
    eprintln!("{got:x?}");
    assert_eq!(got, GOLDEN.to_vec());
}

/// Taken on `origin/octaver` (2a135a0), before the re-anchoring existed, and since changed on
/// purpose twice:
///
/// * "whole loops" (`stop-all-fix`): its section `back` overdubs `theme`, which was silent in
///   `pause`. The engine records a layer over a loop that does not play, and the runner used to
///   start it only at the end of that take - one pass of silence where the musician should hear
///   the loop he layers onto. Now the loop plays from the section start (`Runner::comes_back`);
///   everything before `back` stayed bit for bit what it was
///   ([`whole_loops_change_only_from_the_overdub_on_a_returning_loop`]).
/// * all four (`audio-clean`): loop playback and monitoring got their declicking fades
///   (`Track::play_frame`). Checked against the previous output sample by sample: every frame of
///   both buses is bit-identical except runs of about 480 frames (one 10 ms fade; a few frames
///   longer where two tracks' seams lie a few frames apart) right before a loop's seam, start,
///   stop or re-anchoring and right after a monitoring switch - about 0.2 % of all frames.
///
/// Before the fades: 0x239a_6ea4_e2fa_f135, 0xf3c7_eb75_2ba3_7c7b, 0x916c_672b_6618_fad1,
/// 0xa280_b7c2_ff5b_f75d. When the two were merged (`integration`), the merged output was compared
/// sample by sample with both parents: against `stop-all-fix` + `load-meter` it differs only in
/// those fade runs (480-484 frames), against `audio-clean` only in "whole loops" from the fade
/// before `back` on - the overdub change above - and nowhere else.
const GOLDEN: [(&str, u64); 4] = [
    ("henry-3-4", 0xcd12_e6da_5f4f_0c7d),
    ("waltz", 0xb80a_7581_3166_0428),
    ("solo-loop", 0xeac7_ad07_0b49_9a75),
    ("whole loops", 0xaf35_6d9f_4435_8ab7),
];

/// "whole loops" up to the start of `back`, hashed on the code before the change above.
/// Taken again with the fades (`audio-clean`); before them 0x818f_c23a_4ee3_4e10. The merge
/// comparison above shows the stretch is bit for bit `audio-clean`'s up to the fade-in in front of
/// `back`, which belongs to the loop that now plays from `back`'s first sample.
const WHOLE_LOOPS_BEFORE_BACK: u64 = 0x0223_0af7_c903_7b2b;

#[test]
fn whole_loops_change_only_from_the_overdub_on_a_returning_loop() {
    let got = sound_until(WHOLE_LOOPS, 3);
    eprintln!("{got:#x}");
    assert_eq!(got, WHOLE_LOOPS_BEFORE_BACK);
}
