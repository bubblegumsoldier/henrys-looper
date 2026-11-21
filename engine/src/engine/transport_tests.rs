//! The transport: a clock that can stand.
//!
//! The app opens with the transport standing ([`EngineCore::hold_transport`]) and the musician
//! starts and stops it. What these tests pin down is what that button promises: standing means
//! bar 1, no click, no loop heard; starting makes the press bar 1 and plays every loop from its top;
//! stopping keeps every layer and leaves no half-made take behind.
//!
//! [`EngineCore::hold_transport`]: super::process::EngineCore::hold_transport

use super::command::Command;
use super::sim::{Sim, SimSpec, mono, silence};
use super::timeline::TimeSignature;
use super::track::TrackState;

const BLOCK: usize = 128;
const R: u64 = 827;

fn fingerprint(k: u64) -> f32 {
    ((k % 65_536) as f32) / 65_536.0 - 0.5 + 0.001
}

fn spec(click: bool) -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 2,
        latency: R,
        roundtrip: R,
        block: BLOCK,
        click,
        // No cable: the input carries only what the musician plays, as on stage.
        loopback_channel: None,
        ..Default::default()
    }
}

fn held(spec: SimSpec, played: super::sim::Played) -> Sim {
    let mut sim = Sim::new(spec, played);
    sim.core.hold_transport();
    sim
}

fn layer(sim: &Sim, track: usize, layer: usize) -> Vec<f32> {
    let t = sim.core.track(track);
    t.layer(layer).expect("Ebene").content(t.loop_len()).to_vec()
}

/// Record one loop of `len` frames at `start` on track 0 and let it play.
fn record_loop(sim: &mut Sim, start: u64, len: u64) {
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: start + len });
    sim.send(Command::StartPlay { track: 0, at: start + len });
    sim.run_to(start + len + R + 4 * BLOCK as u64);
}

#[test]
fn a_held_transport_stands_at_bar_one_and_is_silent() {
    let mut sim = held(spec(true), silence());
    sim.run_to(200_000);
    let status = sim.latest_status().expect("Status");
    assert!(!status.transport);
    assert_eq!((status.bar, status.beat, status.beat_offset), (0, 0, 0));
    assert!(status.pos >= 190_000, "die Sample-Achse laeuft trotzdem weiter");
    assert!(
        sim.out_l.iter().all(|&s| s == 0.0),
        "kein Klick, solange der Transport steht"
    );
}

#[test]
fn start_makes_the_press_bar_one_and_the_click_begins_there() {
    let mut sim = held(spec(true), silence());
    sim.run_to(10_000);
    let at = 12_345u64;
    sim.send(Command::TransportStart { at });
    sim.run_to(at + 30_000);

    assert!(sim.out_l[..at as usize].iter().all(|&s| s == 0.0), "vor dem Start still");
    assert_ne!(sim.out_l[at as usize + 1], 0.0, "Klick der Eins genau am Start");
    // 100 BPM: a beat is 28 800 frames; the second click sits exactly one beat after the start.
    assert_ne!(sim.out_l[(at + 28_800 + 1) as usize], 0.0);
    assert_eq!(sim.out_l[(at + 28_800 - 1) as usize], 0.0);

    let status = sim.latest_status().expect("Status");
    assert!(status.transport);
    assert_eq!(status.bar, 0, "Takt 1");
    assert_eq!(status.beat, 1, "zweiter Schlag");
}

#[test]
fn stop_keeps_the_loops_and_start_plays_them_from_the_top() {
    let mut sim = Sim::new(spec(false), mono(fingerprint));
    let len = 30_000u64;
    record_loop(&mut sim, 5_000, len);
    let content = layer(&sim, 0, 0);
    assert_eq!(content.len() as u64, len);

    // The loop plays ...
    let now = sim.pos();
    sim.run_to(now + 2_000);
    assert!(sim.out_l[now as usize..].iter().any(|&s| s != 0.0));

    // ... until the transport stops: silence after a fade of one fade length, nothing is lost.
    sim.send(Command::TransportStop);
    sim.run_blocks(5);
    let stopped_from = sim.pos();
    sim.run_to(stopped_from + 50_000);
    assert!(
        sim.out_l[stopped_from as usize..].iter().all(|&s| s == 0.0),
        "der Loop schweigt bei stehendem Transport"
    );
    let status = sim.latest_status().expect("Status");
    assert!(!status.transport);
    assert_eq!((status.bar, status.beat), (0, 0));
    assert_eq!(status.tracks[0].layers, 1, "die Ebene bleibt");
    assert!(status.tracks[0].playing, "der Track bleibt auf Play");
    assert_eq!(status.tracks[0].state, TrackState::Playing);

    // Start again in the middle of a block: the loop begins with its first frame exactly there,
    // faded in over one fade length - a transport start acts on its own sample, nothing could
    // fade in before it - and is the loop as it is heard from there on.
    let at = sim.pos() + 1_000 + 37;
    sim.send(Command::TransportStart { at });
    sim.run_to(at + len + 500);
    assert_eq!(sim.out_l[at as usize - 1], 0.0);
    let edge = crate::engine::track::edge_frames(48_000);
    for i in [0u64, 1, 2, edge - 1] {
        let want = content[i as usize] * crate::engine::track::fade_in(i, edge);
        assert_eq!(sim.out_l[(at + i) as usize], want, "Einblendung, Frame {i} des Loops");
    }
    for i in [edge, 999, len - edge - 1] {
        assert_eq!(sim.out_l[(at + i) as usize], content[i as usize], "Frame {i} des Loops");
    }
    let heard = sim.core.track(0).steady_layer(0, len - 1).l;
    assert_eq!(sim.out_l[(at + len - 1) as usize], heard, "letzter Frame mit der Naht-Blende");
    assert_eq!(sim.out_l[(at + len) as usize], content[0], "und wieder von vorn");
    assert_eq!(sim.core.track(0).origin(), at, "der Loop ist auf die neue Eins verankert");
    let status = sim.latest_status().expect("Status");
    assert!(status.transport);
    assert_eq!(status.tracks[0].origin, at);
}

