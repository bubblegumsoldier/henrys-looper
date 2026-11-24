//! MIDI loops, offline: where a played note is stored, when it plays back, and that no note is
//! left hanging whatever happens to the loop.

use std::sync::{Arc, Mutex};

use rtrb::Producer;

use super::command::Command;
use super::midiloop::{NoteGrid, PlayEvent, RawEvent};
use super::sim::{Sim, SimSpec, TrackSpec, silence};
use super::source::testing::Recorder;
use super::source::{
    FeedMsg, FeedSender, Instrument, InstrumentMsg, InstrumentSender, LiveEvent, NoteKind,
    SourceShared, TrackRoute, port_feed, source_channel,
};
use super::track::TrackState;

/// The Monitor switch of a MIDI loop: it cuts the live keys before the instrument.
mod monitor_tests;

const BLOCK: usize = 128;
/// Output latency of the simulated device, in frames.
const OUT: u64 = 300;
/// `C`: output latency plus one block.
const C: u64 = OUT + BLOCK as u64;
/// Two bars at 100 BPM, 4/4, 48 kHz.
const L: u64 = 2 * 4 * 28_800;

type Seen = Arc<Mutex<Vec<(u64, [u8; 3])>>>;

struct Rig {
    sim: Sim,
    _instruments: InstrumentSender,
    feeds: FeedSender,
    keys: Producer<LiveEvent>,
    seen: Seen,
}

/// One MIDI-loop instrument track (plus `extra` audio tracks), a recorder for an instrument that
/// reports `latency` frames, a clock, and the clip stage.
fn rig_with(latency: u32, extra: usize) -> Rig {
    rig_at(100.0, latency, extra)
}

fn rig() -> Rig {
    rig_with(0, 0)
}

/// Play `msg` at the moment the musician hears output position `heard`.
fn play(rig: &mut Rig, heard: u64, msg: [u8; 3]) {
    let t = rig.sim.heard_ns(heard);
    rig.sim.run_to(heard + OUT);
    rig.keys.push(LiveEvent { host_ns: t, msg }).unwrap();
}

/// Record the first pass `0..L` and play it from `L` on.
fn record_first_pass(rig: &mut Rig) {
    rig.sim.send(Command::StartRecord { track: 0, at: 0 });
    rig.sim.send(Command::StopRecord { track: 0, at: L });
    rig.sim.send(Command::StartPlay { track: 0, at: L });
}

fn raw(rig: &Rig, layer: usize) -> Vec<RawEvent> {
    rig.sim
        .core
        .track(0)
        .layer(layer)
        .and_then(|l| l.clip())
        .map(|c| c.raw().to_vec())
        .unwrap_or_default()
}

fn play_list(rig: &Rig, layer: usize) -> Vec<PlayEvent> {
    rig.sim
        .core
        .track(0)
        .layer(layer)
        .and_then(|l| l.clip())
        .map(|c| c.play().to_vec())
        .unwrap_or_default()
}

/// What the instrument got in `from..to`, absolute output positions.
fn sent(rig: &Rig, from: u64, to: u64) -> Vec<(u64, [u8; 3])> {
    rig.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| (from..to).contains(at))
        .copied()
        .collect()
}

/// Which notes the instrument is left sounding after everything it got: note ons minus offs,
/// "all notes off" clears a channel.
fn sounding(rig: &Rig) -> Vec<(u8, u8)> {
    let mut on: Vec<(u8, u8)> = Vec::new();
    for (_, m) in rig.seen.lock().unwrap().iter() {
        let c = m[0] & 0x0F;
        match m[0] & 0xF0 {
            0x90 if m[2] > 0 => {
                if !on.contains(&(c, m[1])) {
                    on.push((c, m[1]));
                }
            }
            0x80 | 0x90 => on.retain(|&n| n != (c, m[1])),
            0xB0 if m[1] == 123 => on.retain(|&(ch, _)| ch != c),
            _ => {}
        }
    }
    on.sort();
    on
}

/// The core promise: a key pressed while beat `B` is heard is stored at `B` - frame and tick - and
/// the loop plays it back on `B` in every pass, moved forward by the instrument's latency so it is
/// *heard* there.
#[test]
fn a_note_played_on_the_heard_beat_is_stored_and_played_on_that_beat() {
    for latency in [0u32, 100] {
        let mut rig = rig_with(latency, 0);
        record_first_pass(&mut rig);
        let beat = 3 * 28_800;
        play(&mut rig, beat, [0x90, 60, 100]);
        play(&mut rig, beat + 1_000, [0x80, 60, 0]);
        rig.sim.run_to(3 * L);

        let track = rig.sim.core.track(0);
        assert_eq!(track.loop_len(), L);
        assert_eq!(track.layer_count(), 1);
        let raw = raw(&rig, 0);
        assert_eq!(raw.len(), 2, "{raw:?}");
        assert_eq!(raw[0].frame, beat as i64);
        assert_eq!(raw[0].ev.tick, 3 * i64::from(super::source::PPQ), "Schlag 4 in Ticks");
        assert_eq!(raw[0].ev.kind, NoteKind::On);
        assert_eq!(raw[0].ev.d2, 100, "Velocity bleibt");
        assert_eq!(raw[1].frame, (beat + 1_000) as i64);

        // Live, the note went out as early as possible: in the block it arrived in.
        let live = sent(&rig, 0, L);
        assert_eq!(live.len(), 2);
        assert!(live[0].0 >= beat + OUT && live[0].0 < beat + OUT + 2 * BLOCK as u64);
        // Played back: on the beat in pass 2 and 3, minus the instrument's latency.
        let lat = u64::from(latency);
        for pass in [1, 2] {
            let got = sent(&rig, pass * L - lat, (pass + 1) * L - lat);
            assert_eq!(
                got,
                vec![
                    (pass * L + beat - lat, [0x90, 60, 100]),
                    (pass * L + beat + 1_000 - lat, [0x80, 60, 0])
                ],
                "Durchlauf {pass}, Latenz {latency}"
            );
        }
        assert!(sounding(&rig).is_empty());
    }
}

/// Continuous overdub on a MIDI loop: every pass with something in it is a layer of its own, an
/// empty one is dropped, and all of them play together.
#[test]
fn three_latched_passes_are_three_midi_layers() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 1_000, [0x90, 40, 90]);
    play(&mut rig, 2_000, [0x80, 40, 0]);
    rig.sim.run_to(L / 2);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: true,
        at: 0,
    });
    for (pass, note) in [(1u64, 50u8), (2, 52), (3, 55)] {
        play(&mut rig, pass * L + 10_000, [0x90, note, 90]);
        play(&mut rig, pass * L + 12_000, [0x80, note, 0]);
    }
    rig.sim.run_to(4 * L - 10_000);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: false,
        at: 0,
    });
    rig.sim.run_to(6 * L);
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 4, "Aufnahme plus drei Overdub-Durchlaeufe");
    for (layer, note) in [(1usize, 50u8), (2, 52), (3, 55)] {
        let r = raw(&rig, layer);
        assert_eq!(r.len(), 2, "Ebene {layer}");
        assert_eq!(r[0].ev.d1, note);
        assert_eq!(r[0].frame.rem_euclid(L as i64), 10_000);
    }
    // Pass 5 plays all four layers.
    let got = sent(&rig, 5 * L, 6 * L);
    let ons: Vec<u8> = got
        .iter()
        .filter(|(_, m)| m[0] == 0x90)
        .map(|(_, m)| m[1])
        .collect();
    assert_eq!(ons, vec![40, 50, 52, 55]);
    let status = rig.sim.latest_status().unwrap().tracks()[0];
    assert!(status.midi_loop);
    assert_eq!(&status.layer_notes[..4], &[1, 1, 1, 1]);
    assert!(sounding(&rig).is_empty());
}

/// A hit a little before the one is not lost: it plays at the end of the loop, exactly as early as
/// it was played - and a grid puts it on the one.
#[test]
fn the_pre_roll_keeps_a_hit_just_before_the_start() {
    let mut rig = rig();
    rig.sim.run_to(20_000);
    let start = 40_000u64;
    // 300 frames before the start - well inside a 32nd (3600 frames at 100 BPM).
    play(&mut rig, start - 300, [0x99, 36, 120]);
    play(&mut rig, start - 100, [0x89, 36, 0]);
    // Before the pre-roll: not taken.
    rig.sim.send(Command::StartRecord { track: 0, at: start });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: start + L,
    });
    rig.sim.send(Command::StartPlay {
        track: 0,
        at: start + L,
    });
    rig.sim.run_to(start + 2 * L + 1_000);
    let r = raw(&rig, 0);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0].frame, -300);
    assert!(play_list(&rig, 0).contains(&PlayEvent {
        offset: (L - 300) as u32,
        msg: [0x99, 36, 120],
        frame: -300,
    }));
    // Played back 300 frames before the third pass begins (the first pass was the recording).
    let got = sent(&rig, start + 2 * L - 1_000, start + 2 * L);
    assert_eq!(got, vec![
        (start + 2 * L - 300, [0x99, 36, 120]),
        (start + 2 * L - 100, [0x89, 36, 0])
    ]);

    // With a 1/16 grid the hit sits on the one.
    rig.sim.clips.as_mut().unwrap().set_grid(0, NoteGrid::Sixteenth);
    rig.sim.run_blocks(4);
    let list = play_list(&rig, 0);
    assert_eq!(list[0], PlayEvent {
        offset: 0,
        msg: [0x99, 36, 120],
        frame: 0,
    });
    assert_eq!(list[1].offset, 200, "die Laenge bleibt");
    assert_eq!(raw(&rig, 0)[0].frame, -300, "Rohdaten unveraendert");
}

/// Something played earlier than the pre-roll is not part of the take.
#[test]
fn what_was_played_before_the_pre_roll_is_not_taken() {
    let mut rig = rig();
    rig.sim.run_to(20_000);
    let start = 40_000u64;
    play(&mut rig, start - 10_000, [0x90, 36, 120]);
    play(&mut rig, start - 9_000, [0x80, 36, 0]);
    rig.sim.send(Command::StartRecord { track: 0, at: start });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: start + L,
    });
    rig.sim.run_to(start + L + 2 * C);
    assert!(raw(&rig, 0).is_empty());
}

