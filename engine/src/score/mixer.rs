//! The `mixer:` section of a score: channel faders, mutes, effect chains and bus levels, kept with
//! the song they belong to.
//!
//! ```yaml
//! mixer:
//!   buses: {main: 0, monitor: -6, click: -12, inputs: 0}   # dB
//!   tracks:
//!     voice: {gain: -3.5, preset: voice}
//!     guitar:
//!       gain: -1
//!       mute: true
//!       preset: piezo_guitar
//!       comp: {threshold: -12}
//!       reverb: {on: false}
//! ```
//!
//! # What it says, and what it does not
//!
//! * **Levels are in dB**, rounded to a tenth, because that is how a fader is read. `-100` and
//!   anything below is silence. A track goes from silence to +12 dB (`MAX_TRACK_GAIN`), a bus,
//!   the click and the monitored inputs to +12 dB as well.
//! * **An effect chain is a preset plus what differs from it.** `preset:` names one of the three
//!   ready-made chains (`dry` when omitted); every effect line (`high_pass`, `eq`, `band1`..`band3`,
//!   `comp`, `delay`, `reverb`) overrides only the fields it names. A chain with no effect switched
//!   on is bypassed unless `bypass: false` says otherwise - which is exactly what the presets do,
//!   so `preset: voice` alone means "the voice chain, as the button loads it".
//! * **Solo is not part of it.** A solo forgotten in a saved song silences every other track the
//!   next time it is loaded - on stage, in front of an audience. Mute is kept: a muted track is a
//!   decision about the arrangement, and it shows on its strip.
//! * **Pan, bus routing and monitoring are not here** - they already are the track's own fields
//!   under `tracks:`.
//! * **Inside a written track entry, an omitted field is the default** (0 dB, not muted, dry). A
//!   track the section does not name keeps whatever it has.
//!
//! The same per-track model doubles as the app's local store for the free mode (no score loaded),
//! keyed by track name - see `mixstore.rs` in the app.
//!
//! Nothing here runs on the audio thread; [`track_commands`] and [`bus_commands`] produce the
//! ordinary untimed commands a mouse click would send.

use serde::{Deserialize, Serialize};

use super::map::OrderedMap;
use crate::engine::bus::{Bus, MAX_BUS_GAIN};
use crate::engine::command::{Command, MAX_LEVEL_GAIN, Status, TrackStatus};
use crate::engine::fx::{
    BandKind, BandSettings, ChainSettings, DelayNote, EQ_BANDS, FxParam, FxPreset, FxSlot,
};
use crate::engine::track::MAX_TRACK_GAIN;

/// Everything at or below this many dB is written and read as silence.
pub const DB_FLOOR: f64 = -100.0;
/// Highest level any fader of the section may name.
pub const DB_MAX: f64 = 12.0;

/// The preset words of the format, in the order the buttons stand in.
pub const PRESET_NAMES: [&str; 3] = ["dry", "voice", "piezo_guitar"];
pub const BAND_KIND_NAMES: [&str; 3] = ["peak", "low_shelf", "high_shelf"];
pub const DELAY_NOTE_NAMES: [&str; 4] = ["quarter", "dotted_eighth", "eighth", "triplet_eighth"];

// ---------------------------------------------------------------------------------------------
// The model, as written
// ---------------------------------------------------------------------------------------------

/// `buses:` - the levels of the two output buses, the click and the monitored inputs, in dB.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BusLevels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<f64>,
    /// The volume of every monitored live input (`global.monitor_gain`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<f64>,
}

impl BusLevels {
    pub fn is_empty(&self) -> bool {
        self.main.is_none() && self.monitor.is_none() && self.click.is_none() && self.inputs.is_none()
    }

