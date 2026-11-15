//! The bridge against a MIDI system in memory ([`super::fake::FakeMidi`]) and a profile folder of
//! its own per test: learn, autosave, auto-connect, hot-plug, several devices, status.

use std::path::PathBuf;

use looper_engine::midi::{Context, MidiAction, Resolution, SessionShape, TrackLayout};

use super::fake::FakeMidi;
use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("looper-bridge-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("Testordner");
    dir
}

fn bridge(name: &str) -> (MidiBridge, FakeMidi, PathBuf) {
    let midi = FakeMidi::default();
    let dir = temp_dir(name);
    (MidiBridge::with(Box::new(midi.clone()), &dir), midi, dir)
}

fn press(note: u8) -> MidiEvent {
    MidiEvent::NoteOn {
        channel: 1,
        note,
        velocity: 100,
    }
}

fn knob(number: u8, value: u8) -> MidiEvent {
    MidiEvent::ControlChange {
        channel: 1,
        controller: number,
        value,
    }
}

/// Everything queued, fed through the learn mode the way `host::pump_midi` does.
fn pump_learn(bridge: &mut MidiBridge) -> Vec<MidiEventReport> {
    let mut events = Vec::new();
    bridge.drain(&mut events);
    events
        .into_iter()
        .filter_map(|(link, event)| bridge.feed_learn(link, &event))
        .collect()
}

/// Everything queued, resolved against a session with these tracks.
fn pump_resolve(bridge: &mut MidiBridge, tracks: &[&str]) -> Vec<(String, Resolution)> {
    let layout = TrackLayout::new(tracks.iter().copied());
    let ctx = Context::bare(&layout);
    let mut events = Vec::new();
    bridge.drain(&mut events);
    events
        .into_iter()
        .map(|(link, event)| {
            let name = bridge.links[link].port.name().to_string();
            (name, bridge.resolve(link, &event, &ctx))
        })
        .collect()
}

fn device<'a>(view: &'a MidiView, name: &str) -> &'a MidiDeviceView {
    view.devices
        .iter()
        .find(|d| d.device == name)
        .unwrap_or_else(|| panic!("kein Geraet {name}: {:?}", view.devices))
}