#[test]
fn stop_during_a_first_take_discards_it_and_returns_its_buffer() {
    let mut sim = Sim::new(spec(false), mono(fingerprint));
    sim.send(Command::StartRecord { track: 0, at: 5_000 });
    sim.send(Command::StopRecord { track: 0, at: 5_000 + 40_000 });
    sim.send(Command::StartPlay { track: 0, at: 5_000 + 40_000 });
    sim.run_to(20_000);
    assert_eq!(sim.core.track(0).layer_count(), 1);

    sim.send(Command::TransportStop);
    sim.run_to(80_000);
    let track = sim.core.track(0);
    assert_eq!(track.layer_count(), 0, "eine halbe erste Aufnahme bleibt nicht liegen");
    assert_eq!(track.loop_len(), 0);
    assert!(!track.playing(), "das StartPlay am Take-Ende war vor dem Stopp geschickt");
    assert!(track.take().is_none());
    assert!(sim.reclaimed() >= 1, "der Puffer ist zurueck beim Steuer-Thread");
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.tracks[0].state, TrackState::Empty);
}

#[test]
fn stop_during_an_overdub_keeps_what_was_played() {
    let mut sim = Sim::new(spec(false), mono(fingerprint));
    let len = 30_000u64;
    record_loop(&mut sim, 5_000, len);
    let origin = sim.core.track(0).origin();
    let start = origin + 2 * len;
    assert!(start > sim.pos());
    sim.send(Command::StartOverdub { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: start + len });
    sim.run_to(start + 10_000);

    sim.send(Command::TransportStop);
    let stop_at = sim.pos();
    sim.run_to(stop_at + R + 4 * BLOCK as u64);
    let track = sim.core.track(0);
    assert_eq!(track.layer_count(), 2, "die angefangene Ebene bleibt");
    assert!(track.take().is_none(), "und ist sauber abgeschlossen");
    let overdub = track.layer(1).expect("Ebene 2").content(len).to_vec();
    // What was played before the stop is there, at its place in the loop ...
    let played = (start - origin) % len;
    assert_eq!(overdub[played as usize], fingerprint(start + R));
    assert_eq!(overdub[(played + 5_000) as usize], fingerprint(start + R + 5_000));
    // ... and nothing after the stop was written.
    let after = (stop_at + 100 - origin) % len;
    assert_eq!(overdub[after as usize], 0.0);
}

#[test]
fn stop_drops_what_was_scheduled_before_it() {
    let mut sim = Sim::new(spec(false), mono(fingerprint));
    record_loop(&mut sim, 5_000, 30_000);
    sim.send(Command::StopPlay { track: 0, at: 0 });
    sim.run_blocks(1);
    let later = sim.pos() + 40_000;
    sim.send(Command::StartPlay { track: 0, at: later });
    sim.send(Command::TransportStop);
    sim.run_to(later + 1_000);
    assert!(!sim.core.track(0).playing(), "ein vor dem Stopp geplantes Play verfaellt");
}

/// Record pressed while the transport stands: the host sends the start and the take for the same
/// position, in that order. The take begins on the new bar 1, not a loop later.
#[test]
fn a_take_queued_behind_the_start_begins_on_bar_one() {
    let mut sim = held(spec(false), mono(fingerprint));
    sim.run_to(50_000);
    let at = 60_001u64;
    let len = 20_000u64;
    sim.send(Command::TransportStart { at });
    sim.send(Command::StartRecord { track: 0, at });
    sim.send(Command::StopRecord { track: 0, at: at + len });
    sim.send(Command::StartPlay { track: 0, at: at + len });
    sim.run_to(at + len + R + 4 * BLOCK as u64);

    let track = sim.core.track(0);
    assert_eq!(track.origin(), at);
    assert_eq!(track.loop_len(), len);
    let content = layer(&sim, 0, 0);
    assert_eq!(content[0], fingerprint(at + R), "erstes Sample ist das der Eins");
    assert_eq!(content[(len - 1) as usize], fingerprint(at + R + len - 1));
    // And it plays right after, from its top.
    assert_eq!(sim.out_l[(at + len) as usize], content[0]);
}