    /// What the engine has right now, every field written.
    pub fn from_status(status: &Status) -> Self {
        Self {
            main: Some(gain_to_db(status.bus_gain(Bus::Main))),
            monitor: Some(gain_to_db(status.bus_gain(Bus::Monitor))),
            click: Some(gain_to_db(status.click_gain)),
            inputs: Some(gain_to_db(status.monitor_gain)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HighPassLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<f64>,
}

/// `eq: {on: true}` - the bands themselves are `band1`..`band3`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SwitchLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BandLine {
    /// `peak`, `low_shelf` or `high_shelf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<f64>,
    /// dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knee: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub makeup: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DelayLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// `quarter`, `dotted_eighth`, `eighth` or `triplet_eighth`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReverbLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f64>,
}

/// One track's entry under `mixer.tracks`. Every field is optional; see the module comment for
/// what an omitted one means.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackMixSource {
    /// Channel fader in dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    /// `dry`, `voice` or `piezo_guitar`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The whole chain out of the signal path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_pass: Option<HighPassLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<SwitchLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band1: Option<BandLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band2: Option<BandLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band3: Option<BandLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp: Option<CompLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<DelayLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb: Option<ReverbLine>,
    /// The instrument plugin of an instrument track, by plugin id (`au:aumu:dls :appl`). Only the
    /// id: the plugin's state stays on this computer (`instruments.json`, by track name).
    /// **Absent means "keep what the track has"** - unlike the fader fields, because a score
    /// written before this field existed must not take anybody's piano away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    /// The effect plugins of the track's insert slots, in slot order, by plugin id. Like
    /// `instrument`: only ids, the states stay in `plugin-rack.json`; absent keeps what the track
    /// has, `inserts: []` empties the slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserts: Option<Vec<InsertLine>>,
}

/// One effect plugin of `inserts: [...]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsertLine {
    pub plugin: String,
    /// Absent means `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass: Option<bool>,
}

/// The whole section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixerSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buses: Option<BusLevels>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub tracks: OrderedMap<TrackMixSource>,
}

// ---------------------------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------------------------

/// dB as written -> linear gain, capped at `max`. At or below [`DB_FLOOR`] is silence.
pub fn db_to_gain(db: f64, max: f32) -> f32 {
    if db.is_nan() || db <= DB_FLOOR {
        return 0.0;
    }
    (10f64.powf(db / 20.0) as f32).clamp(0.0, max)
}

/// Linear gain -> dB as written: a tenth of a dB, [`DB_FLOOR`] for silence.
pub fn gain_to_db(gain: f32) -> f64 {
    if gain.is_nan() || gain <= 0.0 {
        return DB_FLOOR;
    }
    let db = 20.0 * f64::from(gain).log10();
    if db <= DB_FLOOR {
        return DB_FLOOR;
    }
    let rounded = (db * 10.0).round() / 10.0;
    // `-0.0` would be written as `-0`.
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// An `f32` the engine reports, as the `f64` that prints the same: `0.62`, not
/// `0.6200000047683716`. Read back and narrowed to `f32` it is the engine's number again.
pub fn clean(value: f32) -> f64 {
    format!("{value}").parse().unwrap_or(f64::from(value))
}

pub fn preset_name(preset: FxPreset) -> &'static str {
    match preset {
        FxPreset::Dry | FxPreset::Custom => "dry",
        FxPreset::Voice => "voice",
        FxPreset::PiezoGuitar => "piezo_guitar",
    }
}

pub fn parse_preset(text: &str) -> Option<FxPreset> {
    match text {
        "dry" => Some(FxPreset::Dry),
        "voice" => Some(FxPreset::Voice),
        "piezo_guitar" => Some(FxPreset::PiezoGuitar),
        _ => None,
    }
}

pub fn band_kind_name(kind: BandKind) -> &'static str {
    match kind {
        BandKind::Peak => "peak",
        BandKind::LowShelf => "low_shelf",
        BandKind::HighShelf => "high_shelf",
    }
}

pub fn parse_band_kind(text: &str) -> Option<BandKind> {
    match text {
        "peak" => Some(BandKind::Peak),
        "low_shelf" => Some(BandKind::LowShelf),
        "high_shelf" => Some(BandKind::HighShelf),
        _ => None,
    }
}

pub fn parse_delay_note(text: &str) -> Option<DelayNote> {
    DELAY_NOTE_NAMES
        .contains(&text)
        .then(|| DelayNote::parse(text))
        .flatten()
}

// ---------------------------------------------------------------------------------------------
// Written -> what the engine should have
// ---------------------------------------------------------------------------------------------

/// One track's mix, resolved: what the engine is to be put into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackMix {
    pub gain: f32,
    pub mute: bool,
    /// The preset the chain is built from.
    pub base: FxPreset,
    pub bypass: bool,
    pub settings: ChainSettings,
}

impl TrackMix {
    /// The preset the engine will report once this is applied: the base, or `custom` once
    /// anything differs from it.
    pub fn shown_preset(&self) -> FxPreset {
        if self.settings == self.base.settings() {
            self.base
        } else {
            FxPreset::Custom
        }
    }
}

fn set<T: Copy>(target: &mut T, value: Option<T>) {
    if let Some(v) = value {
        *target = v;
    }
}

fn f(value: Option<f64>) -> Option<f32> {
    value.map(|v| v as f32)
}