/// **The owner's workflow**: plug in, connect once, switch on learn, click, press - and the
/// binding is in the file without anything called "save".
#[test]
fn a_learned_binding_is_on_disk_at_once_without_saving() {
    let (mut bridge, midi, dir) = bridge("autosave");
    midi.plug("Testpad");
    bridge.scan();
    assert!(!bridge.is_open(), "an unknown device is not taken by itself");

    let view = bridge.open("Testpad").expect("verbunden");
    assert!(view.connected);
    let path = dir.join("testpad.yaml");
    assert!(path.exists(), "connecting by hand remembers the device at once");

    bridge.arm("track.1.record").expect("Adresse");
    midi.send("Testpad", press(36));
    midi.send(
        "Testpad",
        MidiEvent::NoteOff {
            channel: 1,
            note: 36,
            velocity: 0,
        },
    );
    let reports = pump_learn(&mut bridge);
    assert_eq!(reports.len(), 1, "the note off of the same pad does not learn again");
    assert_eq!(reports[0].outcome, MidiOutcome::Learned);
    assert_eq!(reports[0].device.as_deref(), Some("Testpad"));
    assert!(!bridge.is_learning());

    let text = std::fs::read_to_string(&path).expect("Profil");
    assert!(text.contains("ch1.note36: {target: track.1.record}"), "{text}");
    let view = bridge.view();
    assert!(!view.dirty);
    assert_eq!(view.save_error, None);
    assert_eq!(view.bindings.len(), 1);

    // Unbinding is saved just the same.
    bridge.unbind("ch1.note36", None).expect("geloest");
    let text = std::fs::read_to_string(&path).expect("Profil");
    assert!(!text.contains("note36"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// After a restart of the app the device is known and comes by itself - and so after a replug.
#[test]
fn a_known_device_connects_by_itself_at_start_and_after_a_replug() {
    let (mut bridge, midi, dir) = bridge("autoconnect");
    std::fs::write(
        dir.join("mpd218.yaml"),
        "# von Hand\ndevice: MPD218\nbindings:\n  ch10.note36: {target: track.1.record}\n  \
         ch10.note37: {target: track.1.play}\n",
    )
    .unwrap();
    midi.plug("MPD218");
    let log = bridge.scan();
    assert!(bridge.is_open(), "{log:?}");
    assert!(log[0].contains("connected"), "{log:?}");
    let view = bridge.view();
    assert_eq!(device(&view, "MPD218").state, DeviceState::Connected);
    assert_eq!(device(&view, "MPD218").active, 2);

    midi.unplug("MPD218");
    let log = bridge.scan();
    assert!(log[0].contains("disconnected - 2 bindings inactive"), "{log:?}");
    let view = bridge.view();
    let mpd = device(&view, "MPD218");
    assert_eq!(mpd.state, DeviceState::Missing);
    assert_eq!((mpd.active, mpd.inactive), (0, 2));
    assert!(mpd.last_seen.is_some(), "\"last seen\" survives the unplug");
    assert!(mpd.summary.contains("not plugged in - 2 bindings inactive"), "{}", mpd.summary);
    assert!(view.bindings.iter().all(|b| b.state == BindingState::DeviceMissing));

    midi.plug("MPD218");
    bridge.scan();
    assert_eq!(device(&bridge.view(), "MPD218").state, DeviceState::Connected);
    assert_eq!(midi.opened(), vec!["MPD218", "MPD218"]);

    // A fresh bridge - the app started again - connects without being asked.
    let mut again = MidiBridge::with(Box::new(midi.clone()), &dir);
    again.scan();
    assert!(again.is_open());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A foot pedal and a pad controller both send `ch1.note36`. Each must do its own job.
#[test]
fn two_devices_at_once_keep_their_own_bindings_and_learn_into_their_own_profile() {
    let (mut bridge, midi, dir) = bridge("two");
    std::fs::write(
        dir.join("pedal.yaml"),
        "device: FCB Pedal\nbindings:\n  ch1.note36: {target: track.1.play}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pads.yaml"),
        "device: Pads\nbindings:\n  ch1.note36: {target: track.2.record}\n",
    )
    .unwrap();
    midi.plug("FCB Pedal");
    midi.plug("Pads");
    bridge.scan();
    assert_eq!(bridge.view().devices.iter().filter(|d| d.connected).count(), 2);

    midi.send("FCB Pedal", press(36));
    midi.send("Pads", press(36));
    let mut results = pump_resolve(&mut bridge, &["a", "b"]);
    results.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(results[0].1, Resolution::Action(MidiAction::Play { track: 0 }));
    assert_eq!(results[1].1, Resolution::Action(MidiAction::Record { track: 1 }));

    // Learning from the pedal writes the pedal's file and leaves the pads' alone.
    bridge.arm("transport.next").expect("Adresse");
    midi.send("FCB Pedal", knob(64, 127));
    pump_learn(&mut bridge);
    let pedal = std::fs::read_to_string(dir.join("pedal.yaml")).unwrap();
    let pads = std::fs::read_to_string(dir.join("pads.yaml")).unwrap();
    assert!(pedal.contains("ch1.cc64: {target: transport.next}"), "{pedal}");
    assert!(!pads.contains("cc64"), "{pads}");

    // The same id on two devices: unbinding has to say which.
    let err = bridge.unbind("ch1.note36", None).expect_err("zweideutig");
    assert!(err.contains("several devices"), "{err}");
    bridge.unbind("ch1.note36", Some("Pads")).expect("eindeutig");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Track 5 in a two-track session, a section the score does not have, a score that takes a pad
/// over: each says why it does not work - as data, for any user interface.
#[test]
fn every_binding_says_whether_it_works_in_this_session_and_why_not() {
    let (mut bridge, midi, dir) = bridge("status");
    std::fs::write(
        dir.join("pads.yaml"),
        "device: Pads\nbindings:\n  ch1.note36: {target: track.1.record}\n  \
         ch1.note40: {target: track.5.record}\n  ch1.note41: {target: global.click}\n",
    )
    .unwrap();
    midi.plug("Pads");
    bridge.scan();

    // No engine yet: nothing is called broken.
    let view = bridge.view();
    assert!(view.bindings.iter().all(|b| b.state == BindingState::Unchecked));
    assert_eq!(device(&view, "Pads").inactive, 0);

    bridge.set_shape(SessionShape {
        tracks: Some(vec!["stimme".into(), "bass".into()]),
        sections: None,
    });
    let view = bridge.view();
    let five = view.bindings.iter().find(|b| b.id == "ch1.note40").unwrap();
    assert_eq!(five.state, BindingState::TargetMissing);
    assert!(five.reason.as_deref().unwrap().contains("2 tracks"));
    assert_eq!(device(&view, "Pads").inactive, 1);
    assert!(device(&view, "Pads").summary.contains("2 of 3"), "{}", device(&view, "Pads").summary);

    // A score binding by track name gets the address the button carries, and takes pad 36 over.
    let yaml = "title: T\nbpm: 100\ntracks:\n  stimme: {input: 1}\n  bass: {input: 2}\nmidi:\n  \
                track.bass.record: {note: 36}\nsections:\n  - id: eins\n    bars: 4\n    \
                tracks:\n      stimme: record\n";
    let score = looper_engine::score::compile_score(yaml).expect("Partitur");
    let notes = bridge.set_score(Some(&score)).expect("Bindungen");
    assert_eq!(notes.len(), 1, "{notes:?}");
    let view = bridge.view();
    let from_score = view.bindings.iter().find(|b| b.from_score).unwrap();
    assert_eq!(from_score.address, "track.bass.record");
    assert_eq!(from_score.resolved_address.as_deref(), Some("track.2.record"));
    assert_eq!(from_score.state, BindingState::Active);
    let taken = view
        .bindings
        .iter()
        .find(|b| b.id == "ch1.note36" && !b.from_score)
        .unwrap();
    assert_eq!(taken.state, BindingState::Overridden);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Disconnect" by hand sticks - until the device is unplugged or connected by hand again.
#[test]
fn a_manual_disconnect_is_respected_until_replug() {
    let (mut bridge, midi, dir) = bridge("suppress");
    midi.plug("Pads");
    bridge.open("Pads").expect("verbunden");
    bridge.close(Some("Pads"));
    bridge.scan();
    assert!(!bridge.is_open(), "not reconnected behind the user's back");
    assert_eq!(device(&bridge.view(), "Pads").state, DeviceState::Disconnected);

    midi.unplug("Pads");
    bridge.scan();
    midi.plug("Pads");
    bridge.scan();
    assert!(bridge.is_open(), "a replug means: use it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// On Windows another program may hold the device; that is a state, not a log flood.
#[test]
fn a_busy_device_is_shown_as_unavailable_and_reported_once() {
    let (mut bridge, midi, dir) = bridge("busy");
    std::fs::write(dir.join("pads.yaml"), "device: Pads\nbindings: {}\n").unwrap();
    midi.plug("Pads");
    midi.hold("Pads");
    let first = bridge.scan();
    assert_eq!(first.len(), 1, "{first:?}");
    bridge.last_scan = None;
    assert!(bridge.scan().is_empty(), "the same failure is logged once");
    let view = bridge.view();
    let pads = device(&view, "Pads");
    assert_eq!(pads.state, DeviceState::Unavailable);
    assert!(pads.error.as_deref().unwrap().contains("ALLOCATED"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A profile with a typo is reported and never overwritten - not even by learning into it.
#[test]
fn a_broken_profile_is_left_alone_and_learning_into_it_is_refused() {
    let (mut bridge, midi, dir) = bridge("broken");
    let path = dir.join("pads.yaml");
    let broken = "# Notizen\ndevice: Pads\nbindings:\n  ch1.note36: {target: track.1.recrd}\n";
    std::fs::write(&path, broken).unwrap();
    midi.plug("Pads");
    bridge.scan();
    let view = bridge.view();
    assert_eq!(device(&view, "Pads").state, DeviceState::Error);

    bridge.arm("track.1.play").expect("Adresse");
    midi.send("Pads", press(37));
    let reports = pump_learn(&mut bridge);
    assert_eq!(reports[0].outcome, MidiOutcome::Refused);
    assert!(reports[0].note.as_deref().unwrap().contains("has errors"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);

    // Fixed by hand while the app runs: picked up on the next scan.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, "device: Pads\nbindings:\n  ch1.note36: {target: track.1.record}\n").unwrap();
    bridge.scan();
    let view = bridge.view();
    assert_eq!(device(&view, "Pads").state, DeviceState::Connected);
    assert_eq!(view.bindings.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// mode, takeover, min, max - editable from the list, checked, and saved.
#[test]
fn the_options_of_a_binding_are_edited_checked_and_saved() {
    let (mut bridge, midi, dir) = bridge("edit");
    midi.plug("Pads");
    bridge.open("Pads").unwrap();
    for (address, event) in [("track.1.monitor", press(38)), ("track.1.fx.reverb.mix", knob(3, 10)), ("track.1.record", press(36))] {
        bridge.arm(address).unwrap();
        midi.send("Pads", event);
        pump_learn(&mut bridge);
    }
    let options = |mode: Option<&str>, takeover: Option<&str>, min, max| BindingOptions {
        mode: mode.map(str::to_string),
        takeover: takeover.map(str::to_string),
        min,
        max,
    };
    let view = bridge.edit("ch1.note38", None, &options(Some("momentary"), None, None, None)).unwrap();
    let monitor = view.bindings.iter().find(|b| b.id == "ch1.note38").unwrap();
    assert_eq!(monitor.mode.as_deref(), Some("momentary"));
    assert_eq!(monitor.modes.len(), 4);
    bridge
        .edit("ch1.cc3", None, &options(None, Some("jump"), Some(0.0), Some(0.4)))
        .unwrap();
    let err = bridge
        .edit("ch1.note36", None, &options(Some("toggle"), None, None, None))
        .expect_err("ein Trigger schaltet nicht um");
    assert!(err.contains("press"), "{err}");

    let text = std::fs::read_to_string(dir.join("pads.yaml")).unwrap();
    assert!(text.contains("ch1.note38: {target: track.1.monitor, mode: momentary}"), "{text}");
    assert!(text.contains("ch1.cc3: {target: track.1.fx.reverb.mix, takeover: jump, min: 0, max: 0.4}"), "{text}");
    assert!(text.contains("ch1.note36: {target: track.1.record}"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_address_is_refused_with_the_way_to_the_list() {
    let (mut bridge, _midi, dir) = bridge("typo");
    let err = bridge.arm("track.1.recrd").expect_err("Tippfehler");
    assert!(err.contains("midi targets"), "{err}");
    assert!(!bridge.is_learning());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A knob's stream is coalesced and waits for its window; a pad goes out at once.
#[test]
fn a_knob_sweep_coalesces_and_a_pad_is_emitted_at_once() {
    let (mut bridge, midi, dir) = bridge("coalesce");
    midi.plug("Pads");
    bridge.open("Pads").unwrap();
    bridge.take_feed();
    for value in 0..40u8 {
        bridge.report(0, &knob(3, value), MidiOutcome::Unbound, None, None);
    }
    assert_eq!(bridge.pending.len(), 1, "forty steps of a knob are one entry");
    assert!(bridge.take_feed().is_none(), "a knob waits for its window");
    bridge.report(0, &press(36), MidiOutcome::Unbound, None, None);
    let feed = bridge.take_feed().expect("a pad goes out at once");
    assert_eq!(feed.events.len(), 2);
    assert_eq!(feed.events[0].value, 39);
    assert_eq!(feed.events[1].port.as_deref(), Some("Pads"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_kind_of_control_has_a_short_name_for_the_badge() {
    assert_eq!(MidiId::note(1, 36).short(), "Pad 36");
    assert_eq!(MidiId::cc(1, 3).short(), "CC 3");
    assert_eq!(MidiId::bend(1).short(), "Bend");
    assert_eq!(MidiId::note(10, 36).to_string(), "ch10.note36");
}

/// The view serialises to the documented JSON shape - the contract a new user interface builds on.
#[test]
fn the_view_serialises_with_the_documented_fields() {
    let (mut bridge, midi, dir) = bridge("json");
    midi.plug("Pads");
    bridge.open("Pads").unwrap();
    bridge.arm("track.1.pan").unwrap();
    midi.send("Pads", knob(10, 64));
    pump_learn(&mut bridge);
    let json = serde_json::to_value(bridge.view()).expect("JSON");
    for field in ["devices", "ports", "bindings", "session", "dir", "save_error", "learning"] {
        assert!(json.get(field).is_some(), "{field} fehlt: {json}");
    }
    let binding = &json["bindings"][0];
    for field in ["state", "reason", "resolved_address", "device", "control", "takeover", "range_min"] {
        assert!(binding.get(field).is_some(), "{field} fehlt: {binding}");
    }
    assert_eq!(binding["state"], "unchecked");
    assert_eq!(json["devices"][0]["state"], "connected");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real thing on macOS: a CoreMIDI virtual source plays the controller. It appears after the
/// bridge has started (hot-plug), is connected by itself because its profile is known, delivers a
/// pad press through the driver, and disappears again. No hardware involved - this proves the
/// CoreMIDI path including the run-loop pump, not a particular device.
#[cfg(target_os = "macos")]
#[test]
fn coremidi_virtual_controller_is_found_connected_heard_and_lost() {
    use midir::MidiOutput;
    use midir::os::unix::VirtualOutput;
    use std::time::{Duration, Instant};

    let dir = temp_dir("coremidi");
    let name = format!("Looper Testpad {}", std::process::id());
    std::fs::write(
        dir.join("testpad.yaml"),
        format!("device: \"{name}\"\nbindings:\n  ch1.note36: {{target: track.1.record}}\n"),
    )
    .unwrap();
    let mut bridge = MidiBridge::with(Box::new(SystemMidi), &dir);
    bridge.scan();
    assert!(!bridge.is_open(), "the virtual port does not exist yet");

    let wait = |bridge: &mut MidiBridge, want: bool| {
        let start = Instant::now();
        while bridge.is_open() != want && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
            bridge.scan();
        }
        bridge.is_open() == want
    };

    let output = MidiOutput::new("Looper Test").expect("CoreMIDI");
    let mut port = output.create_virtual(&name).expect("virtueller Port");
    assert!(wait(&mut bridge, true), "hot-plugged port not connected: {:?}", bridge.view().ports);
    assert_eq!(device(&bridge.view(), &name).state, DeviceState::Connected);

    port.send(&[0x90, 36, 100]).expect("senden");
    let layout = TrackLayout::new(["a"]);
    let ctx = Context::bare(&layout);
    let start = Instant::now();
    let mut got = Vec::new();
    while got.is_empty() && start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
        bridge.drain(&mut got);
    }
    assert_eq!(got.len(), 1, "the pad press arrives through CoreMIDI");
    let (link, event) = got[0];
    assert_eq!(
        bridge.resolve(link, &event, &ctx),
        Resolution::Action(MidiAction::Record { track: 0 })
    );

    drop(port);
    assert!(wait(&mut bridge, false), "unplugged port still connected");
    assert_eq!(device(&bridge.view(), &name).state, DeviceState::Missing);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The foot pedal is gone, a different one is on the desk: its bindings move over in one step. A
/// control the new device already uses for something else stays where it was, and says so.
#[test]
fn the_bindings_of_a_missing_device_move_to_another_one() {
    let (mut bridge, midi, dir) = bridge("move");
    std::fs::write(
        dir.join("old.yaml"),
        "device: Old Pedal\nbindings:\n  ch1.note60: {target: transport.next}\n  \
         ch1.note61: {target: global.click}\n  ch1.note62: {target: track.1.record}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("new.yaml"),
        "# meine Notizen\ndevice: New Pedal\nbindings:\n  ch1.note62: {target: track.2.play}\n",
    )
    .unwrap();
    midi.plug("New Pedal");
    bridge.scan();
    assert_eq!(device(&bridge.view(), "Old Pedal").state, DeviceState::Missing);

    let action = ProfileAction::Move {
        device: "Old Pedal".into(),
        to: "New Pedal".into(),
    };
    let view = bridge.manage(&action).expect("moved");
    let message = view.message.clone().unwrap();
    assert!(message.contains("Moved 2 bindings"), "{message}");
    assert!(message.contains("Pad 62"), "the conflict is named: {message}");
    assert_eq!(device(&view, "New Pedal").bindings, 3);
    assert_eq!(device(&view, "Old Pedal").bindings, 1, "the conflicting one stays");

    let new = std::fs::read_to_string(dir.join("new.yaml")).unwrap();
    assert!(new.starts_with("# meine Notizen"), "{new}");
    assert!(new.contains("ch1.note60: {target: transport.next}"), "{new}");
    assert!(new.contains("ch1.note62: {target: track.2.play}"), "unchanged: {new}");
    let old = std::fs::read_to_string(dir.join("old.yaml")).unwrap();
    assert!(!old.contains("note60") && old.contains("note62"), "{old}");

    // The moved control works through the new device right away.
    midi.send("New Pedal", press(61));
    let results = pump_resolve(&mut bridge, &["a", "b"]);
    assert!(
        !matches!(results[0].1, Resolution::Unbound),
        "global.click answers on the new pedal: {results:?}"
    );

    // Nothing left that could move -> refused with a sentence.
    let err = bridge.manage(&action).expect_err("only the conflict is left");
    assert!(err.contains("Nothing moved"), "{err}");
    let same = ProfileAction::Move {
        device: "New Pedal".into(),
        to: "New Pedal".into(),
    };
    assert!(bridge.manage(&same).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A name of its own in the app and "last seen" both survive a restart; the profile file is not
/// touched by either.
#[test]
fn name_and_last_seen_are_remembered_across_a_restart() {
    let (mut bridge, midi, dir) = bridge("meta");
    let text = "device: FS-4\nbindings:\n  ch1.note60: {target: transport.next}\n";
    std::fs::write(dir.join("fs-4.yaml"), text).unwrap();
    midi.plug("FS-4");
    bridge.scan();
    let view = bridge
        .manage(&ProfileAction::Rename {
            device: "FS-4".into(),
            name: "  Foot switch  ".into(),
        })
        .expect("renamed");
    assert_eq!(device(&view, "FS-4").name, "Foot switch");
    midi.unplug("FS-4");
    bridge.scan();
    let seen = device(&bridge.view(), "FS-4").last_seen.expect("seen");
    assert_eq!(std::fs::read_to_string(dir.join("fs-4.yaml")).unwrap(), text);

    // The app starts again, the pedal is still not there.
    let mut again = MidiBridge::with(Box::new(midi.clone()), &dir);
    again.scan();
    let view = again.view();
    let fs4 = device(&view, "FS-4");
    assert_eq!(fs4.state, DeviceState::Missing);
    assert_eq!(fs4.last_seen, Some(seen), "last seen comes from devices.json");
    assert_eq!(fs4.name, "Foot switch");

    // An empty name gives the device its own name back.
    let view = again
        .manage(&ProfileAction::Rename {
            device: "FS-4".into(),
            name: String::new(),
        })
        .expect("reset");
    assert_eq!(device(&view, "FS-4").name, "FS-4");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Deleting a profile disconnects the device, keeps the file aside and does not connect it again.
#[test]
fn a_deleted_profile_is_kept_aside_and_its_device_is_no_longer_taken() {
    let (mut bridge, midi, dir) = bridge("delete");
    std::fs::write(
        dir.join("pads.yaml"),
        "device: Pads\nbindings:\n  ch1.note36: {target: track.1.record}\n",
    )
    .unwrap();
    midi.plug("Pads");
    bridge.scan();
    assert!(bridge.is_open());
    let view = bridge
        .manage(&ProfileAction::Delete {
            device: "Pads".into(),
        })
        .expect("deleted");
    assert!(view.devices.is_empty());
    assert!(view.bindings.is_empty());
    assert!(!bridge.is_open());
    assert!(!dir.join("pads.yaml").exists());
    assert!(dir.join("pads.yaml.deleted").exists(), "nothing is destroyed");
    assert!(view.ports.iter().any(|p| p.name == "Pads" && p.profile.is_none()));
    bridge.scan();
    assert!(!bridge.is_open(), "an unknown device is not taken by itself");
    let err = bridge
        .manage(&ProfileAction::Delete {
            device: "Pads".into(),
        })
        .expect_err("gone");
    assert!(err.contains("no MIDI profile"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
