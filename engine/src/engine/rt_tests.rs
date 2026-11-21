//! The audio callback is real-time safe through the owner's whole score: not one allocation and
//! not one free inside `EngineCore::process` and `place_output`, from the count-in through every
//! section to "Stop all" and the clear of a reload.
//!
//! The score is the one the owner played when the dropouts at section starts were logged
//! (`tests/data/hardest-part-live.yaml`, the sheet left out): audio takes that end on the sample
//! the next begins (`intro` → `intro_b`), a ten-bar take with `skip_end` (`chorus_1`'s mic), a
//! `RestartLoop` of two loops together with a MIDI take on an instrument track (`verse_2`),
//! `monitor_fade`, `monitor: always`, play+monitor, and the performance recording writing every
//! stem, dry stem and the mix on its own thread the whole time. Everything that the app attaches to
//! the engine is attached here too, with test doubles where the app has third-party code: plugin
//! inserts in front of the octaver (as the owner's IRDX) with 8 frames of latency, an instrument
//! that plays a decaying tone on every note, the MIDI-loop stage with the score's 1/8 grid, MIDI
//! pads played during the drum take.
//!
//! The global allocator of the test binary counts (`crate::rt::CountingAlloc`); `Sim::step` arms it
//! around exactly the part the app's output callback runs. On a failure the first offending block
//! is named with its profile (phases, applied command kinds) and the backtraces of the first
//! allocations.

use rtrb::Producer;

use super::command::Command;
use super::frame::Channels;
use super::fx::FxSlot;
use super::insert::testing::Delay;
use super::insert::{Insert, InsertMsg, InsertPosition, InsertSender, InsertShared, InsertTarget, insert_channel};
use super::midiloop::NoteGrid;
use super::process::loop_capacity;
use super::runner::{CLOSED_CLICK_GATE, RELOAD_CLEAR, Runner, setup_commands};
use super::sim::{Sim, SimSpec, TrackSpec};
use super::source::{
    BlockEvent, FeedMsg, FeedSender, Instrument, InstrumentMsg, InstrumentSender, LiveEvent,
    SourceProcessor, SourceShared, StereoOut, TrackRoute, port_feed, source_channel,
};
use super::timeline::Timeline;
use crate::capture::capture_channel;
use crate::capture::run::{Mode, Recording, RunInfo, RunTrack};
use crate::rt;
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
/// The owner's Scarlett 2i2 at 128 frames.
const LATENCY: u64 = 596;
/// Output latency of the simulated device, for the MIDI clock.
const OUT: u64 = 156;
const SCORE: &str = include_str!("../../tests/data/hardest-part-live.yaml");

/// A drum stand-in: a decaying tone per note, fixed state, nothing allocated.
struct Tone {
    phase: f32,
    step: f32,
    level: f32,
}

impl SourceProcessor for Tone {
    fn render(&mut self, events: &[BlockEvent], outs: &mut [StereoOut<'_>]) {
        let out = &mut outs[0];
        let n = out.l.len();
        let mut next = 0;
        for i in 0..n {
            while next < events.len() && events[next].offset as usize <= i {
                let msg = events[next].msg;
                if msg[0] & 0xF0 == 0x90 && msg[2] > 0 {
                    self.level = f32::from(msg[2]) / 127.0 * 0.3;
                    self.step = 0.02 + f32::from(msg[1] % 12) * 0.003;
                    self.phase = 0.0;
                }
                next += 1;
            }
            let v = self.level * self.phase.sin();
            self.phase += self.step;
            self.level *= 0.9995;
            out.l[i] = v;
            out.r[i] = v;
        }
    }
}

/// What the guitar and the voice play: low and busy, no two passes alike.
fn played(k: u64, ch: usize) -> f32 {
    let t = k as f32 / RATE as f32;
    let f = if ch == 0 { 220.0 } else { 41.0 };
    0.2 * (std::f32::consts::TAU * f * t).sin() + 0.05 * (t * 0.37).sin()
}

struct Rig {
    sim: Sim,
    runner: Runner,
    first_bar: u64,
    offsets: Vec<u64>,
    _instruments: InstrumentSender,
    _feeds: FeedSender,
    _inserts: InsertSender,
    keys: Producer<LiveEvent>,
    /// Pad hits still to play, by heard output position.
    pads: Vec<(u64, [u8; 3])>,
    port: crate::capture::CapturePort,
    recording: Option<Recording>,
    folder: std::path::PathBuf,
}

impl Rig {
    /// The owner's setup; `plugins: false` leaves the inserts out, so the frame path (no plugin
    /// anywhere) is covered as well as the block path.
    fn new(plugins: bool) -> Self {
        Self::build(plugins, true)
    }

