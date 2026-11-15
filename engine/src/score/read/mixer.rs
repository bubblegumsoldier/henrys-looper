//! Reading `mixer:` - see `score/mixer.rs` for the format. Same rule as the rest of the reader:
//! every problem is reported, a malformed value is skipped rather than guessed.

use super::super::error::{suggest_or_list};
use super::super::map::OrderedMap;
use super::super::mixer::{
    BAND_KIND_NAMES, BandLine, BusLevels, CompLine, DB_MAX, DELAY_NOTE_NAMES, DelayLine,
    HighPassLine, MixerSource, PRESET_NAMES, ReverbLine, SwitchLine, TrackMixSource,
};
use super::super::yaml::Node;
use super::Reader;

pub(super) const MIXER_FIELDS: [&str; 2] = ["buses", "tracks"];
pub(super) const BUS_FIELDS: [&str; 4] = ["main", "monitor", "click", "inputs"];
pub(super) const TRACK_MIX_FIELDS: [&str; 12] = [
    "gain", "mute", "preset", "bypass", "high_pass", "eq", "band1", "band2", "band3", "comp",
    "delay", "reverb",
];

impl Reader {
    pub(super) fn read_mixer(&mut self, node: &Node<'_>, track_names: &[&str]) -> Option<MixerSource> {
        let entries = self.as_map(node, "'mixer'")?;
        let fields = self.index_fields(entries, &MIXER_FIELDS, &[], "'mixer'");
        let buses = fields.get("buses").and_then(|n| self.read_bus_levels(n));
        let mut tracks = OrderedMap::new();
        if let Some(n) = fields.get("tracks")
            && let Some(entries) = self.as_map(n, "'mixer.tracks'")
        {
            for entry in entries {
                let Some(name) = entry.key.as_str() else {
                    self.error(
                        Some(entry.key.pos),
                        format!("Track names must be text, found: {}.", entry.key.describe()),
                    );
                    continue;
                };
                if !track_names.contains(&name) {
                    self.error_with(
                        Some(entry.key.pos),
                        format!("'mixer.tracks' names the track '{name}', which the score does not have."),
                        suggest_or_list(name, track_names),
                    );
                    continue;
                }
                if tracks.contains_key(name) {
                    self.error_with(
                        Some(entry.key.pos),
                        format!("Track '{name}' appears more than once in 'mixer.tracks'."),
                        "Each track may appear only once.",
                    );
                    continue;
                }
                if let Some(mix) = self.read_track_mix(name, &entry.value) {
                    tracks.insert(name, mix);
                }
            }
        }
        Some(MixerSource { buses, tracks })
    }

    /// A level in dB, up to [`DB_MAX`]. Anything at or below -100 dB is silence, `-.inf` too.
    fn read_db(&mut self, node: &Node<'_>, what: &str) -> Option<f64> {
        let value = self.as_number(node, what)?;
        if value.is_nan() || value > DB_MAX {
            self.error_with(
                Some(node.pos),
                format!("{what} must be a level in dB up to +{DB_MAX}, found: {value}."),
                "0 is unity gain, -6 is half as loud, -100 or less is silence.",
            );
            return None;
        }
        Some(value)
    }

    fn read_range(&mut self, node: &Node<'_>, what: &str, min: f64, max: f64) -> Option<f64> {
        let value = self.as_number(node, what)?;
        if !(min..=max).contains(&value) {
            self.error_with(
                Some(node.pos),
                format!("{what} must be between {min} and {max}, found: {value}."),
                format!("Allowed: {min} to {max}."),
            );
            return None;
        }
        Some(value)
    }

    fn read_word(&mut self, node: &Node<'_>, what: &str, words: &[&str]) -> Option<String> {
        let text = self.as_str(node, what)?.to_string();
        if !words.contains(&text.as_str()) {
            self.error_with(
                Some(node.pos),
                format!("{what}: '{text}' is not one of the allowed words."),
                suggest_or_list(&text, words),
            );
            return None;
        }
        Some(text)
    }

    fn read_bus_levels(&mut self, node: &Node<'_>) -> Option<BusLevels> {
        let entries = self.as_map(node, "'mixer.buses'")?;
        let fields = self.index_fields(entries, &BUS_FIELDS, &[], "'mixer.buses'");
        let mut get = |name: &str| {
            fields
                .get(name)
                .and_then(|n| self.read_db(n, &format!("'{name}' in 'mixer.buses'")))
        };
        Some(BusLevels {
            main: get("main"),
            monitor: get("monitor"),
            click: get("click"),
            inputs: get("inputs"),
        })
    }