/// A chord held across the end of the take: the offs are stored where they were played and wrap
/// to the start of the loop, before anything starts there.
#[test]
fn an_off_after_the_end_wraps_to_the_start() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, L - 5_000, [0x90, 60, 100]);
    play(&mut rig, L + 2_000, [0x80, 60, 0]);
    rig.sim.run_to(3 * L);
    let r = raw(&rig, 0);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[1].frame, (L + 2_000) as i64, "wo es gespielt wurde");
    assert_eq!(play_list(&rig, 0)[0], PlayEvent {
        offset: 2_000,
        msg: [0x80, 60, 0],
        frame: (L + 2_000) as i64,
    });
    // Pass 3: off at 2000 (from pass 2's note), on at L - 5000.
    let got = sent(&rig, 2 * L, 3 * L);
    assert_eq!(got, vec![
        (2 * L + 2_000, [0x80, 60, 0]),
        (3 * L - 5_000, [0x90, 60, 100])
    ]);
}

/// A note that never gets its off ends one frame before the end of its pass - no hanging note in
/// the loop.
#[test]
fn a_missing_off_is_put_before_the_loop_end() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 30_000, [0x90, 62, 100]);
    // The key stays down for more than a whole loop.
    rig.sim.run_to(2 * L + 3 * C);
    let r = raw(&rig, 0);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[1].frame, L as i64 - 1);
    assert_eq!(r[1].ev.kind, NoteKind::Off);
    // The late off of the real key is not taken any more.
    play(&mut rig, 2 * L + 4 * C, [0x80, 62, 0]);
    rig.sim.run_to(4 * L);
    assert_eq!(raw(&rig, 0).len(), 2);
    let got = sent(&rig, 3 * L, 4 * L);
    assert_eq!(got, vec![(3 * L + 30_000, [0x90, 62, 100]), (4 * L - 1, [0x80, 62, 0])]);
}

/// Offs of keys the take never saw pressed are not recorded.
#[test]
fn an_orphan_off_is_not_recorded() {
    let mut rig = rig();
    rig.sim.run_to(10_000);
    play(&mut rig, 10_000, [0x90, 64, 100]);
    rig.sim.send(Command::StartRecord {
        track: 0,
        at: 30_000,
    });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: 30_000 + L,
    });
    play(&mut rig, 40_000, [0x80, 64, 0]);
    rig.sim.run_to(30_000 + L + 2 * C);
    assert!(raw(&rig, 0).is_empty());
}

/// The note a loop plays while the musician holds a key of his own: every way of stopping the loop
/// ends the loop's note, and only that one.
#[test]
fn stopping_the_loop_in_any_way_leaves_no_note_hanging_and_keeps_the_live_key() {
    type Stop = fn(&mut Rig);
    let stops: [(&str, Stop); 8] = [
        ("stop", |r| {
            let at = r.sim.pos() + 64;
            r.sim.send(Command::StopPlay { track: 0, at });
        }),
        ("mute", |r| {
            r.sim.send(Command::SetLayerMute {
                track: 0,
                layer: 0,
                muted: true,
            })
        }),
        ("undo", |r| r.sim.send(Command::UndoLayer { track: 0 })),
        ("clear", |r| {
            let at = r.sim.pos();
            r.sim.send(Command::ClearTrack { track: 0, at });
        }),
        ("remove", |r| {
            r.sim.send(Command::RemoveLayer { track: 0, layer: 0 })
        }),
        ("transport", |r| r.sim.send(Command::TransportStop)),
        ("clear all", |r| {
            let at = r.sim.pos();
            r.sim.send(Command::ClearAll { at });
        }),
        ("engine stop", |r| r.sim.send(Command::Stop)),
    ];
    for (name, stop) in stops {
        let mut rig = rig();
        record_first_pass(&mut rig);
        play(&mut rig, 10_000, [0x90, 60, 100]);
        play(&mut rig, 80_000, [0x80, 60, 0]);
        // In pass 2, the loop's note is sounding; the musician holds another key.
        play(&mut rig, L + 20_000, [0x90, 72, 90]);
        rig.sim.run_to(L + 40_000);
        assert_eq!(sounding(&rig), vec![(0, 60), (0, 72)], "{name}: vorher");
        stop(&mut rig);
        rig.sim.run_to(2 * L + 50_000);
        assert_eq!(sounding(&rig), vec![(0, 72)], "{name}: Loop-Note aus, Live-Taste bleibt");
    }
}

/// Unplugging the keyboard ends the live notes; nothing is left on.
#[test]
fn a_lost_port_leaves_nothing_hanging() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    play(&mut rig, 80_000, [0x80, 60, 0]);
    play(&mut rig, L + 20_000, [0x90, 72, 90]);
    rig.sim.run_to(L + 40_000);
    rig.feeds.send(FeedMsg::Detach { port: 0 }).unwrap();
    rig.sim.run_to(3 * L);
    assert!(sounding(&rig).is_empty());
}

/// A section change in a score is a stop of this track and a record on the next: the loop's note
/// ends, the new take starts clean.
#[test]
fn a_new_recording_ends_the_old_loops_notes() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    play(&mut rig, 80_000, [0x80, 60, 0]);
    rig.sim.run_to(L + 40_000);
    rig.sim.send(Command::StartRecord {
        track: 0,
        at: L + 50_000,
    });
    rig.sim.run_to(2 * L);
    assert!(sounding(&rig).is_empty());
    assert_eq!(rig.sim.core.track(0).state(2 * L), TrackState::Recording);
}

/// Unmuting brings the layer back from its next event on; the gain of a MIDI layer scales the
/// velocity.
#[test]
fn a_layer_gain_scales_the_velocity_and_mute_comes_back() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    play(&mut rig, 12_000, [0x80, 60, 0]);
    rig.sim.run_to(L + 20_000);
    rig.sim.send(Command::SetLayerGain {
        track: 0,
        layer: 0,
        gain: 0.5,
    });
    rig.sim.run_to(3 * L);
    assert_eq!(sent(&rig, 2 * L, 3 * L)[0], (2 * L + 10_000, [0x90, 60, 50]));
}

/// The grid changes what is played, never what was recorded, and can be taken back: off again,
/// the notes are where they were played.
#[test]
fn a_grid_is_applied_to_existing_layers_and_can_be_taken_back() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    // 1/16 at 100 BPM is 7200 frames. 500 late, 2000 long.
    play(&mut rig, 7_200 + 500, [0x90, 60, 100]);
    play(&mut rig, 7_200 + 2_500, [0x80, 60, 0]);
    rig.sim.run_to(L + 3 * C);
    rig.sim.clips.as_mut().unwrap().set_grid(0, NoteGrid::Sixteenth);
    rig.sim.run_to(2 * L);
    assert_eq!(sent(&rig, 2 * L, 3 * L).len(), 0, "noch nicht gelaufen");
    rig.sim.run_to(3 * L);
    assert_eq!(sent(&rig, 2 * L, 3 * L), vec![
        (2 * L + 7_200, [0x90, 60, 100]),
        (2 * L + 9_200, [0x80, 60, 0])
    ]);
    rig.sim.clips.as_mut().unwrap().set_grid(0, NoteGrid::None);
    rig.sim.run_to(4 * L);
    assert_eq!(sent(&rig, 3 * L, 4 * L), vec![
        (3 * L + 7_700, [0x90, 60, 100]),
        (3 * L + 9_700, [0x80, 60, 0])
    ]);
    // The mirror on the control side has exactly the raw data.
    let id = rig.sim.core.track(0).layer(0).unwrap().clip().unwrap().id();
    assert_eq!(
        rig.sim.clips.as_ref().unwrap().mirror(id).unwrap(),
        raw(&rig, 0).as_slice()
    );
}

/// The loop type can only change while the track is empty, and only on an instrument track.
#[test]
fn the_loop_type_changes_only_on_an_empty_instrument_track() {
    let mut rig = rig_with(0, 1);
    rig.sim.send(Command::SetLoopKind {
        track: 1,
        midi: true,
    });
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    rig.sim.run_to(L / 2);
    rig.sim.send(Command::SetLoopKind {
        track: 0,
        midi: false,
    });
    rig.sim.run_to(L / 2 + 1_000);
    assert!(rig.sim.core.track(0).midi_loop());
    assert!(!rig.sim.core.track(1).midi_loop(), "Audiospur bleibt Audio");
    let status = rig.sim.latest_status().unwrap();
    assert!(status.ignored_commands >= 2);
}

/// The clips go back to the control thread when layers are dropped; nothing leaks.
#[test]
fn clips_come_back_to_the_pool() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    play(&mut rig, 12_000, [0x80, 60, 0]);
    rig.sim.run_to(L + 3 * C);
    rig.sim.send(Command::StartOverdub { track: 0, at: L * 2 });
    rig.sim.run_to(3 * L + 3 * C);
    assert_eq!(rig.sim.core.track(0).layer_count(), 2);
    let at = rig.sim.pos();
    rig.sim.send(Command::ClearTrack { track: 0, at });
    rig.sim.run_blocks(4);
    assert_eq!(rig.sim.clips.as_ref().unwrap().reclaimed, 2);
    assert_eq!(rig.sim.core.clip_stock(), super::midiloop::CLIP_STOCK);
}

/// An audio track next to a MIDI loop records exactly what it did before.
#[test]
fn an_audio_track_beside_a_midi_loop_is_unchanged() {
    let mut rig = rig_with(0, 1);
    record_first_pass(&mut rig);
    rig.sim.send(Command::StartRecord { track: 1, at: 0 });
    rig.sim.send(Command::StopRecord { track: 1, at: L });
    rig.sim.run_to(L + 2_000);
    let audio = rig.sim.core.track(1);
    assert!(!audio.midi_loop());
    assert_eq!(audio.loop_len(), L);
    assert!(audio.layer(0).unwrap().clip().is_none());
}

/// Record and overdub back to back, a chord held across the boundary: its off belongs to the
/// recording (wrapped to its start), not to the overdub, which never saw it pressed.
#[test]
fn a_held_chord_across_record_and_overdub_ends_in_the_recording() {
    let mut rig = rig();
    rig.sim.send(Command::StartRecord { track: 0, at: 0 });
    rig.sim.send(Command::StopRecord { track: 0, at: L });
    rig.sim.send(Command::StartPlay { track: 0, at: L });
    rig.sim.run_to(L / 2);
    rig.sim.send(Command::StartOverdub { track: 0, at: L });
    rig.sim.send(Command::StopRecord { track: 0, at: 2 * L });
    play(&mut rig, L - 4_000, [0x90, 60, 100]);
    play(&mut rig, L + 3_000, [0x80, 60, 0]);
    play(&mut rig, L + 10_000, [0x90, 64, 100]);
    play(&mut rig, L + 11_000, [0x80, 64, 0]);
    rig.sim.run_to(3 * L + 20_000);
    let first = raw(&rig, 0);
    assert_eq!(first.len(), 2, "{first:?}");
    assert_eq!(first[1].frame, (L + 3_000) as i64);
    let second = raw(&rig, 1);
    assert_eq!(second.len(), 2, "{second:?}");
    assert!(second.iter().all(|e| e.ev.d1 == 64));
    assert!(sounding(&rig).is_empty());
}

