//! Phase 3, second half: the runner - the piece that actually plays a compiled score.
//!
//! ```text
//!  CompiledScore ──► Runner ──► Scheduler ──► Command{at} ──► Audio-Thread
//!   (Sollzustaende)   (Ist gegen Soll)   (Zeitstempel)
//! ```
//!
//! It runs in the **control thread** and never touches engine state directly: everything it does
//! leaves as a [`Command`] with a musical timestamp on it. The runner does not wait for bar lines -
//! it works out the sample a change falls on and hands the commands over long before that sample
//! arrives.
//!
//! # The declarative model, and why it means *fewer* commands
//!
//! A section states the complete target state of **every** track, not a delta. The runner therefore
//! compares what it last asked of a track with what the next section asks, and sends only the
//! difference. Written out, because it is the whole point:
//!
//! | vorher | jetzt | Kommandos |
//! |---|---|---|
//! | egal | `record` | `StartRecord`, `StopRecord`, `StartPlay` - ein Take ueber die Sektion |
//! | egal | `overdub` | `StartOverdub`, `StopRecord`, `StartPlay` - eine Ebene, ein Loop-Durchlauf |
//! | nicht `stack` | `stack` | `LatchOverdub an`, dann der erste Durchlauf wie `record` (leere Spur) bzw. `overdub` |
//! | `stack` | `stack` | **keins** - der Latch laeuft einfach weiter |
//! | `stack` | anderes | `LatchOverdub aus` mitten im letzten Durchlauf (+ `StopRecord` an dessen Ende, wenn ein Take folgt), dann wie oben |
//! | klingt schon | `play` | **keins** (mit `restart: true` ggf. `RestartLoop`) |
//! | still | `play` | `StartPlay`, davor `RestartLoop`, wenn die Sektion nicht auf seinem Raster beginnt |
//! | klingt | `stop` / `hear_through` | `StopPlay` |
//! | still | `stop` | **keins** |
//!
//! A track that stands on `play` through five sections gets exactly zero commands, and so does one
//! that stands on `stop`. That is not tidiness: every superfluous command at a section boundary is
//! a state change on the audio thread at a musically exposed moment, and the kind of fault one only
//! notices as a click twenty minutes into a take.
//!
//! # A loop that comes in starts from its top
//!
//! A loop is read as `(position - origin) mod loop_len`, phase-locked to the song from its first
//! take on. A section whose length is not a multiple of a loop would leave that loop out of phase
//! for everything after it. So a loop that comes **in** with a section - it was silent in the
//! section before, or the clip says `restart: true` - is re-anchored on the section's first sample
//! ([`Command::RestartLoop`]), unless the section begins on its grid anyway. See
//! [`Runner::restarts`] for the rule and `docs/architektur.md`, section 10, for the cases. A score
//! whose sections are multiples of its loops therefore gets no new command at all.
//!
//! # Why the runner predicts the loop geometry instead of reading it back
//!
//! An overdub has to be quantised to the track's own `(origin, loop_len)` grid, and a section is
//! scheduled *before* the take that defines that grid has finished - the status snapshot still says
//! `loop_len == 0` at that moment. The runner does not need the snapshot: it issued the take itself,
//! so it knows the loop will be exactly `[start, end)` of the section it recorded in. Every track
//! therefore carries a predicted geometry, written when the runner schedules a `record`, and the
//! prediction is exact because nothing else sends commands while a score is running.
//!
//! # `stack`: the engine keeps overdubbing, the runner only opens and closes the latch
//!
//! A `stack` section does not schedule one take per pass. It switches the engine's continuous
//! overdub ([`Command::LatchOverdub`], see `latch.rs`) on, timed for the first pass, and schedules
//! that first pass exactly like `record` (empty track: origin = section start, length = `bars`) or
//! like `overdub` (on the track's own grid). From then on the engine starts every further pass on
//! the very sample the previous one ended - `origin + n * loop_len` by construction, never a bar
//! line rounded on its own - and drops passes in which nothing was played.
//!
//! The end is known only once the following section is scheduled (at Release, or at once for an
//! `autorelease` section). The latch goes off **in the middle of the last pass** - the pass that
//! begins on the last grid point before the boundary - so that pass is recorded to its end and none
//! follows it. Half a pass is far more than any jitter between the threads and far less than the
//! pass itself. While a `stack` runs, "is a take still running here" is answered from the grid: the
//! change position is pushed to the end of the pass running at that moment, exactly like the end of
//! a planned take.
//!
//! # Why a change never lands inside a running take
//!
//! A release with `quantize: bar` could otherwise fall in bar 3 of an eight-bar recording. Cutting
//! the take there would define a **three-bar loop**, and everything recorded against it afterwards
//! would be wrong - a far worse outcome than waiting five bars. So the change position is pushed to
//! the end of the running take before it is quantised, and the runner says so. The take is finished
//! cleanly and the section changes on the boundary right after it.
//!
//! # Nothing is heard of the clock while the score does not play
//!
//! The engine's clock runs from the moment the score is loaded (the front end starts it; stopping
//! it would drop whatever is still waiting in the engine - see `Command::TransportStop`). But a
//! loaded score that has not been started, one that was stopped and one that has ended must be
//! **silent**: no click, no loop. The loops take care of themselves (every track is empty before
//! the start, and "Stop all" and the end stop every one of them). The click does not: it follows
//! the clock. So the engine's click window ([`Command::ClickGate`]) is held closed while a score is
//! loaded (the front end closes it on load, see [`CLOSED_CLICK_GATE`]), the runner opens it on the
//! first beat of the count-in and closes it again on the sample the score ends or "Stop all" takes
//! effect. The window is timed like everything else here, so the first click of the count-in and
//! the last click of the score are exact to the sample.
//!
//! # Start means now
//!
//! The phase of that idling clock means nothing, so the runner does not wait for its next bar line.
//! [`Runner::start`] makes the moment of the press - plus the head start every timed command needs
//! ([`Runner::start_lead`]) - bar 1 of the run: it sends `Command::TransportStart` for that sample,
//! which moves the engine's `Timeline::zero` there (nothing is stopped, nothing waiting is dropped,
//! and there is no loop to re-anchor: a score only starts on empty tracks), and moves its own grid
//! the same way. The count-in is bars `0..count_in` of that grid, the first section begins on bar
//! `count_in`, and every plan of the run is made on it - so a run is the same to the sample,
//! relative to its own bar 1, whenever the button was pressed.
//!
//! # Monitoring is the one thing that cannot be timestamped
//!
//! [`Command::SetMonitor`] carries no position (see `command.rs`), so it cannot be sent five bars
//! early - the singer would become audible five bars early. The runner therefore holds the monitor
//! change back and sends it in [`Runner::tick`], at the moment the engine has actually reached the
//! section boundary. That is accurate to one turn of the control loop, a few milliseconds; nothing
//! that is *recorded* depends on it, because a recording is sample-stamped either way.

use std::time::Duration;

use crate::score::{ClipOptions, CompiledScore, TrackState as ScoreState};

