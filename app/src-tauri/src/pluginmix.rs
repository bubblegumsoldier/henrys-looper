//! Which plugins sit on which track, as the score's `mixer:` section sees it: the instrument of an
//! instrument track and the effect plugins of its insert slots, **by plugin id only**.
//!
//! ```text
//!  instruments::Manager ─publish_instruments─┐
//!  plugins::Manager ─────publish_inserts─────┼─► SNAPSHOT ◄── Session::mixer_state / mixer_take
//!  Mensch (Kommando, MIDI) ─touch_*──────────┘        │        (Host-Thread, liest nur)
//!                                                       │
//!  Session::queue_mix (Laden, Undo, Tippen) ─apply─► Hauptthread: instruments/plugins apply_score
//! ```
//!
//! The plugins live on the main thread, the mix is answered on the host thread. Rather than asking
//! across threads for every fader move, both managers publish a small snapshot here whenever their
//! slots change, and the host reads it. A human change (picking an instrument, loading, moving,
//! removing or bypassing an insert, by mouse or MIDI) is *touched*: it moves [`rev`], which the
//! status carries as part of `mixer_rev`, and the arrangement then asks for exactly those tracks -
//! the same way as for a fader (`docs/architektur.md`, section 15). Applying a score is no news and
//! touches nothing.
//!
//! # A plugin the score names and this computer does not have
//!
//! The track runs without it (silent for an instrument, one insert fewer), a notice says so, and
//! the entry stays in the file: the snapshot keeps reporting what the score asked for (`missing`)
//! until a human puts something else on that track. Otherwise the next fader move would write the
//! track back without the plugin and the score would lose it on a computer that merely lacks it.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use looper_engine::score::mixer::{InsertLine, MixerSource};

#[derive(Default)]
struct Snapshot {
    /// Track name -> instrument plugin id, as loaded.
    instruments: HashMap<String, String>,
    /// Track name -> insert plugins in slot order, as loaded.
    inserts: HashMap<String, Vec<InsertLine>>,
    /// What a score asked for and could not be loaded here, kept until a human changes the track.
    missing_instrument: HashMap<String, String>,
    missing_inserts: HashMap<String, Vec<InsertLine>>,
    /// Tracks whose inserts a human has changed in this run - an emptied rack is then written as
    /// `inserts: []` instead of being left out (which would mean "keep what you have").
    inserts_touched: HashSet<String>,
    /// Tracks a human changed since the arrangement last asked.
    touched: Vec<String>,
    rev: u64,
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Snapshot) -> T) -> T {
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Snapshot::default))
}

/// The instrument manager's slots: (track, plugin id).
pub fn publish_instruments(slots: Vec<(String, String)>) {
    with(|s| s.instruments = slots.into_iter().collect());
}

/// The insert manager's track slots: track -> plugins in slot order.
pub fn publish_inserts(tracks: HashMap<String, Vec<InsertLine>>) {
    with(|s| s.inserts = tracks);
}

fn touch(s: &mut Snapshot, track: &str) {
    s.rev += 1;
    if !s.touched.iter().any(|t| t == track) {
        s.touched.push(track.to_string());
    }
}

/// A human put an instrument on `track` or took it off.
pub fn touch_instrument(track: &str) {
    with(|s| {
        s.missing_instrument.remove(track);
        touch(s, track);
    });
}

/// A human changed the insert slots of `track`.
pub fn touch_inserts(track: &str) {
    with(|s| {
        s.missing_inserts.remove(track);
        s.inserts_touched.insert(track.to_string());
        touch(s, track);
    });
}

/// A score asked for `id` on `track` and it could not be loaded.
pub fn instrument_missing(track: &str, id: &str) {
    with(|s| {
        s.missing_instrument.insert(track.to_string(), id.to_string());
    });
}

/// A score asked for these inserts on `track` and at least one could not be loaded.
pub fn inserts_missing(track: &str, wanted: Vec<InsertLine>) {
    with(|s| {
        s.missing_inserts.insert(track.to_string(), wanted);
    });
}

/// Whether `id` on `track` is already known to be missing - an undo or a second apply of the
/// same section then says nothing new.
pub fn instrument_is_missing(track: &str, id: &str) -> bool {
    with(|s| s.missing_instrument.get(track).is_some_and(|m| m == id))
}

/// The same for a track's insert list.
pub fn inserts_are_missing(track: &str, wanted: &[InsertLine]) -> bool {
    with(|s| s.missing_inserts.get(track).is_some_and(|m| m == wanted))
}

