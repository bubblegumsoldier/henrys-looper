//! "Stop all" in the owner's score, pressed at every kind of moment.
//!
//! The report: "Sometimes - not always - when I click Stop all, the app acts as if everything were
//! stopped, but everything keeps playing." His score ("The Hardest Part", `tests/data/hardest-part-94.yaml`,
//! the sheet left out) runs every section with `autorelease: true`, so the *following* section is
//! always armed - its commands are in the engine from the first sample of a section on - and it
//! uses everything the runner can send: takes shorter than their section, `restart: true`,
//! re-anchored loops that come back, a MIDI loop on an instrument track, and the Octaver (chain
//! latency, so loops are read ahead).
//!
//! These tests drive the real engine core through `Sim` with that score, press "Stop all" at a
//! chosen sample and then hold the result to one promise:
//!
//! * from the sample the stop takes effect (`Runner::stop_all`'s head start, `Scheduler::soon`) on,
//!   both buses are **digitally silent** - loops, monitored inputs and the click alike - and stay
//!   so through the armed section and beyond, where every left-over command would land;
//! * with the score's mixer on (effects, Octaver read-ahead) the only thing allowed after that
//!   sample is the **tail** of the effect chains (reverb, delay, the Octaver's own latency): it
//!   decays and never grows again - nothing starts sounding later;
//! * no MIDI note is played by the loop after it, and none is left hanging on the instrument;
//! * the runner says "stopped", shows nothing armed and no section, and the number of tracks its
//!   sentence names is the number the engine really had sounding or recording;
//! * the engine's status agrees: no track playing, none recording.

use std::sync::{Arc, Mutex};

use rtrb::Producer;

use super::*;
use crate::engine::bus::Bus;
use crate::engine::command::Command;
use crate::engine::process::loop_capacity;
use crate::engine::sim::{Played, Sim, SimSpec, TrackSpec};
use crate::engine::source::testing::Recorder;
use crate::engine::source::{
    FeedMsg, FeedSender, Instrument, InstrumentMsg, InstrumentSender, LiveEvent, SourceShared,
    TrackRoute, port_feed, source_channel,
};
use crate::engine::track::TrackState;
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: u32 = 128;
/// Output latency of the simulated device (the host clock the MIDI input is placed with).
const OUT: u64 = 300;
/// Latency the simulated drum instrument reports, so its loop is read ahead as well.
const DRUM_LATENCY: u32 = 100;

const SCORE: &str = include_str!("../../../tests/data/hardest-part-94.yaml");

/// Section indices of the owner's score, in order.
const SECTIONS: [&str; 13] = [
    "intro",
    "intro_b",
    "verse_1",
    "verse_1_bass",
    "verse_1_b",
    "chorus_1",
    "verse_2",
    "verse_2_b",
    "chorus_2",
    "bridge",
    "chorus_3",
    "chorus_3_all",
    "end",
];
const CHORUS_2: usize = 8;
const BRIDGE: usize = 9;
const VERSE_2: usize = 6;

type Seen = Arc<Mutex<Vec<(u64, [u8; 3])>>>;

/// A distinct, practically never silent signal per input channel.
fn signal(k: u64, ch: usize) -> f32 {
    let k = k + ch as u64 * 7_919;
    ((k % 30_011) as f32) / 30_011.0 - 0.5
}

/// The owner's score on the real engine core: eight tracks, the mic on input 1, the guitar on
/// input 2, the drums on a MIDI-loop instrument that records what it is sent.
struct Stage {
    sim: Sim,
    runner: Runner,
    keys: Producer<LiveEvent>,
    seen: Seen,
    _instruments: InstrumentSender,
    _feeds: FeedSender,
    first_bar: u64,
    /// Bar offset of every section's start from the first one, plus the end.
    offsets: Vec<u64>,
    drums: usize,
    /// Pad hits the drummer plays, as (heard position, message), in order.
    hits: Vec<(u64, [u8; 3])>,
    next_hit: usize,
    stopped: bool,
}