use super::command::{Command, MAX_TRACKS};
use super::live::TrackDef;
use super::schedule::{Quantize, Scheduled, Scheduler};
use super::timeline::Timeline;

/// Bars of count-in before the first section, unless the score or the caller says otherwise.
///
/// Two bars - the owner's choice from playing with it. One bar (the shortest lead that is still a
/// *bar*) proved too short in practice: the first bar is spent recognising the tempo, the second
/// one is where the musician gets ready to come in, so the entry is hittable even at a fast tempo
/// or an odd metre. A score that wants something else says so with `count_in:` (1 to 8). The lead
/// is exactly this: the count-in starts the moment Start is pressed (plus [`Runner::start_lead`]),
/// and that moment is bar 1 of the run - no rest of a bar is waited for.
pub const DEFAULT_COUNT_IN_BARS: u32 = 2;
/// Longest count-in that can be set. More than eight bars of click is waiting, not counting in.
pub const MAX_COUNT_IN_BARS: u32 = 8;

/// What a front end sends the moment it loads a score: the click window closed, at once (timestamp
/// 0 is long past, so the engine acts on it in its next block). A loaded score is silent until it
/// is started - see the module comment.
pub const CLOSED_CLICK_GATE: Command = Command::ClickGate {
    at: 0,
    open: false,
    bar1: 0,
};

/// The refusal every by-hand transport action gets while a score is playing.
///
/// It lives here, next to the runner whose rule it is, because it has to be **one** sentence for
/// **every** way of pressing the button: the mouse (`Action::moves_transport` in the app's
/// `host.rs`) and a MIDI pad (`midi::Target::moves_transport`) run into the same wall for the same
/// reason, and two wordings would suggest two rules. See `docs/architektur.md` section 10.
pub const TRANSPORT_BELONGS_TO_SCORE: &str =
    "The score is running - it sends the Record and Play commands itself. \
     Manual transport works again after \"Stop all\". Level, pan, layers and \
     effects stay adjustable meanwhile.";

/// What the runner is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Score loaded, nothing scheduled.
    Idle,
    /// The count-in is running; the first section is armed.
    CountIn,
    /// A section is sounding.
    Running,
    /// The last section is over.
    Finished,
    /// The musician stopped everything.
    Stopped,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Idle => "ready",
            Phase::CountIn => "count-in",
            Phase::Running => "running",
            Phase::Finished => "end",
            Phase::Stopped => "stopped",
        }
    }

    /// Whether the score is over, one way or the other.
    pub fn is_over(self) -> bool {
        matches!(self, Phase::Finished | Phase::Stopped)
    }
}

/// One section, with `repeat:` resolved and the track states flattened into track order.
///
/// A copy rather than a borrow of [`crate::score::CompiledSection`]: the runner mutates itself
/// while it reads a section, and a `Vec` indexed by track number is what the diff wants anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SectionPlan {
    id: String,
    bars: u32,
    autorelease: bool,
    quantize: Quantize,
    /// Target state per track, in track order. Always complete - the compiler fills the gaps.
    want: Vec<ScoreState>,
    /// Clip options per track, in track order (`restart`, `bars`); empty where none are written.
    clips: Vec<ClipOptions>,
}

/// Where a section sits on the sample timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub index: usize,
    /// First sample of the section.
    pub start: u64,
    /// Zero-based bar index of that sample.
    pub start_bar: u64,
    pub bars: u32,
}

/// What an armed change will turn into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Section(usize),
    /// Past the last section: everything falls silent and the run is over.
    End,
}

/// A change that has been scheduled but has not taken effect yet.
///
/// Its commands are already in the engine's waiting room - that is the point of scheduling early -
/// which is exactly why it cannot be retargeted afterwards. See [`Runner::goto`].
#[derive(Clone, Debug, PartialEq)]
struct Armed {
    target: Target,
    at: u64,
    at_bar: u64,
    /// Every track as it was **before** this change was scheduled - i.e. as it is while the
    /// current section sounds. Scheduling a change rewrites the runner's picture of each track to
    /// what it will be once the change has happened (with `autorelease` that is the whole time a
    /// section plays), so [`Runner::stop_all`] reads what is true *now* from here.
    before: Vec<Now>,
}

/// One track as the sounding section has it - see [`Armed::before`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Now {
    sounding: bool,
    /// End of the take running in that section, 0 for none.
    take_end: u64,
    stacking: bool,
}

/// The runner's own picture of one track.
#[derive(Clone, Debug)]
struct TrackRun {
    name: String,
    /// Whether the score allows this track to be monitored at all.
    can_monitor: bool,
    /// An instrument track: heard whenever it is played (unless the score says monitor: false,
    /// which the engine was started with), so the sections never switch its monitoring.
    instrument: bool,
    /// What the last scheduled section asks of it.
    target: ScoreState,
    /// Whether the runner believes the track is audible once that section has begun.
    sounding: bool,
    /// Predicted loop geometry - see the module comment. `len == 0` means "no loop yet".
    origin: u64,
    /// The bar `origin` is the line of. Every origin the runner sets is a section start, so it is
    /// one; kept as a number so "does this section begin on the loop's grid" is asked in bars.
    origin_bar: u64,
    len: u64,
    /// The same length in bars. Kept beside `len` because bar boundaries are rounded individually:
    /// eight bars here and eight bars there can differ by a sample, so the question "is this
    /// section long enough for one pass of the loop" has to be asked in bars, not in samples.
    len_bars: u32,
    layers: u8,
    /// End of a take the runner has scheduled and that has not been passed yet; 0 means none.
    take_end: u64,
    /// The bar that end falls in. A take's end and a section's bar line can differ by a sample
    /// (every bar boundary is rounded on its own), and a change must not be pushed a whole section
    /// further just because of that - so "is a take still running here" is asked in bars.
    take_end_bar: u64,
    /// Monitoring as last sent to the engine.
    monitor: bool,
    /// A `stack` is running: the latch is on (or scheduled to go on) and no end is planned yet.
    stacking: bool,
    /// Where that latch went on - the start of the stack's first pass.
    stack_from: u64,
}

/// What a display needs, as one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct RunnerView {
    pub phase: Phase,
    /// Section sounding right now, 0-based, and its id.
    pub section: Option<usize>,
    pub section_id: String,
    pub section_count: usize,
    pub bars_total: u32,
    /// 1-based bar inside the current pass of the section.
    pub bar: u32,
    /// 1-based beat inside that bar.
    pub beat: u32,
    /// 1-based pass number - a section without `autorelease` repeats until the release button.
    pub pass: u32,
    /// Target state per track, in track order.
    pub tracks: Vec<ScoreState>,
    /// Description of an armed change plus its lead, or `None`.
    pub armed: Option<ArmedView>,
    /// Bars of count-in the next start plays.
    pub count_in_bars: u32,
}