/// A pedal held down when the take begins is part of what was played: recorded at the start.
#[test]
fn a_pedal_held_at_the_start_is_recorded_pressed() {
    let mut rig = rig();
    rig.sim.run_to(5_000);
    play(&mut rig, 5_000, [0xB0, 64, 127]);
    rig.sim.run_to(20_000);
    rig.sim.send(Command::StartRecord {
        track: 0,
        at: 30_000,
    });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: 30_000 + L,
    });
    play(&mut rig, 40_000, [0x90, 60, 100]);
    play(&mut rig, 50_000, [0xB0, 64, 0]);
    play(&mut rig, 51_000, [0x80, 60, 0]);
    rig.sim.run_to(30_000 + L + 2 * C);
    let r = raw(&rig, 0);
    let pedal: Vec<(i64, u8)> = r
        .iter()
        .filter(|e| e.ev.kind == NoteKind::Control)
        .map(|e| (e.frame, e.ev.d2))
        .collect();
    assert_eq!(pedal, vec![(0, 127), (20_000, 0)]);
}

/// Stopping the transport while a key of the loop's first take is still down: the take is kept
/// only if its end had passed; an overdub that is cut short gets its open notes ended before the
/// end of the pass.
#[test]
fn a_transport_stop_in_an_overdub_ends_its_open_notes_in_the_layer() {
    let mut rig = rig();
    record_first_pass(&mut rig);
    rig.sim.run_to(L / 2);
    rig.sim.send(Command::StartOverdub { track: 0, at: L });
    play(&mut rig, L + 20_000, [0x90, 67, 100]);
    rig.sim.run_to(L + 30_000);
    rig.sim.send(Command::TransportStop);
    rig.sim.run_to(4 * L);
    let r = raw(&rig, 1);
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[1].ev.kind, NoteKind::Off);
    assert_eq!(r[1].frame, L as i64 - 1);
    assert_eq!(sounding(&rig), vec![(0, 67)], "die gehaltene Live-Taste klingt weiter");
}

/// A recording of a MIDI loop: the stem as audio, and next to it `01 keys.mid` with everything the
/// track sent its instrument - the live notes of the take and the loop's playback after it.
#[test]
fn a_recording_writes_the_tracks_midi_file() {
    use crate::capture::capture_channel;
    use crate::capture::run::{Mode, Recording, RunInfo, RunTrack};
    use crate::engine::frame::Channels;
    let dir = std::env::temp_dir().join(format!("henrys-looper-midi-capture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut rig = rig();
    let (mut port, endpoint) = capture_channel();
    rig.sim.core.attach_capture(endpoint);
    let mut rec = Recording::start(
        RunInfo {
            title: "MIDI".into(),
            mode: Mode::Free,
            folder: dir.join("run"),
            started_at: "2026-11-15 20:00:00".into(),
            sample_rate: 48_000,
            bpm: 100.0,
            beats_per_bar: 4,
            beat_unit: 4,
            count_in_bars: 0,
            start: 0,
            start_bar: 0,
            tracks: vec![RunTrack {
                name: "keys".into(),
                channels: Channels::Stereo,
                latency_frames: 0,
            }],
            dry: false,
            max_block: BLOCK,
            score_yaml: None,
            score_path: None,
            min_seconds: 1.0,
            queue_seconds: 60.0,
        },
        &mut port,
    )
    .expect("Aufnahme startet");
    rec.note_take();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x90, 60, 100]);
    play(&mut rig, 20_000, [0x80, 60, 0]);
    rig.sim.run_to(2 * L + 30_000);
    rec.stop(2 * L + 30_000);
    let end = rig.sim.pos() + 10 * 48_000;
    while rig.sim.pos() < end && rec.poll().is_none() {
        rig.sim.step();
    }
    let outcome = rec.wait().cloned().expect("fertig");
    assert!(outcome.kept, "{outcome:?}");
    let data = std::fs::read(outcome.folder.join("01 keys.mid")).expect("MIDI-Datei");
    assert_eq!(&data[..4], b"MThd");
    // Live once, then the loop in pass 2 and at the start of pass 3: three note ons.
    let ons = data.windows(3).filter(|w| w == &[0x90, 60, 100]).count();
    assert_eq!(ons, 3);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(outcome.folder.join("session.json")).unwrap())
            .unwrap();
    assert_eq!(json["tracks"][0]["midi"], "01 keys.mid");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A score drives a MIDI loop through its states exactly as an audio loop: record, overdub, play,
/// stop - the runner's commands, the engine's states, and nothing left sounding when it stops.
#[test]
fn a_score_records_overdubs_plays_and_stops_a_midi_loop() {
    use crate::engine::runner::Runner;
    use crate::engine::timeline::{TimeSignature, Timeline};
    let section = |id: &str, state: &str| {
        format!("  - id: {id}\n    bars: 2\n    autorelease: true\n    tracks:\n      keys: {state}\n")
    };
    let yaml = format!(
        "bpm: 100\ntime_signature: 4/4\ntracks:\n  keys: {{input: instrument, loop: midi}}\nsections:\n{}{}{}{}",
        section("rec", "record"),
        section("dub", "overdub"),
        section("play", "play"),
        section("stop", "stop"),
    );
    let score = crate::score::compile_score(&yaml).expect("Partitur");
    assert!(crate::score::score_notes(&yaml).is_empty(), "loop: midi ist kein Hinweis mehr");
    let mut rig = rig();
    let timeline = Timeline::new(48_000, 100.0, TimeSignature::new(4, 4));
    let mut runner = Runner::new(score, timeline, BLOCK as u32).expect("Runner");
    for cmd in super::runner::setup_commands(runner.score()) {
        rig.sim.send(cmd);
    }
    rig.sim.send(Command::SetLoopKind {
        track: 0,
        midi: true,
    });
    runner.start(0);
    for cmd in runner.take_commands() {
        rig.sim.send(cmd);
    }
    let first = runner.armed_at().expect("armiert").1;
    // The run's own grid: the start made its moment bar 1.
    let timeline = runner.scheduler().timeline;
    let bar = |n: u64| timeline.bar_start(first + n);
    let step = |rig: &mut Rig, runner: &mut Runner, to: u64| {
        while rig.sim.pos() < to {
            rig.sim.step();
            runner.tick(rig.sim.pos());
            for cmd in runner.take_commands() {
                rig.sim.send(cmd);
            }
        }
    };
    step(&mut rig, &mut runner, bar(0) + 5_000);
    let at = bar(0) + 10_000;
    step(&mut rig, &mut runner, at + OUT);
    play(&mut rig, at, [0x90, 60, 100]);
    step(&mut rig, &mut runner, at + 2_000 + OUT);
    play(&mut rig, at + 2_000, [0x80, 60, 0]);
    step(&mut rig, &mut runner, bar(2) + 5_000);
    assert_eq!(rig.sim.core.track(0).state(bar(2)), TrackState::Overdub);
    let at = bar(2) + 30_000;
    step(&mut rig, &mut runner, at + OUT);
    play(&mut rig, at, [0x90, 64, 100]);
    step(&mut rig, &mut runner, at + 2_000 + OUT);
    play(&mut rig, at + 2_000, [0x80, 64, 0]);
    step(&mut rig, &mut runner, bar(6));
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 2);
    assert_eq!(track.loop_len(), bar(2) - bar(0));
    let got = sent(&rig, bar(4), bar(6));
    assert!(got.contains(&(bar(4) + 10_000, [0x90, 60, 100])), "{got:?}");
    assert!(got.contains(&(bar(4) + 30_000, [0x90, 64, 100])), "{got:?}");
    step(&mut rig, &mut runner, bar(7));
    assert!(sent(&rig, bar(6) + 1_000, bar(7)).is_empty());
    assert!(sounding(&rig).is_empty());
}

/// Transpose: a pad bank that sends an octave low reaches the drum kit an octave up. The key range
/// is checked on the note as it comes in; a note pushed out of 0..=127 is dropped; a change of the
/// shift ends the held notes, so no off goes out with the wrong shift; a MIDI loop stores what the
/// instrument got.
#[test]
fn transpose_shifts_live_notes_after_the_key_range() {
    let mut rig = rig();
    rig.feeds
        .send(FeedMsg::Route {
            track: 0,
            route: TrackRoute {
                ports: 1,
                channels: TrackRoute::ALL_CHANNELS,
                low: 36,
                high: 51,
            },
        })
        .unwrap();
    rig.feeds
        .send(FeedMsg::Transpose {
            track: 0,
            semitones: 12,
        })
        .unwrap();
    record_first_pass(&mut rig);
    play(&mut rig, 10_000, [0x99, 36, 100]);
    play(&mut rig, 11_000, [0x89, 36, 0]);
    // Outside the range as it comes in: stays with the bindings, even though 54 + 12 would not be.
    play(&mut rig, 12_000, [0x99, 54, 100]);
    play(&mut rig, 13_000, [0x99, 40, 100]);
    rig.sim.run_to(20_000);
    let got: Vec<[u8; 3]> = sent(&rig, 0, 20_000).into_iter().map(|(_, m)| m).collect();
    assert_eq!(got, vec![[0x99, 48, 100], [0x89, 48, 0], [0x99, 52, 100]]);
    // The shift changes while 40 (sounding as 52) is held: 52 is ended right away.
    rig.feeds
        .send(FeedMsg::Transpose {
            track: 0,
            semitones: -12,
        })
        .unwrap();
    rig.sim.run_to(22_000);
    play(&mut rig, 25_000, [0x89, 40, 0]);
    rig.sim.run_to(30_000);
    assert!(sounding(&rig).is_empty(), "{:?}", sounding(&rig));
    // A shift past the top of the keyboard drops the note.
    assert_eq!(super::source::transposed([0x90, 120, 1], 12), None);
    assert_eq!(super::source::transposed([0xB0, 64, 127], 12), Some([0xB0, 64, 127]));
    // The loop stored the shifted notes.
    rig.sim.run_to(L + 3 * C);
    let notes: Vec<u8> = raw(&rig, 0)
        .iter()
        .filter(|e| e.ev.kind == NoteKind::On)
        .map(|e| e.ev.d1)
        .collect();
    assert_eq!(notes, vec![48, 52]);
}

// ---------------------------------------------------------------------------------------------
// `stack` on an empty MIDI track: the latch is on before the loop-defining take begins
// ---------------------------------------------------------------------------------------------

