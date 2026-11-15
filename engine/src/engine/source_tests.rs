//! Instrument tracks, offline: where a note played on a beat ends up, in the speakers and in the
//! loop, and what happens to notes that are sounding when things change.

use std::sync::{Arc, Mutex};

use rtrb::Producer;

use super::command::Command;
use super::sim::{Sim, SimSpec, TrackSpec, silence};
use super::source::testing::{Impulse, Recorder};
use super::source::{
    FeedMsg, FeedSender, Instrument, InstrumentMsg, InstrumentSender, LiveEvent, SourceShared,
    TrackRoute, port_feed, source_channel,
};
use super::track::TrackLatency;

const BLOCK: usize = 128;
/// Output latency of the simulated device, in frames.
const OUT: u64 = 300;

struct Rig {
    sim: Sim,
    instruments: InstrumentSender,
    feeds: FeedSender,
    keys: Producer<LiveEvent>,
}

fn rig(tracks: Vec<TrackSpec>, clock: bool) -> Rig {
    let spec = SimSpec {
        tracks,
        loopback_channel: None,
        block: BLOCK,
        bars: 2,
        ..SimSpec::default()
    };
    let mut sim = Sim::new(spec, silence());
    let (instruments, mut feeds, endpoint) = source_channel(16);
    sim.core.attach_sources(endpoint, BLOCK * 4);
    if clock {
        sim.with_clock(OUT);
    }
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
    Rig {
        sim,
        instruments,
        feeds,
        keys,
    }
}

fn install(rig: &mut Rig, track: usize, processor: Box<dyn super::source::SourceProcessor>, latency: u32) {
    rig.instruments
        .send(InstrumentMsg::Install {
            track,
            instrument: Instrument::new(processor, SourceShared::new(latency)),
        })
        .unwrap();
}

/// Play `msg` at the moment the musician hears output position `heard`: the event is stamped with
/// that host time and handed over in the first callback that starts after it.
fn play_heard(rig: &mut Rig, heard: u64, msg: [u8; 3]) {
    let t = rig.sim.heard_ns(heard);
    rig.sim.run_to(heard + OUT);
    rig.keys.push(LiveEvent { host_ns: t, msg }).unwrap();
}

fn nonzero(samples: &[f32]) -> Vec<usize> {
    samples
        .iter()
        .enumerate()
        .filter(|(_, v)| **v != 0.0)
        .map(|(i, _)| i)
        .collect()
}

/// The core promise: a key pressed while beat `B` is heard sounds `C` later - the same `C` for
/// every key - and is stored at `B` in the loop, to the frame. With and without an instrument that
/// reports latency of its own.
#[test]
fn a_note_played_on_the_heard_beat_lands_on_that_beat_in_the_layer() {
    for latency in [0u32, 100] {
        let mut rig = rig(vec![TrackSpec::instrument()], true);
        install(&mut rig, 0, Box::new(Impulse::new(latency as usize)), latency);
        rig.sim.send(Command::StartRecord { track: 0, at: 0 });
        let spb: f64 = 48_000.0 * 60.0 / 100.0;
        let beat = (1.5 * spb).round() as u64;
        let loop_len = 2 * 4 * spb as u64;
        rig.sim.send(Command::StopRecord {
            track: 0,
            at: loop_len,
        });
        play_heard(&mut rig, beat, [0x90, 60, 127]);
        play_heard(&mut rig, beat + 7, [0x80, 60, 0]);
        let c = OUT + BLOCK as u64;
        rig.sim.run_to(loop_len + c + u64::from(latency) + 2 * BLOCK as u64);

        assert_eq!(rig.sim.core.instrument_delay(), Some(c), "C = Ausgangslatenz + 1 Block");
        // Heard: C after the beat, plus the instrument's own latency.
        let heard = nonzero(&rig.sim.out_l);
        assert_eq!(
            heard.first().copied(),
            Some((beat + c + u64::from(latency)) as usize),
            "Latenz {latency}"
        );
        // Stored: on the beat, exactly.
        let track = rig.sim.core.track(0);
        assert_eq!(track.loop_len(), loop_len);
        let layer = track.layer(0).expect("eine Ebene");
        let left = layer.channel(0, loop_len);
        assert_eq!(nonzero(&left), vec![beat as usize], "Latenz {latency}");
        assert_eq!(left[beat as usize], 1.0);
        assert_eq!(rig.sim.core.late_events(), 0);
    }
}