impl Stage {
    fn new(mixer: bool) -> Self {
        let score = compile_score(SCORE).expect("die Partitur uebersetzt");
        assert_eq!(
            score.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            SECTIONS
        );
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
        let drums = score
            .tracks
            .iter()
            .position(|t| t.is_instrument())
            .expect("Drums");
        let played: Played = Box::new(signal);
        let mut sim = Sim::new(
            SimSpec {
                bpm: score.bpm,
                signature: score.signature(),
                bars: max_bars,
                block: BLOCK as usize,
                click: true,
                tracks,
                input_channels: 2,
                loopback_channel: None,
                layer_frames: Some(loop_capacity(&timeline, max_bars)),
                spare_slots: 8,
                ..Default::default()
            },
            played,
        );
        let (mut instruments, mut feeds, endpoint) = source_channel(16);
        sim.core.attach_sources(endpoint, BLOCK as usize * 4);
        sim.with_clock(OUT);
        sim.with_clips();
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
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        instruments
            .send(InstrumentMsg::Install {
                track: drums,
                instrument: Instrument::new(
                    Box::new(Recorder {
                        seen: Arc::clone(&seen),
                        blocks: 0,
                        frames: 0,
                    }),
                    SourceShared::new(DRUM_LATENCY),
                ),
            })
            .unwrap();
        // What the host does on load: the score's setup, the loop kind, the mixer, the closed
        // click window.
        for cmd in setup_commands(&score) {
            sim.send(cmd);
        }
        sim.send(Command::SetLoopKind {
            track: drums,
            midi: true,
        });
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
            keys,
            seen,
            _instruments: instruments,
            _feeds: feeds,
            first_bar,
            offsets,
            drums,
            hits: Vec::new(),
            next_hit: 0,
            stopped: false,
        };
        // The drummer plays the pads through the four recorded bars of verse 2: a short hit a
        // little after every beat.
        let beat = stage.runner.scheduler().timeline.samples_per_beat() as u64;
        let from = stage.section(VERSE_2);
        for n in 0..16u64 {
            let at = from + n * beat + 500;
            stage.hits.push((at, [0x99, 36, 100]));
            stage.hits.push((at + 3_000, [0x89, 36, 0]));
        }
        stage.pump();
        stage
    }

    fn bar(&self, bars: u64) -> u64 {
        self.runner.scheduler().timeline.bar_start(self.first_bar + bars)
    }

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
            while let Some(&(heard, msg)) = self.hits.get(self.next_hit) {
                if heard + OUT > self.sim.pos() {
                    break;
                }
                self.next_hit += 1;
                // After the stop the drummer only lets go of what he holds.
                if self.stopped && msg[0] & 0xF0 == 0x90 {
                    continue;
                }
                let host_ns = self.sim.heard_ns(heard);
                self.keys.push(LiveEvent { host_ns, msg }).unwrap();
            }
            self.runner.tick(self.sim.pos());
            self.pump();
            self.sim.step();
        }
    }

    /// What the engine has sounding or recording right now, by its own status.
    fn busy_tracks(&mut self) -> Vec<usize> {
        let status = self.sim.latest_status().expect("Status");
        status
            .tracks()
            .iter()
            .take(self.runner.score().tracks.len())
            .enumerate()
            .filter(|(_, t)| {
                t.playing || matches!(t.state, TrackState::Recording | TrackState::Overdub)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Press "Stop all" now. Returns the sample it takes effect at, and the sentence.
    fn stop_all(&mut self) -> (u64, String) {
        let est = self.sim.pos();
        let soon = self.runner.scheduler().soon(est);
        let message = self.runner.stop_all(est);
        self.stopped = true;
        self.pump();
        (soon, message)
    }

    /// Largest absolute sample on either bus, either side, in `from..to`.
    fn peak(&self, from: u64, to: u64) -> f32 {
        let mut peak = 0.0f32;
        for bus in Bus::ALL {
            for side in &self.sim.buses[bus.index()] {
                for v in &side[from as usize..to as usize] {
                    peak = peak.max(v.abs());
                }
            }
        }
        peak
    }

    /// First output position in `from..to` where any bus is not exactly zero.
    fn first_sound(&self, from: u64, to: u64) -> Option<(u64, f32)> {
        (from..to).find_map(|p| {
            Bus::ALL.iter().find_map(|bus| {
                let f = self.sim.bus(*bus, p);
                let v = f.l.abs().max(f.r.abs());
                (v != 0.0).then_some((p, v))
            })
        })
    }

    /// Note ons the instrument got in `from..to`.
    fn note_ons(&self, from: u64, to: u64) -> Vec<(u64, [u8; 3])> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, m)| (from..to).contains(at) && m[0] & 0xF0 == 0x90 && m[2] > 0)
            .copied()
            .collect()
    }

    /// Notes the instrument is left holding: ons minus offs, "all notes off" clears a channel.
    fn hanging(&self) -> Vec<(u8, u8)> {
        let mut on: Vec<(u8, u8)> = Vec::new();
        for (_, m) in self.seen.lock().unwrap().iter() {
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
        on
    }
}