    /// `record: false` runs without the performance recording (and its writer thread).
    fn build(plugins: bool, record: bool) -> Self {
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
                        ..TrackSpec::on(0)
                    }
                }
            })
            .collect();
        let drums = score.tracks.iter().position(|t| t.is_instrument()).expect("Drums");
        let names: Vec<String> = score.tracks.iter().map(|t| t.name.clone()).collect();
        let channels: Vec<Channels> = tracks.iter().map(|t| t.channels()).collect();
        let mut sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                latency: LATENCY,
                roundtrip: LATENCY,
                block: BLOCK,
                tracks,
                input_channels: 2,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 8,
                ..Default::default()
            },
            Box::new(played),
        );

        // Plugin inserts in front of the octaver on the three guitar tracks the owner has IRDX on.
        let (mut inserts, endpoint) = insert_channel(32);
        sim.core.attach_inserts(endpoint);
        for name in ["theme", "theme_b", "chorus_guitar"].into_iter().filter(|_| plugins) {
            let t = names.iter().position(|n| n == name).unwrap();
            inserts
                .send(InsertMsg::Install {
                    target: InsertTarget::Track(t),
                    slot: 0,
                    insert: Insert::new(Box::new(Delay::new(8)), InsertShared::new(8))
                        .at(InsertPosition::before(FxSlot::all()[0])),
                })
                .unwrap();
        }

        // The drum instrument, its MIDI port and the MIDI-loop stage with the score's grid.
        let (mut instruments, mut feeds, sources) = source_channel(16);
        sim.core.attach_sources(sources, BLOCK * 16);
        sim.with_clock(OUT);
        sim.with_clips();
        if let Some(clips) = sim.clips.as_mut() {
            clips.set_grid(drums, NoteGrid::Eighth);
        }
        let (keys, handle) = port_feed();
        feeds
            .send(FeedMsg::Attach(handle.take_feed(0).unwrap()))
            .unwrap();
        feeds
            .send(FeedMsg::Route {
                track: drums,
                route: TrackRoute {
                    ports: 1,
                    channels: TrackRoute::ALL_CHANNELS,
                    low: 0,
                    high: 127,
                },
            })
            .unwrap();
        instruments
            .send(InstrumentMsg::Install {
                track: drums,
                instrument: Instrument::new(
                    Box::new(Tone {
                        phase: 0.0,
                        step: 0.0,
                        level: 0.0,
                    }),
                    SourceShared::new(0),
                ),
            })
            .unwrap();
        sim.send(Command::SetLoopKind {
            track: drums,
            midi: true,
        });

        // The recorder's slot.
        let (port, capture) = capture_channel();
        sim.core.attach_capture(capture);

        for cmd in setup_commands(&score) {
            sim.send(cmd);
        }
        if let Some(mix) = &score.mixer {
            for cmd in crate::score::mixer::mixer_commands(mix, &names, &[]) {
                sim.send(cmd);
            }
        }
        sim.send(CLOSED_CLICK_GATE);
        sim.run_blocks(8);

        let mut runner = Runner::new(score.clone(), timeline, BLOCK as u32).expect("Runner");
        runner.start(sim.pos());
        let first_bar = runner.armed_at().expect("armiert").1;
        let folder = std::env::temp_dir().join(format!(
            "henrys-looper-rt-{}-{}",
            std::process::id(),
            if plugins { "plugins" } else { "plain" }
        ));
        let mut rig = Self {
            sim,
            runner,
            first_bar,
            offsets,
            _instruments: instruments,
            _feeds: feeds,
            _inserts: inserts,
            keys,
            pads: Vec::new(),
            port,
            recording: None,
            folder,
        };

        // The performance recording from bar 1 of the first section, as the app starts it.
        let start = rig.section(0);
        let info = RunInfo {
            title: "rt".into(),
            mode: Mode::Score,
            folder: rig.folder.clone(),
            started_at: "2026-11-21 17:25:34".into(),
            sample_rate: RATE,
            bpm: score.bpm,
            beats_per_bar: score.signature().beats_per_bar,
            beat_unit: score.signature().beat_unit,
            count_in_bars: 2,
            start,
            start_bar: rig.first_bar,
            tracks: names
                .iter()
                .zip(channels.iter())
                .map(|(name, c)| RunTrack {
                    name: name.clone(),
                    channels: *c,
                    latency_frames: LATENCY,
                })
                .collect(),
            dry: true,
            max_block: BLOCK * 16,
            score_yaml: None,
            score_path: None,
            min_seconds: 0.0,
            queue_seconds: crate::capture::QUEUE_SECONDS,
        };
        if record {
            rig.recording = Some(Recording::start(info, &mut rig.port).expect("Aufnahme"));
        }

        // Pads on every eighth through the drum take of `verse_2` (kick, snare, hats), and a few
        // during `chorus_2` when the drum loop plays and the pads are only heard.
        let verse_2 = rig.index_of("verse_2");
        let chorus_2 = rig.index_of("chorus_2");
        let eighth = rig.bar_len() / 8;
        for (section, bars) in [(verse_2, 4u64), (chorus_2, 1)] {
            let from = rig.section(section);
            // A first hit a little before the downbeat, as a drummer anticipates.
            let mut at = from.saturating_sub(eighth / 3);
            let end = from + bars * rig.bar_len();
            let mut n = 0u8;
            while at < end {
                let note = [36, 42, 38, 42][usize::from(n % 4)];
                rig.pads.push((at, [0x99, note, 100]));
                rig.pads.push((at + eighth / 2, [0x89, note, 0]));
                at += eighth;
                n = n.wrapping_add(1);
            }
        }
        rig.pads.sort_by_key(|p| p.0);
        rig.pump();
        rig
    }

    fn index_of(&self, id: &str) -> usize {
        self.runner
            .score()
            .sections
            .iter()
            .position(|s| s.id == id)
            .unwrap_or_else(|| panic!("section {id}"))
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

    fn bar_len(&self) -> u64 {
        self.bar(1) - self.bar(0)
    }

    fn section(&self, index: usize) -> u64 {
        self.bar(self.offsets[index])
    }

    fn pump(&mut self) {
        for cmd in self.runner.take_commands() {
            self.sim.send(cmd);
        }
    }

    /// One turn of the control thread, then one callback.
    fn step(&mut self) {
        self.runner.tick(self.sim.pos());
        self.pump();
        // A pad whose moment has been heard arrives now, as the driver's MIDI callback would.
        let pos = self.sim.pos();
        while let Some(&(at, msg)) = self.pads.first() {
            if at + OUT > pos {
                break;
            }
            let host_ns = self.sim.heard_ns(at);
            let _ = self.keys.push(LiveEvent { host_ns, msg });
            self.pads.remove(0);
        }
        self.port.collect();
        self.sim.step();
    }

    fn run_to(&mut self, pos: u64) {
        while self.sim.pos() < pos {
            self.step();
        }
    }
}