/// An armed change as the display shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmedView {
    /// `voice_2` or `Ende`.
    pub label: String,
    /// Index of the section the change leads to, or `None` when it leads past the last one. A
    /// display needs the number rather than the name: two sections may carry the same id, and a
    /// block preview highlights a card by position.
    pub section: Option<usize>,
    pub bars: u32,
    pub beats: u32,
    /// Whether this is the count-in rather than a section change.
    pub count_in: bool,
}

impl ArmedView {
    /// `Switch armed: "voice_2" in 5 bars`, or the count-in equivalent.
    pub fn text(&self) -> String {
        let head = if self.count_in {
            "Count-in: start".to_string()
        } else {
            format!("Switch armed: \"{}\"", self.label)
        };
        format!("{head} {}", lead_text(self.bars, self.beats))
    }
}

/// Plays a compiled score against one engine configuration.
#[derive(Debug)]
pub struct Runner {
    score: CompiledScore,
    plans: Vec<SectionPlan>,
    sched: Scheduler,
    count_in_bars: u32,
    phase: Phase,
    tracks: Vec<TrackRun>,
    current: Option<Placement>,
    armed: Option<Armed>,
    /// Commands waiting to be handed to the engine, oldest first.
    out: Vec<Command>,
    /// Latest sentence for the display.
    message: String,
    /// Sentences the runner produced on its own, at a section boundary - nobody pressed anything,
    /// so no command reply carries them. Collected until the caller takes them.
    notices: Vec<String>,
    /// Where the click window opens for the run that was started last (the first bar of the
    /// count-in); 0 before any start. A close must never land before it - see [`Runner::stop_all`].
    gate_open_at: u64,
}

impl Runner {
    /// Build a runner for `score` on an engine that runs at `timeline` with `buffer_frames`.
    ///
    /// The scheduler's loop length is the **longest** section of the score, which is also the length
    /// every layer buffer has to be allocated at.
    pub fn new(score: CompiledScore, timeline: Timeline, buffer_frames: u32) -> Result<Self, String> {
        if score.sections.is_empty() {
            return Err("The score has no sections.".to_string());
        }
        if score.tracks.len() > MAX_TRACKS {
            return Err(format!(
                "The score names {} tracks; the engine can run at most {MAX_TRACKS}.",
                score.tracks.len()
            ));
        }
        let plans: Vec<SectionPlan> = score
            .sections
            .iter()
            .map(|section| SectionPlan {
                id: section.id.clone(),
                bars: section.bars.max(1),
                autorelease: section.autorelease,
                quantize: section.quantize,
                want: score
                    .tracks
                    .iter()
                    .map(|track| {
                        section
                            .tracks
                            .get(&track.name)
                            .copied()
                            .unwrap_or(ScoreState::Stop)
                    })
                    .collect(),
                clips: score
                    .tracks
                    .iter()
                    .map(|track| section.clip(&track.name))
                    .collect(),
            })
            .collect();
        let tracks: Vec<TrackRun> = score
            .tracks
            .iter()
            .map(|track| TrackRun {
                name: track.name.clone(),
                can_monitor: track.monitor,
                instrument: track.is_instrument(),
                // Before anything is scheduled every track is silent and empty, which is exactly
                // the state a freshly started engine is in.
                target: ScoreState::Stop,
                sounding: false,
                origin: 0,
                origin_bar: 0,
                len: 0,
                len_bars: 0,
                layers: 0,
                take_end: 0,
                take_end_bar: 0,
                monitor: false,
                stacking: false,
                stack_from: 0,
            })
            .collect();
        let bars = plans.iter().map(|p| p.bars).max().unwrap_or(1);
        // The score's own `count_in:` if it has one; the reader has checked the range already.
        let count_in_bars = score
            .count_in
            .unwrap_or(DEFAULT_COUNT_IN_BARS)
            .clamp(1, MAX_COUNT_IN_BARS);
        Ok(Self {
            score,
            plans,
            // The quantisation of the scheduler itself is never used by the runner - every section
            // brings its own - so the default is only what the shared display strings read.
            sched: Scheduler::new(timeline, bars, buffer_frames, Quantize::Loop),
            count_in_bars,
            phase: Phase::Idle,
            tracks,
            current: None,
            armed: None,
            message: String::new(),
            out: Vec::new(),
            notices: Vec::new(),
            gate_open_at: 0,
        })
    }

    /// Bars of count-in before the first section, overriding the score's `count_in:` for this
    /// runner. See [`DEFAULT_COUNT_IN_BARS`].
    #[must_use]
    pub fn with_count_in(mut self, bars: u32) -> Self {
        self.count_in_bars = bars.clamp(1, MAX_COUNT_IN_BARS);
        self
    }

    /// How long after the press the first beat of the count-in sounds, in samples: the scheduler's
    /// head start (`Scheduler::guard`, four audio buffers plus 20 ms - 31 ms at 128 frames and
    /// 48 kHz, 41 ms at 256). The same head start a free-looping Start has, and for the same reason:
    /// the start is a timed command, and it has to be in the audio thread's waiting room before
    /// the sample it names is rendered, or the first beat would be cut. Fixed - it does not depend
    /// on where in a bar the button was pressed, because no bar line is waited for.
    pub fn start_lead(&self) -> u64 {
        self.sched.guard
    }

    /// Bars of count-in the next start plays - after the clamp to at least one.
    pub fn count_in_bars(&self) -> u32 {
        self.count_in_bars
    }

    /// Change the count-in of a loaded score before it is started (or after a rewind). Refused
    /// while it plays: the count-in of a running score is already over or already scheduled.
    pub fn set_count_in(&mut self, bars: u32) -> String {
        self.message = if self.phase == Phase::Idle {
            self.count_in_bars = bars.clamp(1, MAX_COUNT_IN_BARS);
            let note = if bars == 0 {
                " (at least one bar - without it the first take would start at the very next bar line)"
            } else if bars > MAX_COUNT_IN_BARS {
                " (at most 8)"
            } else {
                ""
            };
            format!(
                "Count-in: {} bar{}{note}.",
                self.count_in_bars,
                if self.count_in_bars == 1 { "" } else { "s" }
            )
        } else {
            "The count-in can only be changed before the score starts.".to_string()
        };
        self.message.clone()
    }

    pub fn score(&self) -> &CompiledScore {
        &self.score
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.sched
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The longest section in bars - the length every layer buffer has to hold.
    pub fn max_bars(&self) -> u32 {
        self.sched.bars
    }

    /// Latest sentence about what happened.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where the audio thread is now, as well as the control side can know.
    pub fn estimated_pos(&self, last: Option<(u64, Duration)>) -> u64 {
        self.sched.estimated_pos(last)
    }

    /// What happened on its own since the last call: a section began, the score ended. Oldest
    /// first. A front end shows these the way it shows the answer to a button press.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// Commands the engine has not been given yet. The caller sends them in this order.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.out)
    }

    // ---- the buttons -------------------------------------------------------------------------

