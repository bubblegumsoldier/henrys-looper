//! The Monitor switch of a MIDI loop, offline: off cuts the live MIDI before the instrument, the
//! loop that shares the instrument plays on, and no note is left hanging in either direction.

use super::super::command::Command;
use super::super::source::NoteKind;
use super::{BLOCK, C, L, Rig, rig, ons, play, raw, record_first_pass, sent, sounding};

/// Switch the track's monitoring at output position `at` (the next block from there).
fn monitor_at(rig: &mut Rig, at: u64, on: bool) {
    rig.sim.run_to(at);
    rig.sim.send(Command::SetMonitor { track: 0, on });
}

/// Everything the instrument got in `from..to` that concerns key `note` (ons and offs).
fn key_msgs(rig: &Rig, from: u64, to: u64, note: u8) -> Vec<[u8; 3]> {
    sent(rig, from, to)
        .into_iter()
        .map(|(_, m)| m)
        .filter(|m| matches!(m[0] & 0xF0, 0x80 | 0x90) && m[1] == note)
        .collect()
}

/// Sustain pedal messages the instrument got in `from..to`.
fn pedal_msgs(rig: &Rig, from: u64, to: u64) -> Vec<[u8; 3]> {
    sent(rig, from, to)
        .into_iter()
        .map(|(_, m)| m)
        .filter(|m| m[0] & 0xF0 == 0xB0 && m[1] == 64)
        .collect()
}

/// Off: nothing the musician plays reaches the instrument - keys, controllers, bend, aftertouch -
/// and the status says so. On again: it does.
#[test]
fn monitor_off_cuts_every_live_message_before_the_instrument() {
    let mut rig = rig();
    monitor_at(&mut rig, 1_000, false);
    let from = 2_000;
    play(&mut rig, from, [0x90, 60, 100]);
    play(&mut rig, from + 1_000, [0xB0, 1, 90]);
    play(&mut rig, from + 2_000, [0xB0, 64, 127]);
    play(&mut rig, from + 3_000, [0xE0, 0, 90]);
    play(&mut rig, from + 4_000, [0xD0, 70, 0]);
    play(&mut rig, from + 5_000, [0xA0, 60, 70]);
    play(&mut rig, from + 6_000, [0x80, 60, 0]);
    play(&mut rig, from + 7_000, [0xB0, 64, 0]);
    rig.sim.run_to(from + 20_000);
    assert!(sent(&rig, 0, from + 20_000).is_empty(), "{:?}", sent(&rig, 0, from + 20_000));
    let status = rig.sim.latest_status().unwrap();
    assert!(!status.tracks()[0].monitor);

    monitor_at(&mut rig, 30_000, true);
    play(&mut rig, 31_000, [0x90, 62, 100]);
    play(&mut rig, 32_000, [0x80, 62, 0]);
    rig.sim.run_to(40_000);
    assert_eq!(key_msgs(&rig, 30_000, 40_000, 62), vec![[0x90, 62, 100], [0x80, 62, 0]]);
    assert!(sounding(&rig).is_empty());
}

/// The loop shares the instrument with the live keys, and it is what keeps sounding: monitoring
/// off in pass 2 leaves every note of the loop in place, in every pass.
#[test]
fn a_midi_loop_keeps_sounding_while_monitoring_is_off() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    let beat = 28_800;
    play(&mut rig, beat, [0x90, 60, 100]);
    play(&mut rig, beat + 5_000, [0x80, 60, 0]);
    monitor_at(&mut rig, L + 1_000, false);
    // Played over the loop while off: not heard.
    play(&mut rig, L + 3 * beat, [0x90, 72, 100]);
    play(&mut rig, L + 3 * beat + 5_000, [0x80, 72, 0]);
    rig.sim.run_to(3 * L);
    for pass in [1, 2] {
        let one = pass * L;
        assert_eq!(
            key_msgs(&rig, one, one + L, 60),
            vec![[0x90, 60, 100], [0x80, 60, 0]],
            "pass {}",
            pass + 1
        );
        assert_eq!(ons(&rig, one, one + L, 60), vec![one + beat], "on its beat");
    }
    assert!(key_msgs(&rig, L, 3 * L, 72).is_empty());
    assert!(sounding(&rig).is_empty());
}