fn verdict(rig: &Rig, what: &str) -> String {
    let first = rig.sim.rt_first.map(|(pos, counts, profile)| {
        format!(
            "first at engine frame {pos} ({:.3} s): {counts:?}, commands {:?}, phases {:?}",
            pos as f64 / f64::from(RATE),
            profile.command_names(),
            profile.slowest_phases(8),
        )
    });
    format!(
        "{what}: {:?} inside the callback; {}\n{}",
        rig.sim.rt,
        first.unwrap_or_default(),
        rt::trace::kept().join("\n---\n")
    )
}

/// Every command kind a score run has to apply for the run to prove anything about it.
const MUST_APPLY: [&str; 13] = [
    "StartRecord",
    "StopRecord",
    "StartPlay",
    "StopPlay",
    "RestartLoop",
    "CutTake",
    "MonitorFade",
    "SetMonitor",
    "DropWaiting",
    "ClearAll",
    "ClickGate",
    "TransportStart",
    "SetLoopKind",
];

/// The whole score - every section, the drum take, Stop all, the recording's end and a reload's
/// clear - without a single allocation or free inside the callback.
fn run_the_owners_score(plugins: bool) {
    rt::trace::keep(true);
    let mut rig = Rig::new(plugins);
    let drums = rig.runner.score().tracks.iter().position(|t| t.is_instrument()).unwrap();
    let sections = rig.runner.score().sections.len();
    let mut drum_notes = 0;
    for index in 0..sections - 1 {
        rig.run_to(rig.section(index + 1));
        if let Some(r) = rig.recording.as_ref() {
            r.note_take();
        }
        let id = rig.runner.score().sections[index].id.clone();
        assert!(rig.sim.rt.is_zero(), "{}", verdict(&rig, &format!("through section {id}")));
        if id == "verse_2" {
            drum_notes = rig
                .sim
                .core
                .track(drums)
                .layer(0)
                .and_then(|l| l.clip())
                .map_or(0, |c| c.raw().len());
        }
    }
    // Two bars into `end`, which waits for Release; then Stop all.
    rig.run_to(rig.sim.pos() + 2 * rig.bar_len());
    let pos = rig.sim.pos();
    rig.runner.stop_all(pos);
    rig.pump();
    rig.run_to(pos + 2 * rig.bar_len());
    // The recording ends (with its tail) and is handed back.
    let pos = rig.sim.pos();
    if let Some(r) = rig.recording.as_ref() {
        r.stop(pos);
    }
    rig.run_to(pos + 8 * rig.bar_len());
    // Reload: everything cleared at once - every layer buffer goes back.
    rig.sim.send(RELOAD_CLEAR);
    rig.run_to(rig.sim.pos() + 2 * rig.bar_len());
    assert!(rig.sim.rt.is_zero(), "{}", verdict(&rig, "Stop all, recording end, reload"));
    rt::trace::keep(false);

    // The run really did what it is meant to prove something about.
    let applied = crate::load::BlockProfile {
        command_mask: rig.sim.applied,
        ..Default::default()
    }
    .command_names();
    for kind in MUST_APPLY {
        assert!(applied.contains(&kind), "{kind} never applied: {applied:?}");
    }
    assert!(rig.pads.is_empty(), "every pad was played");
    assert!(drum_notes >= 32, "the drum take holds the pads: {drum_notes} events");
    let mut recording = rig.recording.take().unwrap();
    let outcome = recording.wait().cloned().expect("the writer ended");
    assert!(outcome.kept, "{outcome:?}");
    assert!(outcome.seconds > 150.0, "{outcome:?}");
    // The engine runs here many times faster than real time; a writer that falls behind is only
    // the test's speed, and its gap path is real-time safe as well (it is part of the run above).
    let _ = std::fs::remove_dir_all(&outcome.folder);
}