/// What the score runner sends for `stack` on an empty track: the latch first, then the first pass
/// like `record` over `0..L`, then the latch off in the middle of pass `last`.
fn stack_from_empty(rig: &mut Rig, last: u64) {
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: true,
        at: 0,
    });
    record_first_pass(rig);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: false,
        at: last * L + L / 2,
    });
}

/// The owner's case: the first pass is left empty (he has to get from the guitar to the pads),
/// then three passes are played. The empty pass leaves no layer but keeps the grid - origin and
/// length are those of the first pass - and the first pass actually played is layer 1.
#[test]
fn a_stack_with_an_empty_first_pass_keeps_the_grid_and_starts_with_the_first_played_pass() {
    let mut rig = rig();
    stack_from_empty(&mut rig, 3);
    for (pass, note) in [(1u64, 50u8), (2, 52), (3, 55)] {
        play(&mut rig, pass * L + 10_000, [0x99, note, 90]);
        play(&mut rig, pass * L + 12_000, [0x89, note, 0]);
    }
    rig.sim.run_to(6 * L);
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 3, "der leere erste Durchlauf hinterlaesst keine Ebene");
    assert_eq!(track.origin(), 0, "das Raster ist das des ersten Durchlaufs");
    assert_eq!(track.loop_len(), L);
    assert_eq!(track.muted_mask(), 0, "keine stummgeschaltete Platzhalter-Ebene bleibt");
    assert!(track.take().is_none());
    for (layer, note) in [(0usize, 50u8), (1, 52), (2, 55)] {
        let r = raw(&rig, layer);
        assert_eq!(r.len(), 2, "Ebene {layer}: {r:?}");
        assert_eq!(r[0].ev.d1, note);
        assert_eq!(r[0].frame.rem_euclid(L as i64), 10_000, "taktgenau");
    }
    let ons: Vec<u8> = sent(&rig, 5 * L, 6 * L)
        .iter()
        .filter(|(_, m)| m[0] == 0x99)
        .map(|(_, m)| m[1])
        .collect();
    assert_eq!(ons, vec![50, 52, 55]);
    assert_eq!(rig.sim.latest_status().unwrap().refusal, super::command::Refusal::None);
    assert!(sounding(&rig).is_empty());
}

/// A hit a hair before the one of the first played pass - in the pre-roll window, while the empty
/// first pass is still running - belongs to the pass it was meant for, not to the empty one.
#[test]
fn an_early_hit_after_an_empty_first_pass_lands_in_the_pass_it_was_meant_for() {
    let mut rig = rig();
    stack_from_empty(&mut rig, 2);
    play(&mut rig, L - 500, [0x99, 36, 120]);
    play(&mut rig, L - 200, [0x89, 36, 0]);
    play(&mut rig, L + 30_000, [0x99, 38, 100]);
    play(&mut rig, L + 31_000, [0x89, 38, 0]);
    // Pass 2: something played, plus an early hit at its end. A pass with something in it keeps
    // such a hit where it was played, as before - and the latch ends here, so nothing follows.
    play(&mut rig, 2 * L + 20_000, [0x99, 42, 100]);
    play(&mut rig, 2 * L + 21_000, [0x89, 42, 0]);
    play(&mut rig, 3 * L - 500, [0x99, 40, 120]);
    play(&mut rig, 3 * L - 200, [0x89, 40, 0]);
    rig.sim.run_to(5 * L);
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 2, "Durchlauf 1 und 2");
    let second: Vec<(u8, i64)> = raw(&rig, 1)
        .iter()
        .filter(|e| e.ev.kind == NoteKind::On)
        .map(|e| (e.ev.d1, e.frame.rem_euclid(L as i64)))
        .collect();
    assert_eq!(second, vec![(42, 20_000), (40, (L - 500) as i64)]);
    assert_eq!(track.origin(), 0);
    let first = raw(&rig, 0);
    let notes: Vec<(u8, i64)> = first
        .iter()
        .filter(|e| e.ev.kind == NoteKind::On)
        .map(|e| (e.ev.d1, e.frame))
        .collect();
    assert_eq!(notes, vec![(36, -500), (38, 30_000)], "{first:?}");
    // The early hit plays where it was played: 500 frames before the one, every pass.
    let got = sent(&rig, 4 * L - 1_000, 4 * L);
    assert!(got.contains(&(4 * L - 500, [0x99, 36, 120])), "{got:?}");
    assert!(sounding(&rig).is_empty());
}

/// Nothing played at all until the latch ends: the track is empty again - no silent layer, no
/// refusal - and playing it afterwards is simply silence.
#[test]
fn a_stack_in_which_nothing_was_played_leaves_the_track_empty() {
    let mut rig = rig();
    stack_from_empty(&mut rig, 2);
    rig.sim.run_to(3 * L + 2_000);
    let status = rig.sim.latest_status().unwrap();
    assert_eq!(status.tracks()[0].layers, 0);
    assert_eq!(status.tracks()[0].state, TrackState::Empty);
    // What the runner sends for a later `play` / `stop`: harmless on an empty track.
    rig.sim.send(Command::StartPlay { track: 0, at: 4 * L });
    rig.sim.send(Command::StopPlay { track: 0, at: 5 * L });
    rig.sim.run_to(6 * L);
    let status = rig.sim.latest_status().unwrap();
    assert_eq!(status.refusal, super::command::Refusal::None);
    assert_eq!(status.ignored_commands, 0);
    assert!(sent(&rig, L, 6 * L).iter().all(|(_, m)| m[0] & 0xF0 != 0x90));
}

/// The owner's score, as he wrote it: two loops recorded in turn, then **at once** the drums on
/// `stack` on an empty MIDI track - the first pass left empty while he gets from the guitar to the
/// pads, a hit a hair before the one of the second pass, three played passes, Release, and the
/// groove plays on. `played: false`: nothing is played on the pads at all.
fn owners_drums(played: bool) -> (Rig, Vec<u64>) {
    use crate::engine::runner::Runner;
    use crate::engine::timeline::{TimeSignature, Timeline};
    let yaml = "\
bpm: 100
time_signature: 4/4
tracks:
  Drums: {input: instrument, loop: midi, grid: 1/8t, midi: {device: \"MPD218 Port A\", channel: 10}}
  melodic: {input: 1}
  bass: {input: 2}
sections:
  - id: melodic
    bars: 4
    autorelease: true
    tracks:
      melodic: record
  - id: bass
    bars: 4
    autorelease: true
    tracks:
      melodic: play
      bass: record
  - id: drums
    bars: 4
    autorelease: false
    quantize: loop
    tracks:
      Drums: stack
      melodic: play
      bass: play
  - id: groove
    bars: 4
    autorelease: false
    tracks:
      Drums: play
      melodic: play
      bass: play
";
    let score = crate::score::compile_score(yaml).expect("Partitur");
    assert!(crate::score::score_notes(yaml).is_empty());
    let mut rig = rig_with(0, 2);
    let timeline = Timeline::new(48_000, 100.0, TimeSignature::new(4, 4));
    let mut runner = Runner::new(score, timeline, BLOCK as u32).expect("Runner");
    for cmd in super::runner::setup_commands(runner.score()) {
        rig.sim.send(cmd);
    }
    runner.start(0);
    for cmd in runner.take_commands() {
        rig.sim.send(cmd);
    }
    let first = runner.armed_at().expect("armiert").1;
    // The run's own grid: the start made its moment bar 1.
    let timeline = runner.scheduler().timeline;
    let bar = |n: u64| timeline.bar_start(first + n);
    let step = |rig: &mut Rig, runner: &mut Runner, to: u64| {
        while rig.sim.pos() < to {
            rig.sim.step();
            runner.tick(rig.sim.pos());
            for cmd in runner.take_commands() {
                rig.sim.send(cmd);
            }
        }
    };
    let hit = |rig: &mut Rig, runner: &mut Runner, at: u64, note: u8| {
        step(rig, runner, at + OUT);
        play(rig, at, [0x99, note, 100]);
        step(rig, runner, at + 1_000 + OUT);
        play(rig, at + 1_000, [0x89, note, 0]);
    };
    // The drums section: bars 8..12 is its first pass, which stays empty.
    let passes: Vec<u64> = (0..5).map(|n| bar(8 + 4 * n)).collect();
    if played {
        // 300 frames before the one of pass 2 - inside the pre-roll, while pass 1 still runs.
        hit(&mut rig, &mut runner, passes[1] - 300, 36);
        for (n, note) in [(1usize, 38u8), (2, 42), (3, 46)] {
            hit(&mut rig, &mut runner, passes[n] + 20_000, note);
        }
    }
    // Release in the middle of the fourth pass (the third played one).
    step(&mut rig, &mut runner, passes[3] + 100_000);
    runner.next(rig.sim.pos());
    for cmd in runner.take_commands() {
        rig.sim.send(cmd);
    }
    assert_eq!(runner.armed_at().unwrap().1, first + 24, "am Ende des Durchlaufs");
    step(&mut rig, &mut runner, bar(33));
    assert_eq!(runner.view(rig.sim.pos()).section, Some(3), "groove laeuft");
    let status = rig.sim.latest_status().unwrap();
    assert_eq!(status.ignored_commands, 0, "{:?}", status.refusal);
    (rig, passes)
}

#[test]
fn the_owners_drums_start_with_an_empty_pass_and_stack_three_layers_on_the_grid() {
    let (rig, passes) = owners_drums(true);
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 3, "der leere erste Durchlauf hinterlaesst keine Ebene");
    assert_eq!(track.origin(), passes[0], "Ursprung = Sektionsanfang");
    assert_eq!(track.loop_len(), passes[1] - passes[0], "Laenge = bars");
    assert!(track.playing());
    let ons = |layer: usize| -> Vec<(u8, i64)> {
        raw(&rig, layer)
            .iter()
            .filter(|e| e.ev.kind == NoteKind::On)
            .map(|e| (e.ev.d1, e.frame))
            .collect()
    };
    // The early hit belongs to the first played pass, just before its one.
    assert_eq!(ons(0), vec![(36, -300), (38, 20_000)]);
    assert_eq!(ons(1), vec![(42, 20_000)]);
    assert_eq!(ons(2), vec![(46, 20_000)]);
    // The other two loops: the same grid in bars, one take each.
    let melodic = rig.sim.core.track(1);
    let bass = rig.sim.core.track(2);
    assert_eq!(melodic.loop_len(), track.loop_len());
    assert_eq!(bass.origin() + bass.loop_len(), track.origin(), "taktgenau hintereinander");
    // The groove plays every hit of the three layers.
    let got = sent(&rig, passes[4], passes[4] + track.loop_len() - 1_000);
    let mut notes: Vec<u8> = got.iter().filter(|(_, m)| m[0] == 0x99).map(|(_, m)| m[1]).collect();
    notes.sort();
    assert_eq!(notes, vec![38, 42, 46], "{got:?}");
    assert!(sent(&rig, passes[4] - 1_000, passes[4]).iter().any(|(_, m)| *m == [0x99, 36, 100]));
}

#[test]
fn the_owners_drums_with_nothing_played_leave_the_track_empty_and_the_groove_runs_on() {
    let (rig, _passes) = owners_drums(false);
    let track = rig.sim.core.track(0);
    assert_eq!(track.layer_count(), 0);
    assert!(rig.sim.core.track(1).playing());
    assert!(rig.sim.core.track(2).playing());
}

// ---------------------------------------------------------------------------------------------
// What is played live sounds once in the pass it is recorded in
// ---------------------------------------------------------------------------------------------

/// 20 ms at 48 kHz: how early a drummer hits before the grid line.
const EARLY: u64 = 960;

/// Note ons of key `note` the instrument got in `from..to`.
fn ons(rig: &Rig, from: u64, to: u64, note: u8) -> Vec<u64> {
    sent(rig, from, to)
        .into_iter()
        .filter(|(_, m)| m[0] & 0xF0 == 0x90 && m[2] > 0 && m[1] == note)
        .map(|(at, _)| at)
        .collect()
}

/// One drum hit: on at `at`, off 400 frames later (pads send short notes).
fn hit(rig: &mut Rig, at: u64, note: u8) {
    play(rig, at, [0x99, note, 110]);
    play(rig, at + 400, [0x89, note, 0]);
}

fn grid_rig(grid: NoteGrid) -> Rig {
    let mut rig = rig();
    rig.sim.clips.as_mut().unwrap().set_grid(0, grid);
    rig
}

/// One grid step in frames at 100 BPM (a quarter is 28 800 frames).
fn step_of(grid: NoteGrid) -> u64 {
    (grid.step().unwrap() as u64) * 28_800 / u64::from(super::source::PPQ)
}

/// The owner's report: a hit 20 ms before a grid line in the middle of the recording pass sounds
/// twice - live, and again from the quantised play list on its grid line. It must sound once, live;
/// the loop plays it from the next pass on, on the line.
#[test]
fn an_early_hit_while_recording_sounds_once_in_its_pass() {
    for grid in [NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let s = step_of(grid);
        let mut rig = grid_rig(grid);
        record_first_pass(&mut rig);
        let line = 9 * s;
        hit(&mut rig, line - EARLY, 38);
        rig.sim.run_to(3 * L);
        let first = ons(&rig, 0, L, 38);
        assert_eq!(first.len(), 1, "{grid:?}: Aufnahme-Durchlauf {first:?}");
        assert_eq!(ons(&rig, L, 2 * L, 38), vec![L + line], "{grid:?}: naechster Durchlauf");
        assert_eq!(ons(&rig, 2 * L, 3 * L, 38), vec![2 * L + line], "{grid:?}");
        assert!(sounding(&rig).is_empty());
    }
}

/// The same while overdubbing: the old layer plays on as before, the new hit sounds once.
#[test]
fn an_early_hit_while_overdubbing_sounds_once_in_its_pass() {
    for grid in [NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let s = step_of(grid);
        let mut rig = grid_rig(grid);
        record_first_pass(&mut rig);
        hit(&mut rig, 2 * s, 36);
        rig.sim.send(Command::StartOverdub { track: 0, at: L });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: 2 * L,
        });
        let line = 5 * s;
        hit(&mut rig, L + line - EARLY, 38);
        rig.sim.run_to(4 * L);
        let dub = ons(&rig, L, 2 * L, 38);
        assert_eq!(dub.len(), 1, "{grid:?}: Overdub-Durchlauf {dub:?}");
        assert_eq!(ons(&rig, L, 2 * L, 36), vec![L + 2 * s], "{grid:?}: alte Ebene spielt weiter");
        for pass in [2, 3] {
            assert_eq!(ons(&rig, pass * L, (pass + 1) * L, 38), vec![pass * L + line], "{grid:?}");
            assert_eq!(ons(&rig, pass * L, (pass + 1) * L, 36), vec![pass * L + 2 * s], "{grid:?}");
        }
        assert!(sounding(&rig).is_empty());
    }
}