/// Switching off ends what the musician holds - but a key the loop sounds as well is one voice,
/// and that voice belongs to the loop now: its own off ends it, the musician's later release of
/// it (or of his other key) sends nothing.
#[test]
fn switching_off_ends_the_live_keys_but_not_the_loops_note_on_the_same_key() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    // The loop holds 60 almost the whole pass.
    play(&mut rig, 1_000, [0x90, 60, 100]);
    play(&mut rig, L - 10_000, [0x80, 60, 0]);
    // Pass 2: the musician strikes 60 again on top of it and holds 64 as well.
    play(&mut rig, L + 5_000, [0x90, 60, 90]);
    play(&mut rig, L + 6_000, [0x90, 64, 90]);
    rig.sim.run_to(L + 9_000);
    assert_eq!(sounding(&rig), vec![(0, 60), (0, 64)]);
    let off_at = L + 10_000;
    let after = off_at + 2 * BLOCK as u64;
    monitor_at(&mut rig, off_at, false);
    rig.sim.run_to(after);
    assert_eq!(key_msgs(&rig, off_at, after, 64), vec![[0x80, 64, 0]]);
    assert!(key_msgs(&rig, off_at, after, 60).is_empty(), "60 is the loop's");
    assert_eq!(sounding(&rig), vec![(0, 60)]);
    // He lets go while off: nothing goes out.
    play(&mut rig, L + 20_000, [0x80, 60, 0]);
    play(&mut rig, L + 21_000, [0x80, 64, 0]);
    rig.sim.run_to(L + 30_000);
    assert!(key_msgs(&rig, after, L + 30_000, 60).is_empty());
    assert!(key_msgs(&rig, after, L + 30_000, 64).is_empty());
    assert_eq!(sounding(&rig), vec![(0, 60)], "the loop's note sounds on");
    // The loop's own off ends it.
    rig.sim.run_to(2 * L - 5_000);
    assert_eq!(key_msgs(&rig, L + 30_000, 2 * L - 5_000, 60), vec![[0x80, 60, 0]]);
    assert!(sounding(&rig).is_empty());
    rig.sim.run_to(3 * L);
    assert!(sounding(&rig).is_empty(), "nothing hangs in pass 3 either");
}

/// The musician's pedal goes up when monitoring is switched off - unless the loop holds it down
/// as well (one pedal per channel: lifting it would cut the loop's sustain). A pedal pressed while
/// off never reaches the instrument, and its release after switching on does not either.
#[test]
fn the_live_pedal_is_lifted_on_switching_off_unless_the_loop_holds_it() {
    // Free: no loop.
    let mut rig = rig();
    play(&mut rig, 1_000, [0xB0, 64, 127]);
    play(&mut rig, 2_000, [0x90, 60, 100]);
    monitor_at(&mut rig, 5_000, false);
    rig.sim.run_to(6_000);
    assert_eq!(pedal_msgs(&rig, 5_000, 6_000), vec![[0xB0, 64, 0]]);
    assert_eq!(key_msgs(&rig, 5_000, 6_000, 60), vec![[0x80, 60, 0]]);
    // Released while off, pressed again while off, switched on, released: nothing goes out.
    play(&mut rig, 7_000, [0xB0, 64, 0]);
    play(&mut rig, 8_000, [0xB0, 64, 127]);
    monitor_at(&mut rig, 10_000, true);
    play(&mut rig, 12_000, [0xB0, 64, 0]);
    rig.sim.run_to(14_000);
    assert!(pedal_msgs(&rig, 6_000, 14_000).is_empty(), "{:?}", pedal_msgs(&rig, 6_000, 14_000));
    // Pressed anew while on: heard.
    play(&mut rig, 15_000, [0xB0, 64, 127]);
    play(&mut rig, 16_000, [0xB0, 64, 0]);
    rig.sim.run_to(18_000);
    assert_eq!(pedal_msgs(&rig, 14_000, 18_000), vec![[0xB0, 64, 127], [0xB0, 64, 0]]);

    // The loop holds the pedal through pass 2; the musician presses it too and switches off.
    let mut rig = super::rig();
    record_first_pass(&mut rig);
    play(&mut rig, 1_000, [0xB0, 64, 127]);
    play(&mut rig, 2_000, [0x90, 60, 100]);
    play(&mut rig, 3_000, [0x80, 60, 0]);
    play(&mut rig, L - 10_000, [0xB0, 64, 0]);
    play(&mut rig, L + 5_000, [0xB0, 64, 127]);
    let off_at = L + 10_000;
    monitor_at(&mut rig, off_at, false);
    play(&mut rig, L + 20_000, [0xB0, 64, 0]);
    rig.sim.run_to(2 * L - 20_000);
    assert!(pedal_msgs(&rig, off_at, 2 * L - 20_000).is_empty(), "the loop's pedal stays down");
    rig.sim.run_to(2 * L - 5_000);
    assert_eq!(pedal_msgs(&rig, 2 * L - 20_000, 2 * L - 5_000), vec![[0xB0, 64, 0]]);
}