    /// Start the count-in and arm the first section.
    ///
    /// Everything the first section needs is scheduled here, with timestamps - the runner does not
    /// come back at the boundary to do it.
    pub fn start(&mut self, est: u64) -> String {
        if self.phase.is_over() {
            self.message =
                "The score has ended. Use \"Back to start\" to clear the tracks and play it again."
                    .to_string();
            return self.message.clone();
        }
        if self.phase != Phase::Idle {
            self.message = "The score is already running.".to_string();
            return self.message.clone();
        }
        // The count-in begins **now** - as soon as the command queue can still reach the audio
        // thread in time ([`Runner::start_lead`]), not on a bar line of the clock that has been running
        // since the load. That moment becomes bar 1 of the grid (`Timeline::zero`): the first beat
        // of the count-in, and the first section exactly `count_in` bars later. Every plan of this
        // run - section boundaries, takes, loop geometry, the click - is made on that grid, so the
        // run is the same to the sample whenever the button was pressed. See "Start means now" in
        // the module comment.
        let anchor = self.sched.soon(est);
        self.sched.timeline = self.sched.timeline.with_zero(anchor);
        let first_bar = self.count_in_bars as u64;
        let first = self.sched.timeline.bar_start(first_bar);
        self.phase = Phase::CountIn;
        // The engine's clock moves its bar 1 to the same sample. Nothing waiting is dropped and
        // nothing stops: the tracks are empty (a score starts only after a load or "Back to
        // start", both of which leave them so), so there is no loop to re-anchor either.
        self.out.push(Command::TransportStart { at: anchor });
        // The click is heard from the first beat of the count-in on - not a sample earlier.
        self.out.push(Command::ClickGate {
            at: anchor,
            open: true,
            bar1: first,
        });
        self.gate_open_at = anchor;
        self.arm(Target::Section(0), first, first_bar);
        self.message = format!(
            "Count-in: {} bar{} of click, then \"{}\".",
            self.count_in_bars,
            if self.count_in_bars == 1 { "" } else { "s" },
            self.plans[0].id,
        );
        self.message.clone()
    }

    /// The release button: arm the change to the next section.
    ///
    /// Pressing it twice does not skip anything - the second press finds a change already armed and
    /// only reports how far away it is.
    pub fn next(&mut self, est: u64) -> String {
        self.message = match self.phase {
            Phase::Idle => "The score is not running yet - press Start first.".to_string(),
            Phase::Finished | Phase::Stopped => "The score has ended.".to_string(),
            Phase::CountIn | Phase::Running => match self.armed.clone() {
                Some(armed) => {
                    let (bars, beats) = self.sched.distance(est, armed.at);
                    format!(
                        "A switch to \"{}\" is already armed ({}).",
                        self.target_label(armed.target),
                        lead_text(bars, beats)
                    )
                }
                None => self.arm_next(est, None),
            },
        };
        self.message.clone()
    }

    /// Jump to a section instead of taking the next one.
    ///
    /// Refused while a change is armed, and the refusal is the honest answer rather than a
    /// limitation worth hiding: the armed section's commands are already sitting in the audio
    /// thread's waiting room with their timestamps on them, and nothing in this design can take
    /// them back. Retargeting would mean firing both.
    pub fn goto(&mut self, index: usize, est: u64) -> String {
        self.message = if index >= self.plans.len() {
            format!(
                "There is no section {}. Sections: 1 to {}.",
                index + 1,
                self.plans.len()
            )
        } else {
            match self.phase {
                Phase::Idle => "The score is not running yet - press Start first.".to_string(),
                Phase::Finished | Phase::Stopped => "The score has ended.".to_string(),
                Phase::CountIn | Phase::Running => match self.armed.clone() {
                    Some(armed) => format!(
                        "A switch to \"{}\" is already armed - jump again once it has happened.",
                        self.target_label(armed.target)
                    ),
                    None => self.arm_next(est, Some(index)),
                },
            }
        };
        self.message.clone()
    }

    /// Stop every track and end the run - the emergency brake.
    ///
    /// What it guarantees, from the sample it takes effect on (`soon`, the head start every
    /// command needs - 31 ms at 128 frames): every loop is silent, every monitored input is off,
    /// the click is closed, and nothing the run had scheduled acts any more - not the armed
    /// section, not the end of a take, not a re-anchoring, not the `StartPlay` a take leaves
    /// behind. That last part is one command, [`Command::DropWaiting`]: the run schedules bars
    /// ahead (a whole section ahead with `autorelease`), and calling all of it back is the only
    /// answer that cannot miss a case. Answering each waiting command with a counter-command where
    /// it lands - what this did before - missed several: with `autorelease` the runner's picture
    /// already describes the *armed* section, so every track that falls silent there was "not
    /// sounding" and kept playing until the boundary (the owner's "Stopped all (1 track)" in a
    /// chorus with six tracks playing); and a take that ends inside its section, or an overdub
    /// that starts on a later grid point, was never answered at all.
    ///
    /// The one thing deliberately left running is a take: it is closed on the next bar line (or
    /// at its own end, if that comes first) rather than cut at an arbitrary sample, and nothing
    /// of it is heard - its track is stopped, its monitor off.
    pub fn stop_all(&mut self, est: u64) -> String {
        let soon = self.sched.soon(est);
        let bar = self.sched.next_bar(est);
        // Scheduling a change rewrites the picture of every track to what it will be once the
        // change has happened; what is true *now* is what the change found.
        let (playing_now, landed) = match &self.armed {
            Some(armed) => (armed.before.clone(), armed.at <= soon),
            None => (self.now(), true),
        };
        // An armed change that lands before the stop takes effect does happen (it was due before
        // the call-back), and so do the takes it starts: those are the ones to close.
        let taking = if landed { self.now() } else { playing_now.clone() };

        // Everything the run still has waiting in the engine: gone, at the very sample the stop
        // takes effect. Whatever is sent below comes after it and stays.
        self.out.push(Command::DropWaiting { at: soon });

        let mut stopped = 0usize;
        for track in 0..self.tracks.len() {
            let name = self.tracks[track].name.clone();
            let take_end = taking[track].take_end;
            let running_take = take_end > soon;
            let stacking = taking[track].stacking || playing_now[track].stacking;
            // A running stack: no further pass.
            if stacking {
                self.out.push(Command::LatchOverdub {
                    track,
                    on: false,
                    at: soon,
                });
            }
            // A take that is still running is closed on the bar line rather than left open (its
            // own end was called back with everything else); the loop it defines is short, but the
            // musician asked for silence. A take that ends before that bar line keeps its end.
            if running_take {
                self.out.push(Command::StopRecord {
                    track,
                    at: take_end.min(bar),
                });
            } else if stacking {
                self.out.push(Command::StopRecord { track, at: bar });
            }
            // Every track is stopped, sounding or not - whatever the picture says, the engine is
            // silent after this.
            let plan = self.sched.stop_play_at(track, &name, soon);
            self.push(plan);
            let now = playing_now[track];
            if now.sounding || now.take_end > est || now.stacking {
                stopped += 1;
            }
            self.set_monitor(track, false);
            self.tracks[track].sounding = false;
            self.tracks[track].target = ScoreState::Stop;
            self.tracks[track].take_end = 0;
            self.tracks[track].stacking = false;
        }

        let note = match self.armed.take() {
            Some(armed) if !landed => format!(
                " The switch to \"{}\" that was armed is called back.",
                self.target_label(armed.target),
            ),
            _ => String::new(),
        };
        // The click falls silent with the loops. Never before the window opened: pressed within the
        // head start of a Start, the opening is still waiting in the engine and would otherwise act
        // after the close (commands at one position run in the order they were sent).
        self.out.push(Command::ClickGate {
            at: soon.max(self.gate_open_at),
            open: false,
            bar1: 0,
        });
        self.phase = Phase::Stopped;
        self.current = None;
        self.message = format!(
            "Stopped all ({stopped} track{}).{note}",
            if stopped == 1 { "" } else { "s" }
        );
        self.message.clone()
    }