/// Edge cases in one place: a hit in the pre-roll, just after the one, late, and just before the
/// end of the recording (its line is the next pass's one) - with and without a grid. Each sounds
/// once in the pass it is played in, and once per later pass from the loop.
#[test]
fn hits_at_the_edges_of_a_pass_sound_once() {
    for grid in [NoteGrid::None, NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let mut rig = grid_rig(grid);
        rig.sim.run_to(20_000);
        let start = 40_000u64;
        rig.sim.send(Command::StartRecord { track: 0, at: start });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: start + L,
        });
        rig.sim.send(Command::StartPlay {
            track: 0,
            at: start + L,
        });
        hit(&mut rig, start - 300, 36);
        hit(&mut rig, start + 500, 38);
        hit(&mut rig, start + 4 * 28_800 + 700, 42);
        hit(&mut rig, start + L - EARLY, 46);
        rig.sim.run_to(start + 4 * L + 2_000);
        let pass = |n: u64| (start + n * L - 2_000, start + (n + 1) * L - 2_000);
        for note in [36u8, 38, 42] {
            let (a, b) = pass(0);
            let got = ons(&rig, a, b, note);
            assert_eq!(got.len(), 1, "{grid:?} Note {note}: Aufnahme-Durchlauf {got:?}");
        }
        // No flam anywhere: two ons of the same key are a loop apart, never a step or less.
        for note in [36u8, 38, 42, 46] {
            let all = ons(&rig, 0, start + 4 * L, note);
            assert!(all.len() >= 3, "{grid:?} Note {note}: {all:?}");
            for w in all.windows(2) {
                assert!(w[1] - w[0] > L / 2, "{grid:?} Note {note} doppelt: {all:?}");
            }
        }
        for n in [1u64, 2] {
            for note in [38u8, 42] {
                let (a, b) = pass(n);
                let got = ons(&rig, a, b, note);
                assert_eq!(got.len(), 1, "{grid:?} Note {note}: Durchlauf {n} {got:?}");
            }
        }
        // 46 was heard live 20 ms before the end; its line is the next one. No flam there: the
        // loop plays it from the one after.
        let flam = ons(&rig, start + L - EARLY, start + L + 7_200, 46);
        assert_eq!(flam.len(), 1, "{grid:?}: 46 an der Grenze {flam:?}");
        let (a, b) = pass(2);
        assert_eq!(ons(&rig, a, b, 46).len(), 1, "{grid:?}: 46 spaeter je Durchlauf einmal");
        assert!(sounding(&rig).is_empty(), "{grid:?}: {:?}", sounding(&rig));
    }
}

/// A key held down while overdubbing on a grid: the loop sends neither its on nor its off in that
/// pass, so the live note is neither struck twice nor cut short; from the next pass on the loop
/// plays it on the line with its length, and nothing hangs.
#[test]
fn a_held_key_while_overdubbing_is_left_alone_by_the_loop() {
    let grid = NoteGrid::Sixteenth;
    let s = step_of(grid);
    let mut rig = grid_rig(grid);
    record_first_pass(&mut rig);
    hit(&mut rig, 2 * s, 36);
    rig.sim.send(Command::StartOverdub { track: 0, at: L });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: 2 * L,
    });
    let line = 4 * s;
    play(&mut rig, L + line - EARLY, [0x90, 60, 100]);
    play(&mut rig, L + line - EARLY + 10_000, [0x80, 60, 0]);
    rig.sim.run_to(4 * L);
    let keys = |from: u64, to: u64| -> Vec<(u64, [u8; 3])> {
        sent(&rig, from, to).into_iter().filter(|(_, m)| m[1] == 60).collect()
    };
    let dub = keys(L, 2 * L);
    assert_eq!(dub.len(), 2, "nur live: an und aus {dub:?}");
    assert_eq!(dub[0].1, [0x90, 60, 100]);
    assert_eq!(dub[1].1, [0x80, 60, 0]);
    assert_eq!(keys(2 * L, 3 * L), vec![
        (2 * L + line, [0x90, 60, 100]),
        (2 * L + line + 10_000, [0x80, 60, 0])
    ]);
    assert!(sounding(&rig).is_empty());
}

/// Continuous overdub over three passes: every hit of every layer sounds once in its own pass, and
/// from the next pass on once per pass, on its line.
#[test]
fn latched_passes_with_early_hits_sound_once_each() {
    let grid = NoteGrid::Sixteenth;
    let s = step_of(grid);
    let mut rig = grid_rig(grid);
    record_first_pass(&mut rig);
    hit(&mut rig, 3 * s - EARLY, 36);
    rig.sim.run_to(L / 2);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: true,
        at: 0,
    });
    for (pass, note) in [(1u64, 38u8), (2, 42), (3, 46)] {
        hit(&mut rig, pass * L + (2 + pass) * 4 * s - EARLY, note);
    }
    rig.sim.run_to(4 * L - 10_000);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: false,
        at: 0,
    });
    rig.sim.run_to(6 * L);
    for (pass, note) in [(0u64, 36u8), (1, 38), (2, 42), (3, 46)] {
        let line = if pass == 0 { 3 * s } else { (2 + pass) * 4 * s };
        for p in 0..6u64 {
            let got = ons(&rig, p * L, (p + 1) * L, note);
            let want = usize::from(p >= pass);
            assert_eq!(got.len(), want, "Note {note}, Durchlauf {p}: {got:?}");
            if p > pass {
                assert_eq!(got, vec![p * L + line], "Note {note}, Durchlauf {p}");
            }
        }
    }
    assert!(sounding(&rig).is_empty());
}

// ---------------------------------------------------------------------------------------------
// The owner's drum take, played back exactly: every hit once per pass, nothing missing
// ---------------------------------------------------------------------------------------------

/// The owner's take (see the data file): frame from the one of the recording pass, message.
const OWNER_TAKE: &[(i64, [u8; 3])] = &include!("../../tests/data/owner-drums-124.rs");

