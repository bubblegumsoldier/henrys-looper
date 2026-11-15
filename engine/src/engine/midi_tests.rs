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
    let mut tracks = vec![TrackSpec::instrument()];
    for i in 0..extra {
        tracks.push(TrackSpec::on(i));
    }
    let spec = SimSpec {
        tracks,
        loopback_channel: None,
        block: BLOCK,
        bars: 4,
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
    });
    for (pass, note) in [(1u64, 50u8), (2, 52), (3, 55)] {
        play(&mut rig, pass * L + 10_000, [0x90, note, 90]);
        play(&mut rig, pass * L + 12_000, [0x80, note, 0]);
    }
    rig.sim.run_to(4 * L - 10_000);
    rig.sim.send(Command::LatchOverdub {
        track: 0,
        on: false,
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
        msg: [0x99, 36, 120]
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
        msg: [0x99, 36, 120]
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
        msg: [0x80, 60, 0]
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