impl TrackMixSource {
    pub fn base(&self) -> FxPreset {
        self.preset
            .as_deref()
            .and_then(parse_preset)
            .unwrap_or(FxPreset::Dry)
    }

    /// True when the entry names plugins (instrument or inserts).
    pub fn has_plugins(&self) -> bool {
        self.instrument.is_some() || self.inserts.is_some()
    }

    /// The same entry without the plugin fields - what the free mode's `mixer.json` keeps (the
    /// plugins have their own files there).
    #[must_use]
    pub fn without_plugins(mut self) -> Self {
        self.instrument = None;
        self.inserts = None;
        self
    }

    /// True when the entry says anything about the effect chain beyond its preset.
    pub fn has_effect_lines(&self) -> bool {
        self.high_pass.is_some()
            || self.eq.is_some()
            || self.band1.is_some()
            || self.band2.is_some()
            || self.band3.is_some()
            || self.comp.is_some()
            || self.delay.is_some()
            || self.reverb.is_some()
    }

    pub fn resolve(&self) -> TrackMix {
        let base = self.base();
        let mut s = base.settings();
        if let Some(line) = &self.high_pass {
            set(&mut s.enabled[FxSlot::HighPass.index()], line.on);
            set(&mut s.high_pass_hz, f(line.hz));
        }
        if let Some(line) = &self.eq {
            set(&mut s.enabled[FxSlot::Eq.index()], line.on);
        }
        for (band, line) in [&self.band1, &self.band2, &self.band3].into_iter().enumerate() {
            if let Some(line) = line {
                let b = &mut s.bands[band];
                set(&mut b.kind, line.kind.as_deref().and_then(parse_band_kind));
                set(&mut b.hz, f(line.hz));
                set(&mut b.q, f(line.q));
                set(&mut b.gain_db, f(line.gain));
            }
        }
        if let Some(line) = &self.comp {
            set(&mut s.enabled[FxSlot::Comp.index()], line.on);
            set(&mut s.comp.threshold_db, f(line.threshold));
            set(&mut s.comp.ratio, f(line.ratio));
            set(&mut s.comp.attack_ms, f(line.attack));
            set(&mut s.comp.release_ms, f(line.release));
            set(&mut s.comp.knee_db, f(line.knee));
            set(&mut s.comp.makeup_db, f(line.makeup));
        }
        if let Some(line) = &self.delay {
            set(&mut s.enabled[FxSlot::Delay.index()], line.on);
            set(&mut s.delay_note, line.note.as_deref().and_then(parse_delay_note));
            set(&mut s.delay_feedback, f(line.feedback));
            set(&mut s.delay_mix, f(line.mix));
        }
        if let Some(line) = &self.reverb {
            set(&mut s.enabled[FxSlot::Reverb.index()], line.on);
            set(&mut s.reverb_size, f(line.size));
            set(&mut s.reverb_damping, f(line.damping));
            set(&mut s.reverb_mix, f(line.mix));
        }
        TrackMix {
            gain: self
                .gain
                .map(|db| db_to_gain(db, MAX_TRACK_GAIN))
                .unwrap_or(1.0),
            mute: self.mute.unwrap_or(false),
            base,
            bypass: self.bypass.unwrap_or_else(|| default_bypass(&s)),
            settings: s,
        }
    }

    /// The entry that describes a track's current state: the fader always, mute when on, and the
    /// chain as the preset it differs least from plus the lines that differ.
    ///
    /// `shown` is the preset the engine reports; it only breaks a tie, so a chain loaded as
    /// "voice" is written as `preset: voice` even where another reading would be as short.
    pub fn from_state(
        gain: f32,
        mute: bool,
        bypass: bool,
        shown: FxPreset,
        settings: &ChainSettings,
    ) -> Self {
        let mut best: Option<(usize, FxPreset, TrackMixSource)> = None;
        // The shown preset first, so it wins a tie.
        let mut order: Vec<FxPreset> = vec![shown];
        order.extend(FxPreset::loadable().into_iter().filter(|p| *p != shown));
        for base in order.into_iter().filter(|p| *p != FxPreset::Custom) {
            let entry = describe_chain(base, settings);
            let lines = entry.line_count();
            if best.as_ref().is_none_or(|(n, _, _)| lines < *n) {
                best = Some((lines, base, entry));
            }
        }
        let (_, base, mut entry) = best.expect("there is always a loadable preset");
        if base != FxPreset::Dry {
            entry.preset = Some(preset_name(base).to_string());
        }
        if bypass != default_bypass(settings) {
            entry.bypass = Some(bypass);
        }
        entry.gain = Some(gain_to_db(gain));
        if mute {
            entry.mute = Some(true);
        }
        entry
    }