/// The rig of [`rig_with`] at `bpm`.
fn rig_at(bpm: f64, latency: u32, extra: usize) -> Rig {
    let mut tracks = vec![TrackSpec::instrument()];
    for i in 0..extra {
        tracks.push(TrackSpec::on(i));
    }
    let spec = SimSpec {
        tracks,
        loopback_channel: None,
        block: BLOCK,
        bars: 4,
        bpm,
        input_channels: extra.max(1),
        ..SimSpec::default()
    };
    let mut sim = Sim::new(spec, silence());
    let (mut instruments, mut feeds, endpoint) = source_channel(16);
    sim.core.attach_sources(endpoint, BLOCK * 4);
    sim.with_clock(OUT);
    sim.with_clips();
    let (keys, handle) = port_feed();
    feeds
        .send(FeedMsg::Attach(handle.take_feed(0).unwrap()))
        .unwrap();
    feeds
        .send(FeedMsg::Route {
            track: 0,
            route: TrackRoute {
                ports: 1,
                channels: TrackRoute::ALL_CHANNELS,
                low: 0,
                high: 127,
            },
        })
        .unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    instruments
        .send(InstrumentMsg::Install {
            track: 0,
            instrument: Instrument::new(
                Box::new(Recorder {
                    seen: Arc::clone(&seen),
                    blocks: 0,
                    frames: 0,
                }),
                SourceShared::new(latency),
            ),
        })
        .unwrap();
    sim.send(Command::SetLoopKind {
        track: 0,
        midi: true,
    });
    Rig {
        sim,
        _instruments: instruments,
        feeds,
        keys,
        seen,
    }
}


/// Twelve short takes on a 1/16 grid, each cleared again: every clip in circulation has had a
/// play list from the control thread swapped in - what the owner's app had after his earlier runs
/// on a grid.
fn use_every_clip_on_a_grid(rig: &mut Rig) {
    rig.sim.clips.as_mut().unwrap().set_grid(0, NoteGrid::Sixteenth);
    for _ in 0..12 {
        let p = rig.sim.pos() + 2_000;
        rig.sim.send(Command::StartRecord { track: 0, at: p });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: p + 24_000,
        });
        play(rig, p + 1_000, [0x99, 36, 100]);
        play(rig, p + 1_400, [0x89, 36, 0]);
        rig.sim.run_to(p + 24_000 + 4 * C);
        let at = rig.sim.pos() + BLOCK as u64;
        rig.sim.send(Command::ClearTrack { track: 0, at });
        rig.sim.run_to(at + 4 * C);
    }
    assert_eq!(rig.sim.core.track(0).layer_count(), 0);
}

/// The owner's score at 124 BPM: three guitar loops (the bass one loops until Release, given in
/// its second pass), then the drums in the states `drums` - one section of four bars each, the
/// last one looping - with his take played from the one of drum pass `played_in` (0: the first).
/// `used`: the clips have been used on a grid before. Returns the rig, that one and the loop
/// length.
fn owners_take(
    grid: NoteGrid,
    drums: &[&str],
    latency: u32,
    played_in: u64,
    used: bool,
) -> (Rig, u64, u64) {
    use crate::engine::runner::Runner;
    use crate::engine::timeline::{TimeSignature, Timeline};
    let grid_yaml = match grid {
        NoteGrid::None => String::new(),
        g => format!(", grid: {}", g.as_str()),
    };
    let mut yaml = format!(
        "bpm: 124\ntime_signature: 4/4\ntracks:\n  Drums: {{input: instrument, midi: {{device: \"MPD218 Port A\", channel: 10}}, loop: midi{grid_yaml}}}\n  melodic_1: {{input: 1}}\n  melodic_2: {{input: 2}}\n  bass: {{input: 3}}\nsections:\n"
    );
    let section = |id: &str, release: bool, tracks: &[(&str, &str)]| {
        let mut s = format!("  - id: {id}\n    bars: 4\n");
        if release {
            s.push_str("    autorelease: true\n");
        } else {
            s.push_str("    autorelease: false\n    quantize: loop\n");
        }
        s.push_str("    tracks:\n");
        for (t, state) in tracks {
            s.push_str(&format!("      {t}: {state}\n"));
        }
        s
    };
    yaml.push_str(&section("melodic_1", true, &[("melodic_1", "record")]));
    yaml.push_str(&section("melodic_2", true, &[("melodic_1", "play"), ("melodic_2", "record")]));
    yaml.push_str(&section("bass", false, &[
        ("melodic_1", "play"),
        ("melodic_2", "play"),
        ("bass", "record"),
    ]));
    for (i, state) in drums.iter().enumerate() {
        let last = i + 1 == drums.len();
        yaml.push_str(&section(&format!("drums_{}", i + 1), !last, &[
            ("melodic_1", "play"),
            ("melodic_2", "play"),
            ("bass", "play"),
            ("Drums", state),
        ]));
    }
    let score = crate::score::compile_score(&yaml).expect("Partitur");
    let mut rig = rig_at(124.0, latency, 3);
    if used {
        use_every_clip_on_a_grid(&mut rig);
    }
    // What the host does with the score's grid.
    rig.sim.clips.as_mut().unwrap().set_grid(0, grid);
    let timeline = Timeline::new(48_000, 124.0, TimeSignature::new(4, 4));
    let mut runner = Runner::new(score, timeline, BLOCK as u32).expect("Runner");
    for cmd in super::runner::setup_commands(runner.score()) {
        rig.sim.send(cmd);
    }
    runner.start(rig.sim.pos());
    for cmd in runner.take_commands() {
        rig.sim.send(cmd);
    }
    let first = runner.armed_at().expect("armiert").1;
    // The run's own grid: the start made its moment bar 1.
    let timeline = runner.scheduler().timeline;
    let bar = |n: u64| timeline.bar_start(first + n);
    let step = |rig: &mut Rig, runner: &mut Runner, to: u64| {
        while rig.sim.pos() < to {
            rig.sim.step();
            runner.tick(rig.sim.pos());
            for cmd in runner.take_commands() {
                rig.sim.send(cmd);
            }
        }
    };
    // Release in the second pass of the bass section: the drums begin on bar 16 (the 17th).
    step(&mut rig, &mut runner, bar(13) + 10_000);
    runner.next(rig.sim.pos());
    for cmd in runner.take_commands() {
        rig.sim.send(cmd);
    }
    let len = bar(20) - bar(16);
    let one = bar(16) + played_in * len;
    for &(f, msg) in OWNER_TAKE {
        let at = (one as i64 + f) as u64;
        step(&mut rig, &mut runner, at + OUT);
        play(&mut rig, at, msg);
    }
    step(&mut rig, &mut runner, one + 4 * len);
    stop_drums(&mut rig);
    let status = rig.sim.latest_status().unwrap();
    assert_eq!(status.ignored_commands, 0, "{:?}", status.refusal);
    (rig, one, len)
}

/// Stop the drums, so that what is left sounding afterwards is what hangs.
fn stop_drums(rig: &mut Rig) {
    let at = rig.sim.pos() + BLOCK as u64;
    rig.sim.send(Command::StopPlay { track: 0, at });
    rig.sim.run_to(at + 4 * C);
}

/// Note ons the instrument got in pass `n` after the one at `one` (0: the pass played in), as
/// (offset from that pass's one, message), sorted. A pass is counted from 5000 frames before its
/// one, so the pre-roll hits belong to the pass they lead into.
fn ons_in_pass(rig: &Rig, one: u64, len: u64, n: u64) -> Vec<(i64, [u8; 3])> {
    let a = one + n * len - 5_000;
    let mut got: Vec<(i64, [u8; 3])> = sent(rig, a, a + len)
        .into_iter()
        .filter(|(_, m)| m[0] & 0xF0 == 0x90 && m[2] > 0)
        .map(|(at, m)| (at as i64 - (one + n * len) as i64, m))
        .collect();
    got.sort();
    got
}

/// What the owner played, as note ons at the offset from the one the loop plays them at - on the
/// nearest line of `grid` - sorted.
fn owners_ons(grid: NoteGrid) -> Vec<(i64, [u8; 3])> {
    // Frames per tick at 124 BPM.
    let fpt = 48_000.0 * 60.0 / 124.0 / f64::from(super::source::PPQ);
    let mut ons: Vec<(i64, [u8; 3])> = OWNER_TAKE
        .iter()
        .filter(|(_, m)| m[0] == 0x99)
        .map(|&(f, m)| match grid.step() {
            None => (f, m),
            Some(step) => {
                let tick = (f as f64 / fpt).round() as i64;
                let line = (tick as f64 / step as f64).round() as i64 * step;
                ((line as f64 * fpt).round() as i64, m)
            }
        })
        .collect();
    ons.sort();
    ons
}

/// The pass the take was played in has every hit once, live; every later pass up to `last` has
/// every hit once, where the loop plays it (a frame of rounding allowed on a grid); nothing is
/// left sounding. Without a grid, the pre-roll hits of the first pass after the take lie before
/// its one, at the end of the take's own pass: they sound there only if the loop already plays
/// in that pass (`preroll_first`), as in a stack; otherwise from the second pass on.
fn check_owners_passes(
    rig: &Rig,
    one: u64,
    len: u64,
    grid: NoteGrid,
    last: u64,
    preroll_first: bool,
    latency: i64,
) {
    // Sent as early as the instrument's latency, so that it is heard on time.
    let want: Vec<(i64, [u8; 3])> =
        owners_ons(grid).into_iter().map(|(f, m)| (f - latency, m)).collect();
    assert_eq!(want.len(), 53);
    let mut live: Vec<[u8; 3]> = ons_in_pass(rig, one, len, 0).iter().map(|(_, m)| *m).collect();
    let mut wanted: Vec<[u8; 3]> = want.iter().map(|(_, m)| *m).collect();
    live.sort();
    wanted.sort();
    assert_eq!(live, wanted, "{grid:?}: Aufnahme-Durchlauf - jeder Schlag einmal, live");
    for n in 1..=last {
        let got = ons_in_pass(rig, one, len, n);
        let want: Vec<(i64, [u8; 3])> =
            want.iter().filter(|(f, _)| n > 1 || preroll_first || *f + latency >= 0).copied().collect();
        let mut left = got.clone();
        let mut missing = Vec::new();
        for w in &want {
            match left.iter().position(|g| g.1 == w.1 && (g.0 - w.0).abs() <= 1) {
                Some(i) => {
                    left.remove(i);
                }
                None => missing.push(*w),
            }
        }
        assert!(
            missing.is_empty() && left.is_empty(),
            "{grid:?}, Durchlauf {n} nach der Aufnahme: {} von {} - fehlend {missing:?}, zu viel {left:?}",
            got.len() - left.len(),
            want.len()
        );
    }
    assert!(sounding(rig).is_empty(), "{grid:?}: {:?}", sounding(rig));
}