    /// Back to the start: the run is forgotten and the score can be started again.
    ///
    /// **The tracks are cleared, and that is not optional.** The runner plans every overdub
    /// against the loop geometry of the takes it scheduled itself and starts from the belief that
    /// every track is empty (see the module comment). A loop left over from the previous run would
    /// be invisible to that plan - the first `play` would not start it, the first `overdub` would
    /// land on a grid the runner does not know. So rewinding means: clear every track (one
    /// `ClearAll`, which also drops whatever the previous run still had waiting in the engine), and
    /// begin again from an empty session, exactly as after loading.
    ///
    /// Refused while the score plays: "Stop all" first. Returns the commands to send; the caller
    /// also has to restore the score's track setup once the clear has happened, because a clear
    /// puts every track back to the values it was *started* with.
    pub fn rewind(&mut self, est: u64) -> Result<(String, Vec<Command>), String> {
        if matches!(self.phase, Phase::CountIn | Phase::Running) {
            return Err(
                "The score is playing. Press \"Stop all\" first, then \"Back to start\"."
                    .to_string(),
            );
        }
        let soon = self.sched.soon(est);
        let clear = Command::ClearAll { at: soon };
        // The clear drops whatever was still waiting - a close of the window among it, if "Stop
        // all" was pressed a moment ago. So the window is closed again behind it.
        let gate = Command::ClickGate {
            at: soon,
            open: false,
            bar1: 0,
        };
        for track in self.tracks.iter_mut() {
            track.target = ScoreState::Stop;
            track.sounding = false;
            track.origin = 0;
            track.origin_bar = 0;
            track.len = 0;
            track.len_bars = 0;
            track.layers = 0;
            track.take_end = 0;
            track.take_end_bar = 0;
            track.stacking = false;
            // A clear restores each track's start-up monitoring, which the caller then switches
            // off again; the runner's picture is "off" either way.
            track.monitor = false;
        }
        self.out.clear();
        self.notices.clear();
        self.current = None;
        self.armed = None;
        self.phase = Phase::Idle;
        self.message = format!(
            "Back to start: all tracks cleared, \"{}\" is ready to play again.",
            self.score.title
        );
        Ok((self.message.clone(), vec![clear, gate]))
    }

    // ---- the clock ---------------------------------------------------------------------------

    /// One turn of the control loop: `pos` is where the engine is.
    ///
    /// This is where an armed change becomes the sounding section, where the monitoring of that
    /// section is switched (the one command that carries no timestamp) and where the follow-up of
    /// an `autorelease` section is armed.
    pub fn tick(&mut self, pos: u64) {
        for track in self.tracks.iter_mut() {
            if track.take_end != 0 && pos >= track.take_end {
                track.take_end = 0;
                track.take_end_bar = 0;
            }
        }
        let Some(armed) = self.armed.clone() else {
            return;
        };
        if pos < armed.at {
            return;
        }
        self.armed = None;
        match armed.target {
            Target::Section(index) => {
                self.current = Some(Placement {
                    index,
                    start: armed.at,
                    start_bar: armed.at_bar,
                    bars: self.plans[index].bars,
                });
                self.phase = Phase::Running;
                self.apply_monitor(index);
                self.message = format!(
                    "Section {}/{} \"{}\": {} bar{}{}.",
                    index + 1,
                    self.plans.len(),
                    self.plans[index].id,
                    self.plans[index].bars,
                    if self.plans[index].bars == 1 { "" } else { "s" },
                    if self.plans[index].autorelease {
                        ""
                    } else {
                        ", loops until Release"
                    }
                );
                self.notices.push(self.message.clone());
                if self.plans[index].autorelease {
                    // The follow-up is known to the sample, so it is armed and sent right away.
                    let at_bar = armed.at_bar + self.plans[index].bars as u64;
                    let at = self.sched.timeline.bar_start(at_bar);
                    let target = if index + 1 < self.plans.len() {
                        Target::Section(index + 1)
                    } else {
                        Target::End
                    };
                    self.arm(target, at, at_bar);
                }
            }
            Target::End => {
                for track in 0..self.tracks.len() {
                    self.set_monitor(track, false);
                }
                self.current = None;
                self.phase = Phase::Finished;
                self.message = "End of the score - all tracks silent.".to_string();
                self.notices.push(self.message.clone());
            }
        }
    }

    // ---- what the display reads --------------------------------------------------------------

    pub fn view(&self, pos: u64) -> RunnerView {
        let timeline = &self.sched.timeline;
        let (section, section_id, bars_total, bar, pass) = match self.current {
            Some(p) => {
                let elapsed = timeline.bar_index_at(pos).saturating_sub(p.start_bar);
                let bars = p.bars.max(1) as u64;
                (
                    Some(p.index),
                    self.plans[p.index].id.clone(),
                    p.bars,
                    (elapsed % bars) as u32 + 1,
                    (elapsed / bars) as u32 + 1,
                )
            }
            None => (None, String::new(), 0, 0, 0),
        };
        // Before the start and after the end the clock's phase means nothing (the next start moves
        // bar 1 anyway), so nothing counts: beat 1. During the head start of a Start the grid is
        // already the new one and reads beat 1 too, because a position before `zero` is beat 0.
        let beat = if matches!(self.phase, Phase::CountIn | Phase::Running) {
            (timeline.beat_index_at(pos) % timeline.beats_per_bar().max(1)) as u32 + 1
        } else {
            1
        };
        let tracks = match self.current {
            Some(p) => self.plans[p.index].want.clone(),
            None => match self.armed.as_ref().map(|a| a.target) {
                Some(Target::Section(i)) => self.plans[i].want.clone(),
                _ => vec![ScoreState::Stop; self.tracks.len()],
            },
        };
        let armed = self.armed.as_ref().map(|a| {
            let (bars, beats) = self.sched.distance(pos, a.at);
            ArmedView {
                label: self.target_label(a.target),
                section: match a.target {
                    Target::Section(index) => Some(index),
                    Target::End => None,
                },
                bars,
                beats,
                count_in: self.phase == Phase::CountIn,
            }
        });
        RunnerView {
            phase: self.phase,
            section,
            section_id,
            section_count: self.plans.len(),
            bars_total,
            bar,
            beat,
            pass,
            tracks,
            armed,
            count_in_bars: self.count_in_bars,
        }
    }