/// The CLI, the score subcommand and every older test never hold the transport: it runs from the
/// first sample, exactly as before.
#[test]
fn without_hold_the_transport_runs_from_the_start() {
    let mut sim = Sim::new(spec(true), silence());
    assert!(sim.core.transport());
    sim.run_to(30_000);
    assert_ne!(sim.out_l[1], 0.0, "Klick auf der allerersten Eins");
    let status = sim.latest_status().expect("Status");
    assert!(status.transport);
    assert_eq!(status.beat, 1);
}

// ---- the count-in of free looping ------------------------------------------------------------

const BAR: u64 = 4 * 28_800;

/// One bar of count-in, then the take: the click is heard during the count-in although the click
/// is off, no loop sounds, and the first recorded sample is the input of exactly bar 1.
#[test]
fn after_a_count_in_the_take_starts_sample_exact_on_bar_one() {
    let mut sim = held(spec(false), mono(fingerprint));
    sim.run_to(10_000);
    let at = 12_345u64;
    let end = at + BAR;
    sim.send(Command::CountIn { at, end });
    sim.send(Command::TransportStart { at: end });
    sim.send(Command::StartRecord { track: 0, at: end });
    sim.send(Command::StopRecord { track: 0, at: end + 2 * BAR });
    sim.send(Command::StartPlay { track: 0, at: end + 2 * BAR });
    sim.run_to(end + 2 * BAR + R + 4 * BLOCK as u64);

    assert!(sim.out_l[..at as usize].iter().all(|&s| s == 0.0));
    assert_ne!(sim.out_l[at as usize + 1], 0.0, "Einzaehler-Klick auf der Eins");
    assert_ne!(sim.out_l[(at + 28_800) as usize + 1], 0.0, "und auf Schlag 2");
    assert!(
        sim.out_l[end as usize + 1..(end + BAR) as usize].iter().all(|&s| s == 0.0),
        "nach dem Einzaehler schweigt der ausgeschaltete Klick wieder"
    );
    let t = sim.core.track(0);
    assert_eq!(t.origin(), end, "Takt 1 ist das Ende des Einzaehlers");
    assert_eq!(t.loop_len(), 2 * BAR);
    let content = layer(&sim, 0, 0);
    assert_eq!(content[0], fingerprint(end + R), "erstes Sample genau auf Takt 1");
    assert_eq!(content[1_000], fingerprint(end + R + 1_000));
    let status = sim.latest_status().expect("Status");
    assert!(status.transport);
}

/// Loops that exist stay silent during the count-in and start from their top on bar 1.
#[test]
fn existing_loops_wait_for_bar_one() {
    // Four outputs, so the click (monitor bus) and the loop (main bus) are apart.
    let mut sim = held(
        SimSpec {
            output_channels: 4,
            ..spec(false)
        },
        mono(fingerprint),
    );
    sim.send(Command::TransportStart { at: 1_000 });
    record_loop(&mut sim, 1_000, 10_000);
    sim.send(Command::TransportStop);
    sim.run_blocks(4);
    let at = sim.pos() + 2_000;
    let end = at + BAR;
    sim.send(Command::CountIn { at, end });
    sim.send(Command::TransportStart { at: end });
    sim.run_to(end + 20_000);
    // Monitor bus is the only one with the click; the main bus carries only the loop.
    let main = &sim.buses[super::bus::Bus::Main.index()][0];
    assert!(main[at as usize..end as usize].iter().all(|&s| s == 0.0), "kein Loop im Einzaehler");
    assert!(main[end as usize..end as usize + 10_000].iter().any(|&s| s != 0.0), "ab Takt 1 klingt er");
}

/// Cancelled during the count-in: nothing recorded, the clock stands again, nothing fires later.
#[test]
fn a_cancelled_count_in_leaves_nothing_behind() {
    let mut sim = held(spec(false), mono(fingerprint));
    let at = 5_000u64;
    let end = at + BAR;
    sim.send(Command::CountIn { at, end });
    sim.send(Command::TransportStart { at: end });
    sim.send(Command::StartRecord { track: 0, at: end });
    sim.send(Command::StopRecord { track: 0, at: end + BAR });
    sim.run_to(at + BAR / 2);
    sim.send(Command::TransportStop);
    sim.run_to(end + 2 * BAR);
    let status = sim.latest_status().expect("Status");
    assert!(!status.transport, "die Uhr steht");
    assert_eq!(sim.core.track(0).layer_count(), 0, "nichts aufgenommen");
    assert_eq!(status.tracks[0].state, TrackState::Empty);
    assert!(sim.out_l[(at + BAR / 2 + 2 * BLOCK as u64) as usize..].iter().all(|&s| s == 0.0));
}