#[test]
fn the_owners_score_with_plugins_runs_without_an_allocation_in_the_callback() {
    run_the_owners_score(true);
}

#[test]
fn the_owners_score_without_plugins_runs_without_an_allocation_in_the_callback() {
    run_the_owners_score(false);
}

/// Diagnostic, not a gate: which callbacks of the owner's score page-fault, and in what. The
/// process-wide fault counter also sees the writer thread, so run it alone:
/// `cargo test -p looper-engine --lib page_faults_of_the_owners_score -- --ignored --nocapture`.
#[test]
#[ignore]
fn page_faults_of_the_owners_score() {
    // `RT_PROBE_NO_RECORDING=1`: without the writer thread, whose faults the process-wide
    // counter would otherwise mix in.
    let record = std::env::var_os("RT_PROBE_NO_RECORDING").is_none();
    // `RT_PROBE_TWICE=1`: run the score once unprobed first. What faults only the first time is
    // code (or memory) touched for the first time in the process, not memory the run allocates.
    if std::env::var_os("RT_PROBE_TWICE").is_some() {
        let mut warm = Rig::build(true, false);
        let end = warm.section(warm.runner.score().sections.len() - 1);
        warm.run_to(end);
    }
    let mut rig = Rig::build(true, record);
    if std::env::var_os("RT_PROBE_STACK").is_some() {
        rt::prefault_stack();
    }
    rig.sim.fault_probe = true;
    let sections = rig.runner.score().sections.len();
    let mut bounds = Vec::new();
    for index in 0..sections - 1 {
        bounds.push((rig.section(index), rig.runner.score().sections[index].id.clone()));
        rig.run_to(rig.section(index + 1));
    }
    let pos = rig.sim.pos();
    rig.runner.stop_all(pos);
    rig.pump();
    rig.run_to(pos + 2 * rig.bar_len());
    if let Some(r) = rig.recording.as_ref() {
        r.stop(rig.sim.pos());
    }
    rig.run_to(rig.sim.pos() + 8 * rig.bar_len());
    rig.sim.send(RELOAD_CLEAR);
    rig.run_to(rig.sim.pos() + 2 * rig.bar_len());
    let total: u64 = rig.sim.faults.iter().map(|f| f.1).sum();
    eprintln!("{} callbacks faulted, {total} faults in all", rig.sim.faults.len());
    for (pos, n, profile) in rig.sim.faults.iter().filter(|f| f.0 >= rig.section(0)).take(100_000) {
        let section = bounds
            .iter()
            .rev()
            .find(|(at, _)| at <= pos)
            .map_or_else(|| "count-in".to_string(), |(at, id)| format!("{id} +{}", pos - at));
        eprintln!(
            "{pos:>9} {section:<24} {n:>4} faults  commands {:?}",
            profile.command_names()
        );
    }
    if let Some(mut r) = rig.recording.take() {
        if let Some(o) = r.wait() {
            let _ = std::fs::remove_dir_all(&o.folder);
        }
    }
}