/// The owner's report, with his take: record, overdub, overdub without a grid - in an app whose
/// clips had been used on a grid before. Only the first few hits came back; now all of them, in
/// every pass after the recording.
#[test]
fn owners_take_without_grid_plays_every_hit_in_every_later_pass() {
    for used in [false, true] {
        let drums = ["record", "overdub", "overdub"];
        let (rig, one, len) = owners_take(NoteGrid::None, &drums, 0, 0, used);
        check_owners_passes(&rig, one, len, NoteGrid::None, 3, false, 0);
    }
}

/// The same take on the grids he uses, and with an instrument that reports latency.
#[test]
fn owners_take_on_a_grid_plays_every_hit_once_on_its_line() {
    let drums = ["record", "overdub", "overdub"];
    for grid in [NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let (rig, one, len) = owners_take(grid, &drums, 0, 0, true);
        check_owners_passes(&rig, one, len, grid, 3, false, 0);
    }
    let (rig, one, len) = owners_take(NoteGrid::None, &drums, 64, 0, true);
    check_owners_passes(&rig, one, len, NoteGrid::None, 3, false, 64);
}

/// `stack` in one section: the first pass is left empty, the take is played in the second, and the
/// stacking runs on over the passes after it.
#[test]
fn owners_take_in_a_stack_after_an_empty_pass_plays_every_hit_once() {
    for grid in [NoteGrid::None, NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let (rig, one, len) = owners_take(grid, &["stack"], 0, 1, true);
        check_owners_passes(&rig, one, len, grid, 2, true, 0);
        // The take is one layer with every hit; the passes after it added nothing.
        let ons_per_layer: Vec<usize> = (0..rig.sim.core.track(0).layer_count())
            .map(|l| raw(&rig, l).iter().filter(|e| e.ev.kind == NoteKind::On).count())
            .collect();
        assert_eq!(ons_per_layer.iter().sum::<usize>(), 53, "{grid:?}: {ons_per_layer:?}");
        assert_eq!(ons_per_layer[0], 53, "{grid:?}: {ons_per_layer:?}");
    }
}

/// Free mode: the take recorded, then overdub latched on for good - the passes after it are
/// overdubs in which nothing is played.
#[test]
fn owners_take_with_a_latched_overdub_plays_every_hit_once() {
    use crate::engine::timeline::{TimeSignature, Timeline};
    let timeline = Timeline::new(48_000, 124.0, TimeSignature::new(4, 4));
    for grid in [NoteGrid::None, NoteGrid::Sixteenth, NoteGrid::EighthTriplet] {
        let mut rig = rig_at(124.0, 0, 0);
        use_every_clip_on_a_grid(&mut rig);
        rig.sim.clips.as_mut().unwrap().set_grid(0, grid);
        let len = timeline.bar_start(4) - timeline.bar_start(0);
        let one = (rig.sim.pos() / len + 2) * len;
        rig.sim.send(Command::StartRecord { track: 0, at: one });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: one + len,
        });
        rig.sim.send(Command::StartPlay {
            track: 0,
            at: one + len,
        });
        rig.sim.send(Command::LatchOverdub {
            track: 0,
            on: true,
            at: one + len,
        });
        for &(f, msg) in OWNER_TAKE {
            play(&mut rig, (one as i64 + f) as u64, msg);
        }
        rig.sim.run_to(one + 4 * len);
        stop_drums(&mut rig);
        check_owners_passes(&rig, one, len, grid, 3, false, 0);
    }
}

/// Fast hits on one pad: each note on comes before the off of the hit before it. Every hit is
/// played back once per pass, and nothing is left sounding.
#[test]
fn fast_hits_on_one_pad_before_the_previous_off_all_come_back() {
    for grid in [NoteGrid::None, NoteGrid::Sixteenth] {
        let mut rig = rig();
        use_every_clip_on_a_grid(&mut rig);
        rig.sim.clips.as_mut().unwrap().set_grid(0, grid);
        let start = (rig.sim.pos() / L + 1) * L;
        rig.sim.send(Command::StartRecord { track: 0, at: start });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: start + L,
        });
        rig.sim.send(Command::StartPlay {
            track: 0,
            at: start + L,
        });
        // 32 hits a 1/16 apart (7200 frames at 100 BPM), a little late, each held 9000 frames.
        let hits: Vec<u64> = (0..32).map(|i| 7_200 * i + 1_000).collect();
        let mut events: Vec<(u64, [u8; 3])> = Vec::new();
        for &h in &hits {
            events.push((start + h, [0x99, 42, 100]));
            events.push((start + h + 9_000, [0x89, 42, 0]));
        }
        events.sort_by_key(|e| e.0);
        for (at, msg) in events {
            play(&mut rig, at, msg);
        }
        rig.sim.run_to(start + 4 * L);
        for pass in 1..4u64 {
            let got = ons(&rig, start + pass * L - 1_000, start + (pass + 1) * L - 1_000, 42);
            assert_eq!(got.len(), hits.len(), "{grid:?}, Durchlauf {pass}: {got:?}");
        }
        let at = rig.sim.pos() + BLOCK as u64;
        rig.sim.send(Command::StopPlay { track: 0, at });
        rig.sim.run_to(at + 4 * C);
        assert!(sounding(&rig).is_empty(), "{grid:?}: {:?}", sounding(&rig));
    }
}

// ---------------------------------------------------------------------------------------------
// Re-anchoring a MIDI loop (`Command::RestartLoop`)
// ---------------------------------------------------------------------------------------------

/// A playing MIDI loop re-anchored in the middle of a held note: the note of the old phase ends on
/// the very sample of the re-anchoring (its own note-off lies in the part of the loop that is now
/// skipped and would never come), and the loop then plays from its top - on time, the
/// instrument's latency included. Nothing is left hanging, and a stop afterwards still ends what
/// the new phase started.
#[test]
fn a_midi_loop_re_anchored_mid_note_leaves_nothing_hanging() {
    for latency in [0u32, 100] {
        let lat = u64::from(latency);
        let mut rig = rig_with(latency, 0);
        record_first_pass(&mut rig);
        play(&mut rig, 10_000, [0x90, 60, 100]);
        play(&mut rig, 80_000, [0x80, 60, 0]);
        // Pass 2 plays the note from L + 10_000 to L + 80_000. Re-anchor at L + 40_000.
        let at = L + 40_000;
        rig.sim.send(Command::RestartLoop {
            track: 0,
            at,
            play: false,
        });
        rig.sim.run_to(at + 85_000);
        assert_eq!(
            sent(&rig, L, at + 85_000),
            vec![
                (L + 10_000 - lat, [0x90, 60, 100]),
                // The old phase's note ends on the re-anchoring ...
                (at - lat, [0x80, 60, 0]),
                // ... and the loop starts over there: the note at its index 10_000.
                (at + 10_000 - lat, [0x90, 60, 100]),
                (at + 80_000 - lat, [0x80, 60, 0]),
            ],
            "Latenz {latency}"
        );
        rig.sim.run_to(at + 20_000 + L);
        assert_eq!(sounding(&rig), vec![(0, 60)], "Latenz {latency}: Durchlauf nach dem Neuanfang");
        let stop = rig.sim.pos() + 64;
        rig.sim.send(Command::StopPlay { track: 0, at: stop });
        rig.sim.run_to(stop + 4 * BLOCK as u64);
        assert!(sounding(&rig).is_empty(), "Latenz {latency}: nichts haengt");
    }
}

/// A stopped MIDI loop brought back with a re-anchoring (`play: true` plus the `StartPlay` for the
/// same sample, what the score runner sends): a note on the loop's very first frame is played,
/// moved forward by the instrument's latency - without the re-anchoring's read-ahead it would
/// fall before the moment the track starts and be lost.
#[test]
fn a_midi_loop_that_comes_back_plays_its_first_note() {
    for latency in [0u32, 100] {
        let lat = u64::from(latency);
        let mut rig = rig_with(latency, 0);
        rig.sim.send(Command::StartRecord { track: 0, at: 0 });
        rig.sim.send(Command::StopRecord { track: 0, at: L });
        // A note on the downbeat - in the pre-roll window, so it is stored at the loop's frame 0.
        play(&mut rig, 0, [0x90, 64, 90]);
        play(&mut rig, 5_000, [0x80, 64, 0]);
        rig.sim.run_to(L + 1_000);
        let at = L + 3 * 28_800 + 17;
        rig.sim.send(Command::RestartLoop { track: 0, at, play: true });
        rig.sim.send(Command::StartPlay { track: 0, at });
        rig.sim.run_to(at + 10_000);
        assert_eq!(
            sent(&rig, L + 2_000, at + 10_000),
            vec![(at - lat, [0x90, 64, 90]), (at + 5_000 - lat, [0x80, 64, 0])],
            "Latenz {latency}"
        );
        assert!(sounding(&rig).is_empty());
    }
}

// ---------------------------------------------------------------------------------------------
// `skip_end`: the end of a take that is not recorded (`Command::CutTake`)
// ---------------------------------------------------------------------------------------------

/// A MIDI take with a cut one beat before its end: a note before the cut is kept as played, a note
/// held across it ends on the cut (also when its off only comes after the take), and nothing that
/// starts after the cut is taken. The loop keeps its full length.
#[test]
fn a_cut_midi_take_ends_held_notes_on_the_cut_and_takes_nothing_after_it() {
    for latency in [0u32, 100] {
        let mut rig = rig_with(latency, 0);
        record_first_pass(&mut rig);
        let cut = L - 28_800;
        rig.sim.send(Command::CutTake { track: 0, at: 0, from: cut });
        play(&mut rig, 10_000, [0x90, 60, 100]);
        play(&mut rig, 20_000, [0x80, 60, 0]);
        play(&mut rig, L - 40_000, [0x90, 62, 100]);
        play(&mut rig, L - 30_000, [0x90, 65, 100]);
        play(&mut rig, L - 20_000, [0x90, 64, 100]);
        play(&mut rig, L - 15_000, [0x80, 64, 0]);
        play(&mut rig, L - 10_000, [0x80, 62, 0]);
        play(&mut rig, L + 3_000, [0x80, 65, 0]);
        rig.sim.run_to(2 * L + 2 * C);
        assert_eq!(rig.sim.core.track(0).loop_len(), L, "Latenz {latency}");
        let mut got: Vec<(i64, [u8; 3])> = play_list(&rig, 0).iter().map(|e| (e.frame, e.msg)).collect();
        got.sort();
        let cut = cut as i64;
        assert_eq!(
            got,
            vec![
                (10_000, [0x90, 60, 100]),
                (20_000, [0x80, 60, 0]),
                ((L - 40_000) as i64, [0x90, 62, 100]),
                ((L - 30_000) as i64, [0x90, 65, 100]),
                (cut, [0x80, 62, 0]),
                (cut, [0x80, 65, 0]),
            ],
            "Latenz {latency}"
        );
    }
}