    /// Name of a track, for the display.
    pub fn track_name(&self, index: usize) -> &str {
        self.tracks
            .get(index)
            .map(|t| t.name.as_str())
            .unwrap_or("?")
    }

    /// Sample position and bar index an armed change takes effect at, if one is armed.
    pub fn armed_at(&self) -> Option<(u64, u64)> {
        self.armed.as_ref().map(|a| (a.at, a.at_bar))
    }

    /// Section ids in order, for a table of contents.
    pub fn section_ids(&self) -> Vec<&str> {
        self.plans.iter().map(|p| p.id.as_str()).collect()
    }

    // ---- the machinery -----------------------------------------------------------------------

    fn target_label(&self, target: Target) -> String {
        match target {
            Target::Section(i) => self.plans[i].id.clone(),
            Target::End => "End".to_string(),
        }
    }

    fn push(&mut self, plan: Scheduled) {
        self.out.extend(plan.commands);
    }

    /// Monitoring is untimed, so it is only ever changed here - when the engine has reached the
    /// boundary - and only when it really differs from what the engine was last told.
    fn set_monitor(&mut self, track: usize, on: bool) {
        if self.tracks[track].instrument {
            return;
        }
        let on = on && self.tracks[track].can_monitor;
        if self.tracks[track].monitor == on {
            return;
        }
        self.tracks[track].monitor = on;
        self.out.push(Command::SetMonitor { track, on });
    }

    fn apply_monitor(&mut self, index: usize) {
        for track in 0..self.tracks.len() {
            let state = self.plans[index].want[track];
            let want = state.records() || state == ScoreState::HearThrough;
            self.set_monitor(track, want);
        }
    }

    /// Where a change triggered now may take effect, on the grid the current section asks for.
    ///
    /// Two things are added on top of the raw grid: the head start every command needs, and the end
    /// of any take that is still running - see the module comment for why a take is never cut.
    ///
    /// Returns the sample, the bar index, and whether a running take pushed the change further
    /// than the section's own grid would have put it.
    fn change_position(&self, est: u64, current: Placement) -> (u64, u64, bool) {
        // The first bar a change may fall on: no earlier than the bar a running take ends in. A
        // running stack has a take in every pass, so its end is the end of the pass that runs
        // when the change could take effect at the earliest.
        let earliest = est + self.sched.guard;
        let min_bar = self
            .tracks
            .iter()
            .filter(|t| t.take_end != 0 || t.stacking)
            .map(|t| {
                let mut bar = if t.take_end != 0 { t.take_end_bar } else { 0 };
                if t.stacking && t.len > 0 {
                    let pass_end = Scheduler::loop_grid_at_or_after(t.origin, t.len, earliest);
                    bar = bar.max(self.bar_near(pass_end));
                }
                bar
            })
            .max()
            .unwrap_or(0);
        let (at, at_bar) = self.grid_position(est, current, min_bar);
        let (_, free_bar) = self.grid_position(est, current, 0);
        (at, at_bar, at_bar > free_bar)
    }

    /// Half a beat in samples: the tolerance between a loop grid point and the bar line it belongs
    /// to. The two are rounded separately and drift apart by up to a sample per pass; half a beat
    /// is far more than that and far less than any loop, which is always whole bars.
    fn slack(&self) -> u64 {
        (self.sched.timeline.samples_per_beat() / 2.0) as u64
    }

    /// The bar whose line `pos` is (to within [`Self::slack`]) or lies in.
    fn bar_near(&self, pos: u64) -> u64 {
        self.sched.timeline.bar_index_at(pos + self.slack())
    }

    /// The end of a running stack, before the section that starts at `start` takes over.
    ///
    /// The last pass is the one that begins on the last grid point before `start`; the latch goes
    /// off in its middle, so it is recorded to its end and nothing follows it. When a take follows
    /// on this track, that pass also gets its `StopRecord`, which lets the engine queue the new
    /// take behind it instead of refusing it as busy.
    fn end_stack(&mut self, track: usize, start: u64, next: ScoreState) {
        let slack = self.slack();
        let t = &self.tracks[track];
        let (origin, len, from) = (t.origin, t.len, t.stack_from);
        let limit = start.saturating_sub(slack);
        let last = if len == 0 || limit <= origin {
            origin
        } else {
            origin + (limit - origin - 1) / len * len
        };
        let pass_end = last + len;
        let off = (last + len / 2).max(from + 1);
        self.out.push(Command::LatchOverdub {
            track,
            on: false,
            at: off,
        });
        if matches!(next, ScoreState::Record | ScoreState::Overdub) {
            self.out.push(Command::StopRecord {
                track,
                at: pass_end,
            });
        }
        let bar = self.bar_near(pass_end);
        let t = &mut self.tracks[track];
        t.stacking = false;
        if pass_end > t.take_end {
            t.take_end = pass_end;
            t.take_end_bar = bar;
        }
    }

    /// The first point of the current section's quantisation grid at or after `est` plus the head
    /// start, and not before bar `min_bar`.
    fn grid_position(&self, est: u64, current: Placement, min_bar: u64) -> (u64, u64) {
        let timeline = self.sched.timeline;
        let earliest = est + self.sched.guard;
        match self.plans[current.index].quantize {
            Quantize::Bar => {
                let at_bar = timeline
                    .bar_index_at(timeline.bar_start_at_or_after(earliest))
                    .max(min_bar);
                (timeline.bar_start(at_bar), at_bar)
            }
            Quantize::Loop => {
                // The next end of a pass of *this* section, counted in bars so an odd time
                // signature cannot round the boundary away from the section's own grid. At least
                // pass 1: a section cannot end before its first pass is through.
                let bars = current.bars.max(1) as u64;
                let mut pass = (timeline
                    .bar_index_at(earliest)
                    .saturating_sub(current.start_bar)
                    / bars)
                    .max(1);
                loop {
                    let at_bar = current.start_bar + pass * bars;
                    let at = timeline.bar_start(at_bar);
                    if at >= earliest && at_bar >= min_bar {
                        return (at, at_bar);
                    }
                    pass += 1;
                }
            }
        }
    }

