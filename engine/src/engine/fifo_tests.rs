//! The input FIFO (`crate::fifo`) in front of the real engine core: input and output callbacks
//! that do not alternate neatly - an input callback after its output callback, an output callback
//! that stalls for several blocks, input that arrives in a burst - and a take recorded through all
//! of it. The take must still be the input shifted by exactly the compensation, sample for sample;
//! no input frame may be lost; the backlog must shrink back to the cushion; and none of it may
//! allocate.

use super::command::Command;
use super::sim::{Sim, SimSpec, TrackSpec};
use super::bus::BUS_SAMPLES;
use crate::fifo::{CUSHION_BLOCKS, input_fifo};
use crate::rt;

const BLOCK: usize = 128;
const R: u64 = 596;
const SCALE: f32 = 16_777_216.0;

fn counting(k: u64) -> f32 {
    (k + 1) as f32 / SCALE
}

fn decode(v: f32) -> Option<u64> {
    let n = (v.abs() * SCALE).round() as u64;
    (n > 0).then(|| n - 1)
}

/// One driver cycle: does the input callback run before the output callback, after it, or not at
/// all (its frames come with a later one).
///
/// An output callback the driver skips while the input runs on is not modelled: on one device the
/// HAL's overload takes the cycle from both streams, and an output-only loss would misplace every
/// later take physically (the loop is heard later than its position says) whatever the FIFO does.
#[derive(Clone, Copy, Debug)]
enum Cycle {
    InputFirst,
    OutputFirst,
    /// The input callback came late and delivers this cycle's frames with a later one.
    InputHeld,
}

#[test]
fn a_take_through_a_jittery_fifo_is_the_input_shifted_by_the_latency() {
    let mut sim = Sim::new(
        SimSpec {
            bpm: 120.0,
            bars: 2,
            latency: R,
            roundtrip: R,
            block: BLOCK,
            tracks: vec![TrackSpec::on(0).monitoring()],
            input_channels: 1,
            loopback_channel: None,
            ..Default::default()
        },
        Box::new(|_, _| 0.0),
    );
    // Two bars at 120 BPM: 4 s, 192 000 frames. The take covers the first bar after a second.
    let start = 48_000u64;
    let len = 96_000u64;
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord {
        track: 0,
        at: start + len,
    });

    let (mut writer, mut reader) = input_fifo(BLOCK, 1);
    let mut in_scratch = vec![0.0f32; BLOCK * 16];
    let mut out = vec![0.0f32; BLOCK * BUS_SAMPLES];
    let mut next_in = 0u64;
    let mut held = 0usize;
    let mut lost = 0usize;
    let mut late = 0usize;
    let mut max_waiting = 0usize;
    let mut cycle_no = 0u64;
    let pattern = |c: u64| -> Cycle {
        match c % 97 {
            10 | 11 => Cycle::OutputFirst,
            // Three input callbacks late: a backlog of three blocks, then four at once.
            30..=32 => Cycle::InputHeld,
            50 => Cycle::InputHeld,
            70 => Cycle::OutputFirst,
            _ => Cycle::InputFirst,
        }
    };
    let rt_before = rt::counts();
    let mut input_cb = |writer: &mut crate::fifo::FifoWriter, held: &mut usize, lost: &mut usize| {
        let frames = BLOCK * (1 + *held);
        *held = 0;
        let data: Vec<f32> = (0..frames as u64).map(|i| counting(next_in + i)).collect();
        next_in += frames as u64;
        let _armed = rt::arm();
        *lost += writer.push(&data);
    };
    let mut pos = 0u64;
    while pos < start + len + 4 * R + 8 * BLOCK as u64 {
        let cycle = pattern(cycle_no);
        cycle_no += 1;
        match cycle {
            Cycle::InputHeld => held += 1,
            Cycle::InputFirst => input_cb(&mut writer, &mut held, &mut lost),
            Cycle::OutputFirst => {}
        }
        {
            let _armed = rt::arm();
            let pulled = reader.pull(&mut in_scratch, BLOCK);
            if pulled.short {
                late += 1;
            }
            max_waiting = max_waiting.max(reader.waiting());
            sim.core
                .process(&in_scratch[..pulled.frames], &mut out[..BLOCK * BUS_SAMPLES]);
            pos += BLOCK as u64;
        }
        if matches!(cycle, Cycle::OutputFirst) {
            input_cb(&mut writer, &mut held, &mut lost);
        }
    }
    let spent = rt::counts().since(rt_before);
    // The test's own `Vec` of input data is built outside the armed parts; `push`, `pull` and
    // `process` are inside.
    assert!(spent.is_zero(), "{spent:?}");
    assert_eq!(lost, 0, "no input frame lost");
    assert!(late > 0, "the pattern really made the input late");
    assert!(
        max_waiting <= (1 + CUSHION_BLOCKS) * BLOCK,
        "the backlog shrinks to the cushion: {max_waiting} frames waiting at most"
    );

    let track = sim.core.track(0);
    assert_eq!(track.loop_len(), len);
    let content = track.layer(0).expect("the take").channel(0, len);
    for (i, &v) in content.iter().enumerate() {
        // Musical position m = start + i was played at input frame m + R.
        assert_eq!(
            decode(v),
            Some(start + i as u64 + R),
            "layer index {i}: the take is the input shifted by R, nothing lost or doubled"
        );
    }
}