/// A trim moves an instrument track's recording the way it moves an audio track's: a positive
/// value takes the material earlier.
#[test]
fn the_trim_shifts_an_instrument_recording_like_an_audio_one() {
    let mut spec = TrackSpec::instrument();
    spec.latency = TrackLatency::INHERITED.with_trim(10);
    let mut rig = rig(vec![spec], true);
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    rig.sim.send(Command::StartRecord { track: 0, at: 0 });
    rig.sim.send(Command::StopRecord {
        track: 0,
        at: 100_000,
    });
    play_heard(&mut rig, 20_000, [0x90, 60, 127]);
    rig.sim.run_to(102_000);
    let layer = rig.sim.core.track(0).layer(0).expect("eine Ebene");
    assert_eq!(nonzero(&layer.channel(0, 100_000)), vec![20_000 - 10]);
}

/// Since the transport can stand, an instrument must not depend on it: a standing clock still
/// plays every key.
#[test]
fn an_instrument_sounds_while_the_transport_stands() {
    let mut rig = rig(vec![TrackSpec::instrument()], true);
    rig.sim.core.hold_transport();
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    play_heard(&mut rig, 5_000, [0x90, 60, 127]);
    rig.sim.run_to(7_000);
    let c = OUT + BLOCK as u64;
    assert_eq!(nonzero(&rig.sim.out_l), vec![(5_000 + c) as usize]);
    let status = rig.sim.latest_status().expect("Status");
    assert!(!status.transport);
    let t = status.tracks()[0];
    assert!(t.instrument && t.instrument_loaded);
    assert_eq!(t.notes, 1, "die Aktivitaetsanzeige hat die Note gesehen");
    assert_eq!(t.held_notes, 1);
}

/// Without a clock (CLI, calibration) an event sounds at the top of the block it arrives in.
#[test]
fn without_a_clock_an_event_sounds_at_the_top_of_its_block() {
    let mut rig = rig(vec![TrackSpec::instrument()], false);
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    rig.sim.run_to(1_000);
    rig.keys
        .push(LiveEvent {
            host_ns: 0,
            msg: [0x90, 60, 127],
        })
        .unwrap();
    let at = rig.sim.pos();
    rig.sim.run_blocks(1);
    assert_eq!(nonzero(&rig.sim.out_l), vec![at as usize]);
}

fn recorder(rig: &mut Rig) -> Arc<Mutex<Vec<(u64, [u8; 3])>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    install(
        rig,
        0,
        Box::new(Recorder {
            seen: Arc::clone(&seen),
            blocks: 0,
            frames: 0,
        }),
        0,
    );
    seen
}

fn messages(seen: &Arc<Mutex<Vec<(u64, [u8; 3])>>>) -> Vec<[u8; 3]> {
    seen.lock().unwrap().iter().map(|(_, m)| *m).collect()
}

/// "All notes off" ends what is sounding - note offs from the tracker, the pedal up, CC 123 - and
/// nothing else.
#[test]
fn panic_ends_every_sounding_note_and_the_pedal() {
    let mut rig = rig(vec![TrackSpec::instrument()], false);
    let seen = recorder(&mut rig);
    rig.sim.run_blocks(1);
    for msg in [[0x90, 60, 100], [0x90, 64, 100], [0x80, 64, 0], [0xB0, 64, 127]] {
        rig.keys.push(LiveEvent { host_ns: 0, msg }).unwrap();
    }
    rig.sim.run_blocks(1);
    seen.lock().unwrap().clear();
    rig.feeds.send(FeedMsg::PanicAll).unwrap();
    rig.sim.run_blocks(1);
    assert_eq!(
        messages(&seen),
        vec![[0x80, 60, 0], [0xB0, 64, 0], [0xB0, 123, 0]]
    );
    assert_eq!(rig.sim.latest_status().unwrap().tracks()[0].held_notes, 0);
    // A second panic has nothing left to end but still says "all notes off" on no channel.
    seen.lock().unwrap().clear();
    rig.feeds.send(FeedMsg::PanicAll).unwrap();
    rig.sim.run_blocks(1);
    assert!(messages(&seen).is_empty());
}