    /// Arm the section after the current one, or the one `to` names.
    fn arm_next(&mut self, est: u64, to: Option<usize>) -> String {
        let Some(current) = self.current else {
            return "No section is playing yet.".to_string();
        };
        let (at, at_bar, pushed) = self.change_position(est, current);
        let target = match to {
            Some(index) => Target::Section(index),
            None if current.index + 1 < self.plans.len() => Target::Section(current.index + 1),
            None => Target::End,
        };
        self.arm(target, at, at_bar);
        let (bars, beats) = self.sched.distance(est, at);
        // Where the switch lands, said in the section's own terms. The absolute bar index is no
        // use to a musician: it counts from whenever the engine was started, count-in included,
        // and a section that loops makes "bar 44 of a 41-bar arrangement" look like an error.
        let id = &self.plans[current.index].id;
        let len = current.bars.max(1) as u64;
        let rel = at_bar.saturating_sub(current.start_bar);
        let place = if rel % len == 0 {
            format!("at the end of pass {} of \"{id}\"", rel / len)
        } else {
            format!("at bar {} of \"{id}\" (pass {})", rel % len + 1, rel / len + 1)
        };
        let note = if pushed {
            " - after the take that is still recording; a take is never cut short"
        } else {
            ""
        };
        format!(
            "Switch to \"{}\" armed {place}, {}{note}.",
            self.target_label(target),
            lead_text(bars, beats)
        )
    }

    /// Schedule everything a target needs at `at` and remember it as armed.
    fn arm(&mut self, target: Target, at: u64, at_bar: u64) {
        let before = self.now();
        match target {
            Target::Section(index) => self.schedule_section(index, at, at_bar),
            Target::End => self.schedule_silence(at),
        }
        self.armed = Some(Armed {
            target,
            at,
            at_bar,
            before,
        });
    }

    /// Every track as the runner's picture has it at this moment, before anything new is armed.
    fn now(&self) -> Vec<Now> {
        self.tracks
            .iter()
            .map(|t| Now {
                sounding: t.sounding,
                take_end: t.take_end,
                stacking: t.stacking,
            })
            .collect()
    }

    /// The whole translation from target states to commands.
    fn schedule_section(&mut self, index: usize, start: u64, start_bar: u64) {
        let plan = self.plans[index].clone();
        self.sched.bars = plan.bars;
        let end = self.sched.timeline.bar_start(start_bar + plan.bars as u64);

        for track in 0..self.tracks.len() {
            let want = plan.want[track];
            let name = self.tracks[track].name.clone();
            if self.tracks[track].stacking && want != ScoreState::Stack {
                self.end_stack(track, start, want);
            }
            let clip = plan.clips[track];
            if self.restarts(track, want, clip, start_bar) {
                // The loop comes in from its top: re-anchored on this very sample, which every
                // overdub planned below then finds as its grid. `play`: the `StartPlay` follows
                // right away for the same sample, so the engine may sound it from there on its
                // read-ahead axis too (plugin and chain latency).
                self.out.push(Command::RestartLoop {
                    track,
                    at: start,
                    play: want == ScoreState::Play,
                });
                let t = &mut self.tracks[track];
                t.origin = start;
                t.origin_bar = start_bar;
            }
            match want {
                ScoreState::Record => {
                    // A take of its own length (`{state: record, bars: 2}`) ends early and the new
                    // loop plays for the rest of the section - its `StartPlay` comes with the take.
                    // The reader keeps it within the section.
                    let bars = clip.bars.unwrap_or(plan.bars).clamp(1, plan.bars);
                    let take_end = self.sched.timeline.bar_start(start_bar + bars as u64);
                    let scheduled = self.sched.record_at(track, &name, start, bars);
                    self.push(scheduled);
                    let t = &mut self.tracks[track];
                    t.origin = start;
                    t.origin_bar = start_bar;
                    t.len = take_end - start;
                    t.len_bars = bars;
                    t.layers = 1;
                    t.take_end = take_end;
                    t.take_end_bar = start_bar + bars as u64;
                    // The take's own `StartPlay` at `end` leaves the track playing, which is what a
                    // section that loops needs and what the next section then finds.
                    t.sounding = true;
                }
                ScoreState::Overdub => {
                    // A further layer belongs on the **track's own** grid, `origin + n * loop_len`,
                    // not on the bar line the section starts on. The two are the same number to
                    // within a sample or two, and that difference is exactly what would break: a
                    // layer that starts a sample early is a sample short at the end of the pass,
                    // and a `StartOverdub` a sample before the running take's `StopRecord` is
                    // refused as "Busy". See `schedule::Scheduler::loop_grid_at_or_after`.
                    let t = &self.tracks[track];
                    let (take_start, take_end, defines_loop) = if t.len == 0 {
                        // An overdub on a track without a loop is simply the first take, over the
                        // section - the engine treats `StartOverdub` on an empty track that way.
                        (start, end, true)
                    } else {
                        // The grid point that *is* this section's start, even when it lies a few
                        // samples before the bar line: `origin + n * len` and the bar line are
                        // rounded separately and drift apart by up to a sample per pass. Asking
                        // strictly "at or after `start`" would then skip to the *next* grid point
                        // and start the layer a whole loop late - the first pass of the section
                        // would be lost and a release pushed a pass further (seen in Henry's
                        // score: "in 14 bars" in bar 3 of an eight-bar section). Half a beat of
                        // slack is far more than the drift and far less than any real loop length,
                        // which is always whole bars.
                        let slack = (self.sched.timeline.samples_per_beat() / 2.0) as u64;
                        let at = Scheduler::loop_grid_at_or_after(
                            t.origin,
                            t.len,
                            start.saturating_sub(slack),
                        );
                        // One whole pass, unless the section is genuinely shorter than the loop.
                        // The comparison is in **bars**, for the same rounding reason.
                        let stop = if plan.bars >= t.len_bars { at + t.len } else { end };
                        (at, stop, false)
                    };
                    let layers = self.tracks[track].layers;
                    let scheduled =
                        self.sched
                            .overdub_at(track, &name, take_start, take_end, layers);
                    self.push(scheduled);
                    let take_end_bar = self.sched.timeline.bar_index_at(take_end);
                    let t = &mut self.tracks[track];
                    if defines_loop {
                        t.origin = take_start;
                        t.origin_bar = start_bar;
                        t.len = take_end - take_start;
                        t.len_bars = plan.bars;
                        t.layers = 1;
                    } else {
                        t.layers = t.layers.saturating_add(1);
                    }
                    t.take_end = take_end;
                    t.take_end_bar = take_end_bar;
                    t.sounding = true;
                }
                ScoreState::Stack if self.tracks[track].stacking => {
                    // stack -> stack: the latch simply keeps going.
                }
                ScoreState::Stack => {
                    // The latch goes on first, timed for the first pass, so that pass is judged
                    // like every later one; then the first pass itself, exactly as `record` (empty
                    // track - the section's `bars` become the loop) or `overdub` (on the track's own
                    // grid, see there) would schedule it. Every further pass is the engine's.
                    let t = &self.tracks[track];
                    let (take_start, take_end, defines_loop) = if t.len == 0 {
                        (start, end, true)
                    } else {
                        let slack = self.slack();
                        let at = Scheduler::loop_grid_at_or_after(
                            t.origin,
                            t.len,
                            start.saturating_sub(slack),
                        );
                        (at, at + t.len, false)
                    };
                    self.out.push(Command::LatchOverdub {
                        track,
                        on: true,
                        at: take_start,
                    });
                    let scheduled = if defines_loop {
                        self.sched.record_at(track, &name, start, plan.bars)
                    } else {
                        let layers = self.tracks[track].layers;
                        self.sched
                            .overdub_at(track, &name, take_start, take_end, layers)
                    };
                    self.push(scheduled);
                    let take_end_bar = self.bar_near(take_end);
                    let t = &mut self.tracks[track];
                    if defines_loop {
                        t.origin = take_start;
                        t.origin_bar = start_bar;
                        t.len = take_end - take_start;
                        t.len_bars = plan.bars;
                        t.layers = 1;
                    } else {
                        t.layers = t.layers.saturating_add(1);
                    }
                    t.take_end = take_end;
                    t.take_end_bar = take_end_bar;
                    t.sounding = true;
                    t.stacking = true;
                    t.stack_from = take_start;
                }
                ScoreState::Play => {
                    // The one case that must produce nothing: a track that is already sounding.
                    if !self.tracks[track].sounding && self.tracks[track].len > 0 {
                        let scheduled = self.sched.play_at(track, &name, start);
                        self.push(scheduled);
                        self.tracks[track].sounding = true;
                    }
                }
                ScoreState::Stop | ScoreState::HearThrough => {
                    if self.tracks[track].sounding {
                        let scheduled = self.sched.stop_play_at(track, &name, start);
                        self.push(scheduled);
                        self.tracks[track].sounding = false;
                    }
                }
            }
            self.tracks[track].target = want;
        }
    }