/// Where in the run "Stop all" is pressed.
#[derive(Clone, Copy, Debug)]
enum Moment {
    /// `frames` after the start of section `index` (or of the count-in, for `None`).
    After(Option<usize>, u64),
    /// So that the stop takes effect between `frames` and `frames - BLOCK` samples before section
    /// `index` begins (the boundary the armed switch lands on) - the control thread acts between
    /// two blocks, so a press is only ever that exact. 140 puts it inside the Octaver's read-ahead
    /// window (144 frames) of a loop that is re-anchored there; 1 lets it land on the boundary or
    /// just after it, so the armed section really begins.
    Before(usize, u64),
}

/// One press, and everything checked about it. Returns a description for the failure list, or
/// `None` when every promise held.
fn press(moment: Moment, mixer: bool) -> Option<String> {
    let mut stage = Stage::new(mixer);
    let guard = stage.runner.scheduler().guard;
    let count_in_start = stage
        .runner
        .scheduler()
        .timeline
        .bar_start(stage.first_bar - u64::from(stage.runner.count_in_bars()));
    let press_at = match moment {
        Moment::After(None, frames) => count_in_start + frames,
        Moment::After(Some(index), frames) => stage.section(index) + frames,
        Moment::Before(index, frames) => stage.section(index) - guard - frames,
    };
    stage.run_to(press_at);
    let busy = stage.busy_tracks();
    // The press finds the run really playing: an audio loop that runs is heard right before it.
    let audio_busy = busy.iter().any(|&t| t != stage.drums);
    if audio_busy && stage.peak(stage.sim.pos() - 2_400, stage.sim.pos()) == 0.0 {
        return Some(format!("{moment:?}: vor dem Stopp ist nichts zu hoeren - der Test misst nichts"));
    }
    let armed_at = stage.runner.armed_at().map(|(at, _)| at);
    let (soon, message) = stage.stop_all();
    // Long enough for every command that was in the engine to land: the armed section, a take
    // of its own length in it, and the boundary after it.
    let bar = stage.bar(1) - stage.bar(0);
    let until = armed_at.unwrap_or(soon).max(soon) + 11 * bar;
    stage.run_to(until);

    let mut faults = Vec::new();
    let view = stage.runner.view(stage.sim.pos());
    if stage.runner.phase() != Phase::Stopped || view.section.is_some() || view.armed.is_some() {
        faults.push(format!("Anzeige nicht gestoppt: {view:?}"));
    }
    if view.tracks.iter().any(|t| *t != ScoreState::Stop) {
        faults.push(format!("Anzeige zeigt Spuren an: {:?}", view.tracks));
    }
    let n = busy.len();
    let said = format!("({n} track{})", if n == 1 { "" } else { "s" });
    if !message.contains(&said) {
        faults.push(format!("Meldung \"{message}\", die Engine hatte {busy:?} in Betrieb"));
    }
    let status_busy = stage.busy_tracks();
    if !status_busy.is_empty() {
        faults.push(format!("Engine-Status am Ende: Spuren {status_busy:?} laufen noch"));
    }
    if mixer {
        // Tails only: whatever rings on after the stop decays and never comes back up. Half a
        // second for the tails to build (a reverb's diffusion grows before it falls), then no
        // window may be louder than that first half second, and after six seconds it is gone.
        let reference = stage.peak(soon, soon + 24_000).max(1e-4);
        let mut p = soon + 24_000;
        while p + 2_400 <= until {
            let level = stage.peak(p, p + 2_400);
            if level > reference * 1.001 {
                faults.push(format!(
                    "nach dem Stopp wird es wieder lauter: {level} bei {} (Bezug {reference})",
                    p - soon
                ));
                break;
            }
            if p >= soon + 6 * 48_000 && level > 1e-3 {
                faults.push(format!("6 s nach dem Stopp noch {level} bei {}", p - soon));
                break;
            }
            p += 2_400;
        }
    } else if let Some((p, v)) = stage.first_sound(soon, until) {
        faults.push(format!(
            "Klang {v} bei {} Samples nach dem Stopp (Wechsel bei {:?})",
            p - soon,
            armed_at.map(|a| a as i64 - soon as i64)
        ));
    }
    let late = stage.note_ons(soon, until);
    if !late.is_empty() {
        faults.push(format!("MIDI-Noten nach dem Stopp: {:?}", &late[..late.len().min(4)]));
    }
    let hanging = stage.hanging();
    if !hanging.is_empty() {
        faults.push(format!("haengende Noten: {hanging:?}"));
    }
    (!faults.is_empty()).then(|| format!("{moment:?} (Mixer {mixer}): {message} -> {faults:?}"))
}