    /// The same, from a track's status snapshot.
    pub fn from_status(track: &TrackStatus) -> Self {
        Self::from_state(
            track.gain,
            track.muted,
            track.fx.bypass,
            track.fx.preset,
            &track.fx.settings,
        )
    }

    fn line_count(&self) -> usize {
        [
            self.high_pass.is_some(),
            self.eq.is_some(),
            self.band1.is_some(),
            self.band2.is_some(),
            self.band3.is_some(),
            self.comp.is_some(),
            self.delay.is_some(),
            self.reverb.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }
}

/// A chain in which nothing is switched on is out of the signal path - as "dry" loads it.
fn default_bypass(settings: &ChainSettings) -> bool {
    !settings.enabled.iter().any(|on| *on)
}

/// The effect lines that turn `base` into `settings`, each with only the fields that differ.
fn describe_chain(base: FxPreset, settings: &ChainSettings) -> TrackMixSource {
    let b = base.settings();
    let s = settings;
    let on = |slot: FxSlot| {
        let i = slot.index();
        (s.enabled[i] != b.enabled[i]).then_some(s.enabled[i])
    };
    let num = |now: f32, was: f32| (now != was).then(|| clean(now));
    let mut out = TrackMixSource::default();

    let high_pass = HighPassLine {
        on: on(FxSlot::HighPass),
        hz: num(s.high_pass_hz, b.high_pass_hz),
    };
    out.high_pass = (high_pass != HighPassLine::default()).then_some(high_pass);
    let eq = SwitchLine { on: on(FxSlot::Eq) };
    out.eq = (eq != SwitchLine::default()).then_some(eq);
    let bands: Vec<Option<BandLine>> = (0..EQ_BANDS)
        .map(|i| {
            let (now, was): (BandSettings, BandSettings) = (s.bands[i], b.bands[i]);
            let line = BandLine {
                kind: (now.kind != was.kind).then(|| band_kind_name(now.kind).to_string()),
                hz: num(now.hz, was.hz),
                q: num(now.q, was.q),
                gain: num(now.gain_db, was.gain_db),
            };
            (line != BandLine::default()).then_some(line)
        })
        .collect();
    out.band1 = bands[0].clone();
    out.band2 = bands[1].clone();
    out.band3 = bands[2].clone();
    let comp = CompLine {
        on: on(FxSlot::Comp),
        threshold: num(s.comp.threshold_db, b.comp.threshold_db),
        ratio: num(s.comp.ratio, b.comp.ratio),
        attack: num(s.comp.attack_ms, b.comp.attack_ms),
        release: num(s.comp.release_ms, b.comp.release_ms),
        knee: num(s.comp.knee_db, b.comp.knee_db),
        makeup: num(s.comp.makeup_db, b.comp.makeup_db),
    };
    out.comp = (comp != CompLine::default()).then_some(comp);
    let delay = DelayLine {
        on: on(FxSlot::Delay),
        note: (s.delay_note != b.delay_note).then(|| s.delay_note.name().to_string()),
        feedback: num(s.delay_feedback, b.delay_feedback),
        mix: num(s.delay_mix, b.delay_mix),
    };
    out.delay = (delay != DelayLine::default()).then_some(delay);
    let reverb = ReverbLine {
        on: on(FxSlot::Reverb),
        size: num(s.reverb_size, b.reverb_size),
        damping: num(s.reverb_damping, b.reverb_damping),
        mix: num(s.reverb_mix, b.reverb_mix),
    };
    out.reverb = (reverb != ReverbLine::default()).then_some(reverb);
    out
}

// ---------------------------------------------------------------------------------------------
// What the engine has to be sent
// ---------------------------------------------------------------------------------------------

/// The commands that put track `track` into `target`, given what it reports now.
///
/// Fader and mute are always sent (two commands, and a fader never has to be compared). The chain
/// is left alone when it already is what is asked for - reloading it would reset its filters for
/// nothing. Otherwise its preset is loaded and only the knobs that differ from it are turned, so
/// the engine ends up reporting the same preset name [`TrackMix::shown_preset`] predicts.
pub fn track_commands(track: usize, target: &TrackMix, current: &TrackStatus) -> Vec<Command> {
    let mut out = vec![
        Command::SetTrackGain {
            track,
            gain: target.gain,
        },
        Command::SetTrackMute {
            track,
            on: target.mute,
        },
    ];
    if current.fx.bypass == target.bypass && current.fx.settings == target.settings {
        return out;
    }
    let base = target.base.settings();
    let s = &target.settings;
    out.push(Command::LoadFxPreset {
        track,
        preset: target.base,
    });
    for slot in FxSlot::all() {
        let i = slot.index();
        if s.enabled[i] != base.enabled[i] {
            out.push(Command::SetFxEnabled {
                track,
                slot,
                on: s.enabled[i],
            });
        }
    }
    let mut param = |param: FxParam| out.push(Command::SetFxParam { track, param });
    if s.high_pass_hz != base.high_pass_hz {
        param(FxParam::HighPassHz(s.high_pass_hz));
    }
    for band in 0..EQ_BANDS {
        let (now, was) = (s.bands[band], base.bands[band]);
        if now.kind != was.kind {
            param(FxParam::BandKind {
                band,
                kind: now.kind,
            });
        }
        if now.hz != was.hz {
            param(FxParam::BandHz { band, hz: now.hz });
        }
        if now.q != was.q {
            param(FxParam::BandQ { band, q: now.q });
        }
        if now.gain_db != was.gain_db {
            param(FxParam::BandGainDb {
                band,
                db: now.gain_db,
            });
        }
    }
    let (c, bc) = (s.comp, base.comp);
    if c.threshold_db != bc.threshold_db {
        param(FxParam::CompThresholdDb(c.threshold_db));
    }
    if c.ratio != bc.ratio {
        param(FxParam::CompRatio(c.ratio));
    }
    if c.attack_ms != bc.attack_ms {
        param(FxParam::CompAttackMs(c.attack_ms));
    }
    if c.release_ms != bc.release_ms {
        param(FxParam::CompReleaseMs(c.release_ms));
    }
    if c.knee_db != bc.knee_db {
        param(FxParam::CompKneeDb(c.knee_db));
    }
    if c.makeup_db != bc.makeup_db {
        param(FxParam::CompMakeupDb(c.makeup_db));
    }
    if s.delay_note != base.delay_note {
        param(FxParam::DelayNote(s.delay_note));
    }
    if s.delay_feedback != base.delay_feedback {
        param(FxParam::DelayFeedback(s.delay_feedback));
    }
    if s.delay_mix != base.delay_mix {
        param(FxParam::DelayMix(s.delay_mix));
    }
    if s.reverb_size != base.reverb_size {
        param(FxParam::ReverbSize(s.reverb_size));
    }
    if s.reverb_damping != base.reverb_damping {
        param(FxParam::ReverbDamping(s.reverb_damping));
    }
    if s.reverb_mix != base.reverb_mix {
        param(FxParam::ReverbMix(s.reverb_mix));
    }
    if target.bypass != target.base.bypasses() {
        out.push(Command::SetFxBypass {
            track,
            on: target.bypass,
        });
    }
    out
}

/// The commands for the levels `buses:` names; the ones it leaves out are not touched.
pub fn bus_commands(levels: &BusLevels) -> Vec<Command> {
    let mut out = Vec::new();
    for (bus, db) in [(Bus::Main, levels.main), (Bus::Monitor, levels.monitor)] {
        if let Some(db) = db {
            out.push(Command::SetBusGain {
                bus,
                gain: db_to_gain(db, MAX_BUS_GAIN),
            });
        }
    }
    if let Some(db) = levels.click {
        out.push(Command::SetClickGain {
            gain: db_to_gain(db, MAX_LEVEL_GAIN),
        });
    }
    if let Some(db) = levels.inputs {
        out.push(Command::SetMonitorGain {
            gain: db_to_gain(db, MAX_LEVEL_GAIN),
        });
    }
    out
}

/// Every command for a whole section, onto tracks addressed by name. `names` is the engine's
/// track list in order, `current` its newest status per track (shorter or empty is fine: a track
/// without one is taken to be in its starting state). Entries naming a track the engine does not
/// have are skipped - a score is compiled against its own track list, a stored free-mode mix may
/// well name tracks that are not there today.
pub fn mixer_commands(mixer: &MixerSource, names: &[String], current: &[TrackStatus]) -> Vec<Command> {
    let mut out = Vec::new();
    for (name, entry) in mixer.tracks.iter() {
        let Some(index) = names.iter().position(|n| n == name) else {
            continue;
        };
        let now = current.get(index).copied().unwrap_or_default();
        out.extend(track_commands(index, &entry.resolve(), &now));
    }
    if let Some(levels) = &mixer.buses {
        out.extend(bus_commands(levels));
    }
    out
}

#[cfg(test)]
mod tests;