// ---------------------------------------------------------------------------------------------
// A chord held over the end of the take (the owner's pad, 22.11.): it sounds the whole loop
// ---------------------------------------------------------------------------------------------

/// Frames of `from..to` in which the instrument had key `note` sounding, judged from everything it
/// was sent (one voice per key, as a synth without stacking hears it).
fn coverage(rig: &Rig, note: u8, from: u64, to: u64) -> u64 {
    let mut on_since: Option<u64> = None;
    let mut total = 0;
    for &(at, m) in rig.seen.lock().unwrap().iter() {
        if m[1] != note || !matches!(m[0] & 0xF0, 0x80 | 0x90) {
            continue;
        }
        let on = m[0] & 0xF0 == 0x90 && m[2] > 0;
        match (on, on_since) {
            (true, None) => on_since = Some(at),
            (false, Some(s)) => {
                total += at.min(to).saturating_sub(s.max(from));
                on_since = None;
            }
            _ => {}
        }
    }
    if let Some(s) = on_since {
        total += to.saturating_sub(s.max(from));
    }
    total
}

/// When the musician lets go of the chord, relative to the start of the take.
#[derive(Clone, Copy, Debug)]
enum Release {
    /// Shortly after the loop end - after the wrap, while the loop plays its first pass.
    AfterEnd,
    /// Exactly on the loop end.
    AtEnd,
    /// Just before the loop end.
    BeforeEnd,
    /// Not before the next pass is over.
    Late,
}

/// A two-note chord, pressed `on` frames from the start of a take that begins from nothing
/// (negative: a touch early, in the pre-roll), released as `release` says. Returns the rig, the
/// take's start, and the frame the keys went up (relative to the start).
fn held_chord(grid: NoteGrid, on: i64, release: Release) -> (Rig, u64, i64) {
    let mut rig = grid_rig(grid);
    rig.sim.run_to(20_000);
    let start = 40_000u64;
    rig.sim.send(Command::StartRecord { track: 0, at: start });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: start + L,
    });
    rig.sim.send(Command::StartPlay {
        track: 0,
        at: start + L,
    });
    let at = |f: i64| (start as i64 + f) as u64;
    play(&mut rig, at(on), [0x99, 62, 127]);
    play(&mut rig, at(on), [0x99, 55, 127]);
    // Channel pressure while holding, as the owner's pads send it.
    for k in 1..8 {
        play(&mut rig, at(on + k * 25_000), [0xD9, 120, 0]);
    }
    let up = match release {
        Release::AfterEnd => L as i64 + 20_000,
        Release::AtEnd => L as i64,
        Release::BeforeEnd => L as i64 - 300,
        Release::Late => 2 * L as i64 + 30_000,
    };
    play(&mut rig, at(up), [0x89, 62, 0]);
    play(&mut rig, at(up + 40), [0x89, 55, 0]);
    (rig, start, up)
}

/// The owner's report: a chord held over the end of a MIDI loop sounded while he recorded it, and
/// when the loop came round nothing was heard. Its note off came after the wrap and was stored
/// there (or, pressed a touch early, the note was "longer than the loop" and was ended right after
/// its own start) - a note of a few milliseconds - and his release, after the loop had struck the
/// chord again, ended the loop's notes as well.
///
/// Whatever the timing, on a grid or not: every stored note is shorter than the loop and not
/// empty, and every pass after the take has the chord sounding all the way through.
#[test]
fn a_chord_held_over_the_end_sounds_for_the_whole_loop_in_every_pass() {
    for grid in [NoteGrid::None, NoteGrid::Eighth] {
        for on in [-1_000i64, 0, 500] {
            for release in [Release::AfterEnd, Release::AtEnd, Release::BeforeEnd, Release::Late] {
                let name = format!("{grid:?}, an bei {on}, {release:?}");
                // Released before the end, a chord pressed after the start is just shorter than
                // the loop: nothing to test here.
                if matches!(release, Release::BeforeEnd) && on >= 0 {
                    continue;
                }
                let (mut rig, start, _) = held_chord(grid, on, release);
                rig.sim.run_to(start + 4 * L + 3 * C);

                // What was stored: each key one note, longer than nothing, shorter than the loop.
                let r = raw(&rig, 0);
                for key in [62u8, 55] {
                    let ev: Vec<&RawEvent> = r
                        .iter()
                        .filter(|e| e.ev.d1 == key && matches!(e.ev.kind, NoteKind::On | NoteKind::Off))
                        .collect();
                    assert_eq!(ev.len(), 2, "{name}: {ev:?}");
                    let len = ev[1].frame - ev[0].frame;
                    assert!(len > 0 && len < L as i64, "{name}: Laenge {len}");
                }
                // What was played: the chord through the whole of every pass - the first one too,
                // except where a chord pressed early without a grid struck before the loop played
                // (see the next test).
                let first = if grid == NoteGrid::None && on < 0 && matches!(release, Release::BeforeEnd) {
                    2
                } else {
                    1
                };
                for pass in first..4 {
                    let from = start + pass * L;
                    for key in [62u8, 55] {
                        let heard = coverage(&rig, key, from, from + L);
                        // A grid line moves the start by up to 500 frames here; a pre-roll note
                        // ends one frame before its own next start.
                        assert!(heard + 1_100 >= L, "{name}: Durchlauf {pass}, Taste {key}: {heard} von {L}");
                    }
                }
                // Stopped: nothing is left on.
                let at = rig.sim.pos() + 64;
                rig.sim.send(Command::StopPlay { track: 0, at });
                rig.sim.run_to(at + L);
                assert!(sounding(&rig).is_empty(), "{name}: {:?}", sounding(&rig));
            }
        }
    }
}

/// The one gap left: without a grid, a chord pressed a touch early for the one belongs to the end
/// of the loop, and its note on in the loop lies in the take itself, before the loop plays. Let go
/// before the end, the first pass after the take has nothing of it; from the second on it sounds
/// all the way. (Held over the end, the loop takes the held keys over - previous test.)
#[test]
fn a_chord_pressed_early_and_let_go_before_the_end_sounds_from_the_second_pass_without_a_grid() {
    let (mut rig, start, _) = held_chord(NoteGrid::None, -1_000, Release::BeforeEnd);
    rig.sim.run_to(start + 3 * L + C);
    let play = play_list(&rig, 0);
    let on = play.iter().find(|e| e.msg == [0x99, 62, 127]).unwrap();
    let off = play.iter().find(|e| e.msg == [0x89, 62, 0]).unwrap();
    assert_eq!(on.offset, (L - 1_000) as u32, "wo gespielt, vor der Eins");
    assert_eq!(off.offset, (L - 1_001) as u32, "ein Bild vor dem eigenen Wiederanschlag");
    assert!(coverage(&rig, 62, start + 2 * L, start + 3 * L) + 2 >= L);
}

/// The live key and the loop share one voice: the loop striking a key the musician still holds
/// strikes it afresh (off, on), his release then leaves the loop's note sounding, and the loop's
/// own off ends it. Stopping the loop while he still holds the key leaves his note alone.
#[test]
fn a_key_held_live_and_struck_by_the_loop_is_one_voice() {
    let mut rig = grid_rig(NoteGrid::Eighth);
    record_first_pass(&mut rig);
    play(&mut rig, 0, [0x90, 60, 100]);
    play(&mut rig, 50_000, [0x80, 60, 0]);
    // Pass 2: the musician presses the same key before the loop's note comes and holds it past.
    play(&mut rig, L - 2_000 + L, [0x90, 60, 90]);
    play(&mut rig, 2 * L + 10_000, [0x80, 60, 0]);
    rig.sim.run_to(2 * L + 60_000);
    let got: Vec<(u64, [u8; 3])> = sent(&rig, 2 * L - 3_000, 2 * L + 60_000)
        .into_iter()
        .filter(|(_, m)| m[1] == 60)
        .collect();
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(got[0].1, [0x90, 60, 90], "live");
    assert_eq!((got[1].0, got[1].1), (2 * L, [0x80, 60, 0]), "neu angeschlagen: aus ...");
    assert_eq!((got[2].0, got[2].1), (2 * L, [0x90, 60, 100]), "... und an");
    assert_eq!((got[3].0, got[3].1), (2 * L + 50_000, [0x80, 60, 0]), "das Aus der Schleife");

    // Held live while the loop is stopped: the loop's off does not end his note.
    let mut rig = grid_rig(NoteGrid::Eighth);
    record_first_pass(&mut rig);
    play(&mut rig, 0, [0x90, 60, 100]);
    play(&mut rig, 50_000, [0x80, 60, 0]);
    play(&mut rig, 2 * L - 2_000, [0x90, 60, 90]);
    rig.sim.run_to(2 * L + 10_000);
    let at = rig.sim.pos() + 64;
    rig.sim.send(Command::StopPlay { track: 0, at });
    rig.sim.run_to(2 * L + 30_000);
    assert_eq!(sounding(&rig), vec![(0, 60)], "die Live-Taste klingt weiter");
    play(&mut rig, 2 * L + 40_000, [0x80, 60, 0]);
    rig.sim.run_to(2 * L + 60_000);
    assert!(sounding(&rig).is_empty());
}

/// The same chord in an overdub on a playing loop: held over the end of the overdub pass, it
/// sounds through every pass after it, and nothing hangs once the loop stops.
#[test]
fn a_chord_held_over_the_end_of_an_overdub_sounds_for_the_whole_loop() {
    for grid in [NoteGrid::None, NoteGrid::Eighth] {
        let mut rig = grid_rig(grid);
        record_first_pass(&mut rig);
        hit(&mut rig, 10_000, 36);
        rig.sim.send(Command::StartOverdub { track: 0, at: L });
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: 2 * L,
        });
        play(&mut rig, L + 300, [0x90, 62, 100]);
        play(&mut rig, 2 * L + 20_000, [0x80, 62, 0]);
        rig.sim.run_to(5 * L);
        assert_eq!(rig.sim.core.track(0).layer_count(), 2, "{grid:?}");
        for pass in 2..5 {
            let heard = coverage(&rig, 62, pass * L, (pass + 1) * L);
            assert!(heard + 400 >= L, "{grid:?}: Durchlauf {pass}: {heard} von {L}");
        }
        let at = rig.sim.pos() + 64;
        rig.sim.send(Command::StopPlay { track: 0, at });
        rig.sim.run_to(at + L);
        assert!(sounding(&rig).is_empty(), "{grid:?}: {:?}", sounding(&rig));
    }
}