/// The score's request for `track` was met after all.
pub fn found(track: &str, instrument: bool) {
    with(|s| {
        if instrument {
            s.missing_instrument.remove(track);
        } else {
            s.missing_inserts.remove(track);
        }
    });
}

/// Goes up with every human change; part of the status's `mixer_rev`.
pub fn rev() -> u64 {
    with(|s| s.rev)
}

/// The tracks a human changed since the last call, and forget them.
pub fn take() -> Vec<String> {
    with(|s| std::mem::take(&mut s.touched))
}

/// The plugin fields of `track`'s mixer entry: its instrument (only on an instrument track) and
/// its inserts (when it has any, or a human emptied them).
pub fn entry(track: &str, instrument_track: bool) -> (Option<String>, Option<Vec<InsertLine>>) {
    with(|s| {
        let instrument = instrument_track
            .then(|| {
                s.missing_instrument
                    .get(track)
                    .or_else(|| s.instruments.get(track))
                    .cloned()
            })
            .flatten();
        let inserts = match s.missing_inserts.get(track) {
            Some(wanted) => Some(wanted.clone()),
            None => {
                let have = s.inserts.get(track).cloned().unwrap_or_default();
                (!have.is_empty() || s.inserts_touched.contains(track)).then_some(have)
            }
        };
        (instrument, inserts)
    })
}

/// The instrument plugin of every instrument track in `tracks`, for a new score.
pub fn instruments_of(tracks: &[String]) -> Vec<(String, String)> {
    with(|s| {
        tracks
            .iter()
            .filter_map(|t| s.instruments.get(t).map(|id| (t.clone(), id.clone())))
            .collect()
    })
}

/// Put the plugins a `mixer:` section names onto the tracks, by name. Only entries that name
/// plugins count; an absent field keeps what the track has. Runs on the main thread, later.
/// `instrument_tracks` are the names the running engine plays by instrument.
pub fn apply(mixer: &MixerSource, tracks: &[String], instrument_tracks: &[String]) {
    for (name, entry) in mixer.tracks.iter() {
        if !tracks.iter().any(|t| t == name) {
            continue;
        }
        if let Some(id) = &entry.instrument
            && instrument_tracks.iter().any(|t| t == name)
        {
            crate::instruments::apply_score(name.to_string(), id.clone());
        }
        if let Some(inserts) = &entry.inserts {
            crate::plugins::apply_score(name.to_string(), inserts.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str) -> InsertLine {
        InsertLine {
            plugin: id.to_string(),
            bypass: None,
            before: None,
        }
    }

    /// One test for the whole snapshot: it is a process-wide static, and tests run in parallel.
    #[test]
    fn the_snapshot_reports_what_is_loaded_what_is_missing_and_what_a_human_touched() {
        publish_instruments(vec![("keys".into(), "au:aumu:dls :appl".into())]);
        publish_inserts(HashMap::from([("keys".to_string(), vec![line("au:aufx:dely:appl")])]));
        let before = rev();

        // Loaded plugins, by track; an audio track has no instrument field.
        assert_eq!(
            entry("keys", true),
            (Some("au:aumu:dls :appl".into()), Some(vec![line("au:aufx:dely:appl")]))
        );
        assert_eq!(entry("keys", false).0, None);
        // Nothing loaded and nothing touched: both fields stay out ("keep what you have").
        assert_eq!(entry("voice", false), (None, None));

        // A score asked for something this computer lacks: the entry keeps saying so.
        instrument_missing("pads", "vst3:NOTHERE");
        inserts_missing("voice", vec![line("vst3:GONE")]);
        assert_eq!(entry("pads", true).0.as_deref(), Some("vst3:NOTHERE"));
        assert_eq!(entry("voice", false).1, Some(vec![line("vst3:GONE")]));
        assert_eq!(rev(), before, "applying a score is no news");

        // A human acts on the track: the request is forgotten, the change is news.
        touch_instrument("pads");
        assert_eq!(entry("pads", true).0, None);
        touch_inserts("voice");
        assert_eq!(entry("voice", false).1, Some(vec![]), "emptied by hand: written as []");
        assert_eq!(rev(), before + 2);
        let touched = take();
        assert!(touched.contains(&"pads".to_string()) && touched.contains(&"voice".to_string()));
        assert!(take().is_empty());
        assert_eq!(
            instruments_of(&["keys".into(), "voice".into()]),
            vec![("keys".to_string(), "au:aumu:dls :appl".to_string())]
        );
    }
}