    fn read_track_mix(&mut self, name: &str, node: &Node<'_>) -> Option<TrackMixSource> {
        let what = format!("the mixer entry of '{name}'");
        let entries = self.as_map(node, &what)?;
        let fields = self.index_fields(entries, &TRACK_MIX_FIELDS, &[], &what);
        let mut out = TrackMixSource::default();
        let field = |f: &str| format!("'{f}' of {what}");
        if let Some(n) = fields.get("gain") {
            out.gain = self.read_db(n, &field("gain"));
        }
        if let Some(n) = fields.get("mute") {
            out.mute = self.as_bool(n, &field("mute"));
        }
        if let Some(n) = fields.get("preset") {
            out.preset = self.read_word(n, &field("preset"), &PRESET_NAMES);
        }
        if let Some(n) = fields.get("bypass") {
            out.bypass = self.as_bool(n, &field("bypass"));
        }
        if let Some(n) = fields.get("high_pass") {
            out.high_pass = self.read_line(n, &field("high_pass"), &["on", "hz"], |r, key, v, w| {
                let mut line = HighPassLine::default();
                match key {
                    "on" => line.on = r.as_bool(v, w),
                    _ => line.hz = r.read_range(v, w, 20.0, 1_000.0),
                }
                line
            }, |a, b| HighPassLine { on: a.on.or(b.on), hz: a.hz.or(b.hz) });
        }
        if let Some(n) = fields.get("eq") {
            out.eq = self.read_line(n, &field("eq"), &["on"], |r, _, v, w| SwitchLine { on: r.as_bool(v, w) }, |a, b| SwitchLine { on: a.on.or(b.on) });
        }
        for (band, slot) in ["band1", "band2", "band3"].into_iter().zip([&mut out.band1, &mut out.band2, &mut out.band3]) {
            if let Some(n) = fields.get(band) {
                *slot = self.read_line(n, &field(band), &["kind", "hz", "q", "gain"], |r, key, v, w| {
                    let mut line = BandLine::default();
                    match key {
                        "kind" => line.kind = r.read_word(v, w, &BAND_KIND_NAMES),
                        "hz" => line.hz = r.read_range(v, w, 20.0, 20_000.0),
                        "q" => line.q = r.read_range(v, w, 0.1, 12.0),
                        _ => line.gain = r.read_range(v, w, -24.0, 24.0),
                    }
                    line
                }, |a, b| BandLine {
                    kind: a.kind.or(b.kind),
                    hz: a.hz.or(b.hz),
                    q: a.q.or(b.q),
                    gain: a.gain.or(b.gain),
                });
            }
        }
        if let Some(n) = fields.get("comp") {
            out.comp = self.read_line(
                n,
                &field("comp"),
                &["on", "threshold", "ratio", "attack", "release", "knee", "makeup"],
                |r, key, v, w| {
                    let mut line = CompLine::default();
                    match key {
                        "on" => line.on = r.as_bool(v, w),
                        "threshold" => line.threshold = r.read_range(v, w, -60.0, 0.0),
                        "ratio" => line.ratio = r.read_range(v, w, 1.0, 20.0),
                        "attack" => line.attack = r.read_range(v, w, 0.1, 200.0),
                        "release" => line.release = r.read_range(v, w, 5.0, 2_000.0),
                        "knee" => line.knee = r.read_range(v, w, 0.0, 24.0),
                        _ => line.makeup = r.read_range(v, w, -24.0, 24.0),
                    }
                    line
                },
                |a, b| CompLine {
                    on: a.on.or(b.on),
                    threshold: a.threshold.or(b.threshold),
                    ratio: a.ratio.or(b.ratio),
                    attack: a.attack.or(b.attack),
                    release: a.release.or(b.release),
                    knee: a.knee.or(b.knee),
                    makeup: a.makeup.or(b.makeup),
                },
            );
        }
        if let Some(n) = fields.get("delay") {
            out.delay = self.read_line(n, &field("delay"), &["on", "note", "feedback", "mix"], |r, key, v, w| {
                let mut line = DelayLine::default();
                match key {
                    "on" => line.on = r.as_bool(v, w),
                    "note" => line.note = r.read_word(v, w, &DELAY_NOTE_NAMES),
                    "feedback" => line.feedback = r.read_range(v, w, 0.0, 0.95),
                    _ => line.mix = r.read_range(v, w, 0.0, 1.0),
                }
                line
            }, |a, b| DelayLine {
                on: a.on.or(b.on),
                note: a.note.or(b.note),
                feedback: a.feedback.or(b.feedback),
                mix: a.mix.or(b.mix),
            });
        }
        if let Some(n) = fields.get("reverb") {
            out.reverb = self.read_line(n, &field("reverb"), &["on", "size", "damping", "mix"], |r, key, v, w| {
                let mut line = ReverbLine::default();
                match key {
                    "on" => line.on = r.as_bool(v, w),
                    "size" => line.size = r.read_range(v, w, 0.0, 1.0),
                    "damping" => line.damping = r.read_range(v, w, 0.0, 1.0),
                    _ => line.mix = r.read_range(v, w, 0.0, 1.0),
                }
                line
            }, |a, b| ReverbLine {
                on: a.on.or(b.on),
                size: a.size.or(b.size),
                damping: a.damping.or(b.damping),
                mix: a.mix.or(b.mix),
            });
        }
        Some(out)
    }

    /// One effect line: a mapping of known fields, each read by `field` into a line of its own and
    /// merged into one with `merge`.
    fn read_line<T: Default>(
        &mut self,
        node: &Node<'_>,
        what: &str,
        known: &[&str],
        mut field: impl FnMut(&mut Reader, &str, &Node<'_>, &str) -> T,
        merge: impl Fn(T, T) -> T,
    ) -> Option<T> {
        let entries = self.as_map(node, what)?;
        let fields = self.index_fields(entries, known, &[], what);
        let mut out = T::default();
        // In the order of `known`, so a message list does not depend on hash order.
        for key in known {
            if let Some(value) = fields.get(key) {
                let label = format!("'{key}' in {what}");
                let one = field(self, key, value, &label);
                out = merge(out, one);
            }
        }
        Some(out)
    }
}