    /// Whether the section starting on bar `start_bar` re-anchors `track`'s loop - the rule of
    /// "Ein Loop, der einsetzt, beginnt vorn" (`docs/architektur.md`, section 10):
    ///
    /// * the track plays its loop in this section (`play`, or `overdub` / `stack` on a loop it
    ///   already has) and is not simply carrying on a running stack,
    /// * it comes **in** - it was not sounding in the section before - or the clip asks for it
    ///   with `restart: true`,
    /// * and the section does not begin on the loop's own grid anyway. Asked in bars: a loop is
    ///   always whole bars, and its origin always a section start. A score whose sections are
    ///   multiples of its loops therefore never sends a single re-anchoring - it plays exactly as
    ///   it did before the rule existed.
    fn restarts(&self, track: usize, want: ScoreState, clip: ClipOptions, start_bar: u64) -> bool {
        let t = &self.tracks[track];
        let plays_loop = matches!(
            want,
            ScoreState::Play | ScoreState::Overdub | ScoreState::Stack
        );
        if !plays_loop || t.len == 0 || t.len_bars == 0 || t.stacking {
            return false;
        }
        if t.sounding && !clip.restart {
            return false;
        }
        start_bar < t.origin_bar || (start_bar - t.origin_bar) % u64::from(t.len_bars) != 0
    }

    /// Everything silent at `at` - the end of the score, the click included.
    fn schedule_silence(&mut self, at: u64) {
        self.out.push(Command::ClickGate {
            at,
            open: false,
            bar1: 0,
        });
        for track in 0..self.tracks.len() {
            if self.tracks[track].stacking {
                self.end_stack(track, at, ScoreState::Stop);
            }
            if self.tracks[track].sounding {
                let name = self.tracks[track].name.clone();
                let scheduled = self.sched.stop_play_at(track, &name, at);
                self.push(scheduled);
                self.tracks[track].sounding = false;
            }
            self.tracks[track].target = ScoreState::Stop;
        }
    }
}

/// `in 5 bars`, `in 1 bar and 2 beats`, `now`.
fn lead_text(bars: u32, beats: u32) -> String {
    let beats_text = |b: u32| if b == 1 { "1 beat".to_string() } else { format!("{b} beats") };
    match (bars, beats) {
        (0, 0) => "now".to_string(),
        (0, b) => format!("in {}", beats_text(b)),
        (1, 0) => "in 1 bar".to_string(),
        (t, 0) => format!("in {t} bars"),
        (1, b) => format!("in 1 bar and {}", beats_text(b)),
        (t, b) => format!("in {t} bars and {}", beats_text(b)),
    }
}

/// What a score says about how its tracks are set up, as the untimed commands that put an engine
/// into that state: pan, the two bus assignments, the latency compensation - and monitoring off,
/// which is the state the runner's picture starts from (`hear_through` and the takes switch it on
/// at the section boundary).
///
/// Untimed on purpose: these are setup, not musical events, and they are only sent while every
/// track is empty and no score is playing - on load, and again after "Clear all" has put the
/// tracks back to the values they were *started* with.
pub fn setup_commands(score: &CompiledScore) -> Vec<Command> {
    use super::bus::TrackSource;
    let mut out = Vec::with_capacity(score.tracks.len() * 5);
    for (track, spec) in score.tracks.iter().enumerate() {
        out.push(Command::SetPan {
            track,
            pan: spec.pan,
        });
        out.push(Command::SetTrackSend {
            track,
            source: TrackSource::Loop,
            send: spec.loop_send(),
        });
        out.push(Command::SetTrackSend {
            track,
            source: TrackSource::Monitor,
            send: spec.monitor_send(),
        });
        out.push(Command::SetTrackLatency {
            track,
            latency: spec.track_latency(),
        });
        // An instrument track keeps the monitoring it was started with: heard, unless the score
        // says monitor: false (the host starts the engine that way). The engine would ignore
        // the command anyway.
        if !spec.is_instrument() {
            out.push(Command::SetMonitor { track, on: false });
        }
    }
    out
}

/// Check a score against the tracks an engine is **already** running with.
///
/// The engine's track layout - how many tracks there are, what they are called and which inputs
/// they listen on - is decided when the engine starts and never changed underneath a running
/// session. Loading a score with a different layout is therefore refused rather than applied:
/// silently rewiring an input mid-song would send the next take to the wrong microphone, and a
/// track that disappears would take a recorded loop with it. The way out is the honest one - stop
/// the engine and start it with this score, which is what the `score` subcommand does anyway.
///
/// Pan and latency are deliberately **not** part of the comparison: both change where a track sits
/// or how far it is shifted, neither changes what a command means, and both are adjustable at
/// runtime by design.
pub fn check_tracks(score: &CompiledScore, running: &[TrackDef]) -> Result<(), String> {
    if score.tracks.len() != running.len() {
        return Err(format!(
            "The score has {} tracks, the running engine {}. Tracks cannot be rebuilt while the \
             engine runs - stop the engine, then start it with this score.",
            score.tracks.len(),
            running.len()
        ));
    }
    for (want, have) in score.tracks.iter().zip(running) {
        if want.name != have.name {
            return Err(format!(
                "Track {} is called \"{}\" in the score but \"{}\" in the running engine. \
                 Stop the engine, then start it with this score.",
                want.index + 1,
                want.name,
                have.name
            ));
        }
        if want.track_input() != have.input {
            return Err(format!(
                "Track \"{}\" listens to input {} in the score but to {} in the running engine. \
                 Changing an input while the engine runs would record the next take from the \
                 wrong source - stop the engine first.",
                want.name,
                want.track_input().label(),
                have.input.label()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod stop_tests;