/// Rerouting a track or losing its port must not leave a key hanging: the live notes end.
#[test]
fn a_new_route_or_a_lost_port_ends_the_live_notes() {
    let mut rig = rig(vec![TrackSpec::instrument()], false);
    let seen = recorder(&mut rig);
    rig.sim.run_blocks(1);
    rig.keys
        .push(LiveEvent {
            host_ns: 0,
            msg: [0x92, 70, 90],
        })
        .unwrap();
    rig.sim.run_blocks(1);
    seen.lock().unwrap().clear();
    rig.feeds
        .send(FeedMsg::Route {
            track: 0,
            route: TrackRoute {
                ports: 1,
                channels: 1,
                low: 0,
                high: 127,
            },
        })
        .unwrap();
    rig.sim.run_blocks(1);
    assert_eq!(messages(&seen), vec![[0x82, 70, 0], [0xB2, 123, 0]]);
    // Now only channel 1 reaches the track; channel 3 is somebody else's.
    seen.lock().unwrap().clear();
    for msg in [[0x92, 70, 90], [0x90, 50, 90]] {
        rig.keys.push(LiveEvent { host_ns: 0, msg }).unwrap();
    }
    rig.sim.run_blocks(1);
    assert_eq!(messages(&seen), vec![[0x90, 50, 90]]);
    seen.lock().unwrap().clear();
    rig.feeds.send(FeedMsg::Detach { port: 0 }).unwrap();
    rig.sim.run_blocks(1);
    assert_eq!(messages(&seen), vec![[0x80, 50, 0], [0xB0, 123, 0]]);
    assert_eq!(rig.feeds.drain_retired(), 1, "der Feed kommt zurueck");
}

/// A track that is not an instrument track never hears the instrument stage, and an instrument
/// track without an instrument is silent and records silence - nothing crashes, nothing hangs.
#[test]
fn an_empty_slot_is_silent_and_audio_tracks_are_untouched() {
    let mut rig = rig(vec![TrackSpec::instrument(), TrackSpec::on(0)], true);
    play_heard(&mut rig, 2_000, [0x90, 60, 127]);
    rig.sim.run_to(5_000);
    assert!(nonzero(&rig.sim.out_l).is_empty());
    let status = rig.sim.latest_status().unwrap();
    assert!(!status.tracks()[0].instrument_loaded);
    assert!(!status.tracks()[1].instrument);
    assert_eq!(status.tracks()[1].latency_frames, 827, "Audiospur unveraendert");
}

/// A score that switches monitoring off between takes must not silence the piano.
#[test]
fn monitoring_commands_do_not_silence_an_instrument_track() {
    let mut rig = rig(vec![TrackSpec::instrument()], false);
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    rig.sim.send(Command::SetMonitor {
        track: 0,
        on: false,
    });
    rig.sim.run_blocks(2);
    rig.keys
        .push(LiveEvent {
            host_ns: 0,
            msg: [0x90, 60, 127],
        })
        .unwrap();
    rig.sim.run_blocks(1);
    assert_eq!(nonzero(&rig.sim.out_l).len(), 1);
}

/// Swapping the instrument hands the old one back to the thread that sent it.
#[test]
fn a_replaced_instrument_goes_back_to_its_sender() {
    let mut rig = rig(vec![TrackSpec::instrument()], false);
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    rig.sim.run_blocks(1);
    install(&mut rig, 0, Box::new(Impulse::new(0)), 0);
    rig.sim.run_blocks(1);
    assert_eq!(rig.instruments.drain_retired(), 1);
    rig.instruments
        .send(InstrumentMsg::Remove { track: 0 })
        .unwrap();
    rig.sim.run_blocks(1);
    assert_eq!(rig.instruments.drain_retired(), 1);
}