/// The counter is not blind: a plugin that allocates in its process call is caught, with the
/// block it happened in.
#[test]
fn an_allocating_plugin_is_caught() {
    struct Greedy;
    impl super::insert::InsertProcessor for Greedy {
        fn process(&mut self, left: &mut [f32], _right: &mut [f32]) {
            let copy = left.to_vec();
            std::hint::black_box(copy);
        }
    }
    let mut sim = Sim::new(
        SimSpec {
            loopback_channel: None,
            ..Default::default()
        },
        Box::new(|_, _| 0.0),
    );
    let (mut tx, endpoint) = insert_channel(4);
    sim.core.attach_inserts(endpoint);
    sim.run_blocks(4);
    assert!(sim.rt.is_zero(), "{:?}", sim.rt);
    tx.send(InsertMsg::Install {
        target: InsertTarget::Track(0),
        slot: 0,
        insert: Insert::new(Box::new(Greedy), InsertShared::new(0)),
    })
    .unwrap();
    sim.run_blocks(4);
    assert!(sim.rt.allocs >= 4 && sim.rt.frees >= 4, "{:?}", sim.rt);
    let (pos, _, profile) = sim.rt_first.expect("the first one is named");
    assert_eq!(pos, 4 * BLOCK as u64);
    assert!(profile.plugin_ns > 0 || profile.phase_ns.iter().any(|&ns| ns > 0));
}

/// No lock on the audio path. Locks cannot be counted the way allocations are, so this reads the
/// source of every module the callback runs through: a `Mutex`, `RwLock`, `Condvar` or `.lock()`
/// may only appear where the control thread uses it. The one there is - the home of a MIDI port's
/// queue end (`source::FeedHome`) - is taken in `take_feed` and in `PortFeed::drop`; a `PortFeed`
/// is never dropped on the audio thread (it goes back through a queue), and if it were, the free
/// of its allocation would fail the allocation tests above.
#[test]
fn no_lock_on_the_audio_path() {
    const FILES: [(&str, &str); 25] = [
        ("process.rs", include_str!("process.rs")),
        ("midiproc.rs", include_str!("midiproc.rs")),
        ("track.rs", include_str!("track.rs")),
        ("source.rs", include_str!("source.rs")),
        ("insert.rs", include_str!("insert.rs")),
        ("midiloop.rs", include_str!("midiloop.rs")),
        ("latch.rs", include_str!("latch.rs")),
        ("bus.rs", include_str!("bus.rs")),
        ("metro.rs", include_str!("metro.rs")),
        ("frame.rs", include_str!("frame.rs")),
        ("grid.rs", include_str!("grid.rs")),
        ("timeline.rs", include_str!("timeline.rs")),
        ("command.rs", include_str!("command.rs")),
        ("fx/mod.rs", include_str!("fx/mod.rs")),
        ("fx/biquad.rs", include_str!("fx/biquad.rs")),
        ("fx/delay.rs", include_str!("fx/delay.rs")),
        ("fx/dynamics.rs", include_str!("fx/dynamics.rs")),
        ("fx/octaver.rs", include_str!("fx/octaver.rs")),
        ("fx/reverb.rs", include_str!("fx/reverb.rs")),
        ("fx/smooth.rs", include_str!("fx/smooth.rs")),
        ("capture/mod.rs", include_str!("../capture/mod.rs")),
        ("load.rs", include_str!("../load.rs")),
        ("rt.rs", include_str!("../rt.rs")),
        ("click.rs", include_str!("../click.rs")),
        ("meter.rs", include_str!("../meter.rs")),
    ];
    const ALLOWED: [&str; 4] = [
        "use std::sync::{Arc, Mutex};",
        "rx: Mutex<Option<Consumer<LiveEvent>>>,",
        "self.home.rx.lock()",
        "rx: Mutex::new(Some(rx)),",
    ];
    let mut found = Vec::new();
    for (name, text) in FILES {
        // Test modules and test doubles may lock.
        let body = text
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(text)
            .split("pub mod testing")
            .next()
            .unwrap_or(text);
        for (n, line) in body.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let hit = ["Mutex", "RwLock", "Condvar", ".lock()", "parking_lot"]
                .iter()
                .any(|t| code.contains(t));
            if hit && !ALLOWED.iter().any(|a| code.contains(a)) {
                found.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "a lock on the audio path:\n{}", found.join("\n"));
}