/// Every press of one section, collected, so one failing moment does not hide the others.
fn presses(moments: &[Moment], mixer: bool) {
    let failures: Vec<String> = moments.iter().filter_map(|m| press(*m, mixer)).collect();
    assert!(failures.is_empty(), "{} von {}:\n{}", failures.len(), moments.len(), failures.join("\n"));
}

/// Early, one bar in, the middle, the last bar, the last moment before the boundary, inside the
/// read-ahead window before it, and a stop that lands just after it.
fn moments_of(index: usize) -> Vec<Moment> {
    let bar = BAR;
    let score = compile_score(SCORE).expect("die Partitur uebersetzt");
    let bars = u64::from(score.sections[index].bars);
    let mut out = vec![
        Moment::After(Some(index), 1_000),
        Moment::After(Some(index), bar * bars / 2 + 1_000),
    ];
    if bars > 1 {
        out.push(Moment::After(Some(index), bar + 1_000));
        out.push(Moment::After(Some(index), bar * (bars - 1) + 7_000));
    }
    if index + 1 < SECTIONS.len() {
        out.push(Moment::Before(index + 1, 3 * u64::from(BLOCK)));
        out.push(Moment::Before(index + 1, 140));
        out.push(Moment::Before(index + 1, 1));
    }
    out
}

macro_rules! section_tests {
    ($($name:ident: $index:expr,)*) => {$(
        #[test]
        fn $name() {
            presses(&moments_of($index), false);
        }
    )*};
}

section_tests! {
    stop_all_in_intro: 0,
    stop_all_in_intro_b: 1,
    stop_all_in_verse_1: 2,
    stop_all_in_verse_1_bass: 3,
    stop_all_in_verse_1_b: 4,
    stop_all_in_chorus_1: 5,
    stop_all_in_verse_2: 6,
    stop_all_in_verse_2_b: 7,
    stop_all_in_chorus_2: 8,
    stop_all_in_bridge: 9,
    stop_all_in_chorus_3: 10,
    stop_all_in_chorus_3_all: 11,
    stop_all_in_end: 12,
}

/// One bar at 94 BPM in 4/4 and 48 kHz, rounded down - for placing a press, not for checking.
const BAR: u64 = 122_553;

/// During the count-in: right after Start (inside the head start), in its middle, and just before
/// the first take begins.
#[test]
fn stop_all_in_the_count_in() {
    presses(
        &[
            Moment::After(None, 0),
            Moment::After(None, 60_000),
            Moment::Before(0, 3 * u64::from(BLOCK)),
            Moment::Before(0, 1),
        ],
        false,
    );
}