/// A key pressed while off and still held when monitoring comes back stays silent; its release
/// sends nothing (no off for a note the instrument never had), and struck again it is heard.
#[test]
fn a_key_held_through_switching_on_stays_silent_until_struck_again() {
    let mut rig = rig();
    monitor_at(&mut rig, 1_000, false);
    play(&mut rig, 2_000, [0x90, 62, 100]);
    monitor_at(&mut rig, 5_000, true);
    rig.sim.run_to(8_000);
    assert!(key_msgs(&rig, 0, 8_000, 62).is_empty());
    play(&mut rig, 9_000, [0x80, 62, 0]);
    rig.sim.run_to(12_000);
    assert!(key_msgs(&rig, 0, 12_000, 62).is_empty(), "no stray off");
    play(&mut rig, 13_000, [0x90, 62, 100]);
    play(&mut rig, 14_000, [0x80, 62, 0]);
    rig.sim.run_to(16_000);
    assert_eq!(key_msgs(&rig, 12_000, 16_000, 62), vec![[0x90, 62, 100], [0x80, 62, 0]]);

    // Held from before switching off, through it and through switching on: the cut ended it,
    // and its release afterwards sends nothing.
    play(&mut rig, 17_000, [0x90, 64, 100]);
    monitor_at(&mut rig, 18_000, false);
    monitor_at(&mut rig, 20_000, true);
    play(&mut rig, 22_000, [0x80, 64, 0]);
    rig.sim.run_to(24_000);
    assert_eq!(key_msgs(&rig, 16_000, 24_000, 64), vec![[0x90, 64, 100], [0x80, 64, 0]]);
    assert!(sounding(&rig).is_empty());
}

/// Recording goes on while monitoring is off, as on an audio track: the take has the notes where
/// they were played, the instrument does not get them while they are played, and the loop plays
/// them from its first repeat on. A pedal held down when the take begins counts too.
#[test]
fn a_midi_take_records_while_monitoring_is_off_and_is_heard_from_the_first_repeat() {
    let mut rig = rig();
    rig.sim.send(Command::SetMonitor { track: 0, on: false });
    record_first_pass(&mut rig);
    let beat = 28_800;
    play(&mut rig, beat, [0x90, 60, 100]);
    play(&mut rig, beat + 5_000, [0x80, 60, 0]);
    rig.sim.run_to(L);
    assert!(sent(&rig, 0, L).is_empty(), "not heard while it is played");
    let recorded = raw(&rig, 0);
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0].frame, beat as i64);
    assert_eq!(recorded[1].frame, (beat + 5_000) as i64);
    rig.sim.run_to(2 * L);
    assert_eq!(ons(&rig, L, 2 * L, 60), vec![L + beat], "the first repeat plays it");
    assert!(sounding(&rig).is_empty());

    // A pedal pressed while off and held into a take is recorded as pressed at its start.
    let mut rig = super::rig();
    rig.sim.send(Command::SetMonitor { track: 0, on: false });
    play(&mut rig, 1_000, [0xB0, 64, 127]);
    let start = 10_000;
    rig.sim.send(Command::StartRecord { track: 0, at: start });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: start + L,
    });
    play(&mut rig, start + 5_000, [0x90, 60, 100]);
    play(&mut rig, start + 6_000, [0x80, 60, 0]);
    play(&mut rig, start + 8_000, [0xB0, 64, 0]);
    rig.sim.run_to(start + L + C + 2 * BLOCK as u64);
    assert!(sent(&rig, 0, start + L).is_empty());
    let recorded = raw(&rig, 0);
    let pedal: Vec<_> = recorded.iter().filter(|r| r.ev.kind == NoteKind::Control).collect();
    assert_eq!(pedal.len(), 2, "{recorded:?}");
    assert_eq!(pedal[0].frame, 0, "pressed at the start of the take");
}

/// And it is heard: the instrument's output of a MIDI loop is not faded by the switch - only the
/// live keys are cut.
#[test]
fn the_loop_is_heard_in_the_speakers_while_monitoring_is_off() {
    use super::super::source::testing::Impulse;
    use super::super::source::{Instrument, InstrumentMsg, SourceShared};
    let mut rig = rig();
    rig._instruments
        .send(InstrumentMsg::Install {
            track: 0,
            instrument: Instrument::new(Box::new(Impulse::new(0)), SourceShared::new(0)),
        })
        .unwrap();
    record_first_pass(&mut rig);
    let beat = 28_800;
    play(&mut rig, beat, [0x90, 60, 127]);
    play(&mut rig, beat + 5_000, [0x80, 60, 0]);
    monitor_at(&mut rig, L + 1_000, false);
    play(&mut rig, L + 2 * beat, [0x90, 72, 127]);
    rig.sim.run_to(2 * L);
    let heard: Vec<usize> = (L as usize..2 * L as usize)
        .filter(|&i| rig.sim.out_l[i] != 0.0)
        .collect();
    assert_eq!(heard, vec![(L + beat) as usize], "the loop's note, not the live one");
}