/// With the score's mixer: effect chains with tails and the Octaver on both bass tracks, whose
/// chain latency makes the engine read their loops ahead - so a loop that comes back at a
/// boundary is heard from its read-ahead before the boundary. Stops around the boundaries where
/// such loops come back, and the owner's moment in chorus 2.
#[test]
fn stop_all_with_the_mixer_leaves_only_tails() {
    let mut moments = Vec::new();
    // chorus_1 brings theme and bass back (re-anchored); verse_2 restarts them; chorus_2 restarts
    // the drums; chorus_3 brings drums and vocals back after the bridge, chorus_3_all the rest.
    for index in [5usize, 6, CHORUS_2, 10, 11] {
        moments.push(Moment::Before(index, 140));
        moments.push(Moment::Before(index, 3 * u64::from(BLOCK)));
    }
    moments.push(Moment::After(Some(CHORUS_2), BAR + 1_000));
    moments.push(Moment::After(Some(BRIDGE), 1_000));
    presses(&moments, true);
}

/// The owner's moment, by itself: a bar into chorus 2, six tracks sounding, the bridge armed.
/// Before the fix the sentence said "Stopped all (1 track)" and five loops played on until the
/// bridge, nine bars later.
#[test]
fn stop_all_a_bar_into_chorus_2_stops_all_six_tracks_now() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.section(CHORUS_2) + BAR + 1_000);
    assert_eq!(
        stage.busy_tracks().len(),
        6,
        "theme, bass, chorus_guitar, Drums, mic, backing_vocals"
    );
    let (soon, message) = stage.stop_all();
    assert!(message.starts_with("Stopped all (6 tracks)."), "{message}");
    assert!(message.contains("\"bridge\""), "{message}");
    let until = stage.section(BRIDGE) + 2 * BAR;
    stage.run_to(until);
    assert_eq!(stage.first_sound(soon, until), None);
    assert!(stage.busy_tracks().is_empty());
}

/// Run a new start on `stage` and play into the second section: the first take must be the full
/// four bars of the intro.
fn start_again_and_check_the_intro(stage: &mut Stage) {
    stage.runner.start(stage.sim.pos());
    stage.pump();
    stage.first_bar = stage.runner.armed_at().expect("armiert").1;
    stage.stopped = false;
    stage.run_to(stage.section(1) + 2_000);
    let theme = stage.sim.core.track(0);
    assert_eq!(
        theme.loop_len(),
        stage.section(1) - stage.section(0),
        "the intro take, four bars"
    );
    assert_eq!(stage.sim.latest_status().unwrap().tracks()[0].state, TrackState::Ready);
}

/// Stop all, then "Back to start", then Start again: the second run begins clean.
#[test]
fn back_to_start_after_stop_all_plays_the_score_again_from_scratch() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.section(CHORUS_2) + BAR + 1_000);
    stage.stop_all();
    stage.run_to(stage.sim.pos() + BAR);
    let (_, commands) = stage.runner.rewind(stage.sim.pos()).expect("rewind");
    for cmd in commands {
        stage.sim.send(cmd);
    }
    stage.run_to(stage.sim.pos() + 8 * u64::from(BLOCK));
    // What the host does once the clear has happened: the score's setup goes back on.
    for cmd in setup_commands(stage.runner.score()) {
        stage.sim.send(cmd);
    }
    stage.run_to(stage.sim.pos() + 8 * u64::from(BLOCK));
    let status = stage.sim.latest_status().unwrap();
    assert!(status.tracks().iter().all(|t| t.state == TrackState::Empty));
    start_again_and_check_the_intro(&mut stage);
}

/// The old edge case: Stop all during the count-in leaves every track empty, so a score may be
/// loaded again without a clear. The stopped run's first section was already in the engine (a
/// four-bar take on `theme`, ending in the middle of the new run's intro, plus what used to
/// neutralise it); none of it may touch the new run.
#[test]
fn a_score_loaded_after_stop_all_in_the_count_in_is_not_touched_by_the_old_run() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.sim.pos() + 20_000);
    stage.stop_all();
    stage.run_to(stage.sim.pos() + 8 * u64::from(BLOCK));
    // What the host does on load: setup, the closed click window, a new runner on the same grid.
    let score = stage.runner.score().clone();
    for cmd in setup_commands(&score) {
        stage.sim.send(cmd);
    }
    stage.sim.send(CLOSED_CLICK_GATE);
    let timeline = stage.runner.scheduler().timeline;
    stage.runner = Runner::new(score, timeline, BLOCK).expect("Runner");
    start_again_and_check_the_intro(&mut stage);
}

// ---------------------------------------------------------------------------------------------
// "Reload" in the Arrangement: reset everything, then load the file again
// ---------------------------------------------------------------------------------------------

/// What the host sends for a Reload, in its order (`Session::reset_for_reload` and `load_score`
/// in the app): the fresh start [`RELOAD_CLEAR`], the new score's tempo, its track setup, the loop
/// kind of the drums, the closed click window - and a new runner. `bpm` lets the reloaded file
/// differ from the one that was playing.
fn reload(stage: &mut Stage, bpm: f64) {
    let yaml = SCORE.replace("bpm: 94", &format!("bpm: {bpm}"));
    let score = compile_score(&yaml).expect("die neue Fassung uebersetzt");
    let timeline = Timeline::new(RATE, score.bpm, score.signature());
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap();
    let layer_frames = loop_capacity(&timeline, max_bars);
    stage.sim.send(RELOAD_CLEAR);
    stage.sim.send(Command::SetTempo {
        bpm: score.bpm,
        signature: score.signature(),
        layer_frames,
    });
    stage.sim.set_layer_frames(layer_frames as usize);
    for cmd in setup_commands(&score) {
        stage.sim.send(cmd);
    }
    stage.sim.send(Command::SetLoopKind {
        track: stage.drums,
        midi: true,
    });
    stage.sim.send(CLOSED_CLICK_GATE);
    stage.runner = Runner::new(score, timeline, BLOCK).expect("Runner");
    stage.stopped = true;
}

/// After a Reload: within two blocks every track is empty and silent, nothing of the old run acts
/// any more (a few bars of nothing), the new tempo is taken without a refusal, no note hangs - and
/// the reloaded score plays from its start like a freshly loaded one.
fn check_fresh_after_reload(stage: &mut Stage, bpm: f64) {
    let from = stage.sim.pos() + 2 * u64::from(BLOCK);
    let until = from + 12 * BAR;
    stage.run_to(until);
    assert_eq!(stage.first_sound(from, until), None, "nichts klingt nach dem Reload");
    let status = stage.sim.latest_status().unwrap();
    assert_eq!(status.refusal, crate::engine::command::Refusal::None);
    assert_eq!(status.bpm, bpm);
    assert!(
        status.tracks().iter().all(|t| t.state == TrackState::Empty && !t.playing),
        "{:?}",
        status.tracks().iter().map(|t| t.state).collect::<Vec<_>>()
    );
    assert!(stage.hanging().is_empty(), "{:?}", stage.hanging());
    assert!(stage.note_ons(from, until).is_empty());
    assert_eq!(stage.runner.phase(), Phase::Idle);
    start_again_and_check_the_intro(stage);
}

/// Reload while the score plays: a bar into chorus 2, six loops sounding, backing vocals recording.
#[test]
fn reload_while_the_score_plays_resets_everything_and_loads_again() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.section(CHORUS_2) + BAR + 1_000);
    assert_eq!(stage.busy_tracks().len(), 6);
    reload(&mut stage, 94.0);
    check_fresh_after_reload(&mut stage, 94.0);
}

/// Reload after "Stop all", with every loop of the run still recorded - and the file on disk now
/// at another tempo, which a load onto recorded loops would refuse.
#[test]
fn reload_with_recorded_loops_and_a_new_tempo() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.section(BRIDGE) + 1_000);
    stage.stop_all();
    stage.run_to(stage.sim.pos() + BAR);
    assert!(
        stage
            .sim
            .latest_status()
            .unwrap()
            .tracks()
            .iter()
            .filter(|t| t.state != TrackState::Empty)
            .count()
            >= 6
    );
    reload(&mut stage, 100.0);
    check_fresh_after_reload(&mut stage, 100.0);
}

/// Reload in the middle of the drum take (a MIDI loop recording, pads being hit).
#[test]
fn reload_during_the_drum_take_leaves_no_note_hanging() {
    let mut stage = Stage::new(false);
    stage.run_to(stage.section(VERSE_2) + BAR + 20_000);
    reload(&mut stage, 94.0);
    check_fresh_after_reload(&mut stage, 94.0);
}
