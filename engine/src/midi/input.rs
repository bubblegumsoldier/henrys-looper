//! The one part that needs hardware: finding MIDI ports and opening one.
//!
//! # Windows hands a MIDI device out exclusively
//!
//! `midiInOpen` fails with `MMSYSERR_ALLOCATED` when another program already holds the port, and
//! that is not an exotic case here - it is the normal one. If Ableton Live is running with the
//! MPD218 as a control surface, or another instance of this program is open, or a MIDI monitoring
//! tool is still in the tray, the device is gone and there is nothing to be done about it from
//! this side. So that failure gets its own sentence naming the likely culprit, rather than
//! "Fehler beim Oeffnen".
//!
//! # The callback is not the audio thread, and is treated as if it were
//!
//! MIDI callbacks come from a driver thread of the operating system, not from the ASIO callback,
//! so a slow one cannot produce a dropout. It can produce something almost as bad: MIDI messages
//! arriving late or out of order, which on a looper means a take that starts on the wrong beat. So
//! the rules of `docs/architektur.md` section 5 are followed here anyway - the callback decodes
//! three bytes and pushes into a lock-free ring buffer ([`rtrb`], the same one the audio thread
//! uses), and the control thread does everything else.
//!
//! A full queue drops the event and counts it. 1024 events is about eight seconds of a knob being
//! swept continuously; a control thread that has not drained it in eight seconds has a problem the
//! MIDI input cannot fix.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiInputPort};
use rtrb::{Consumer, Producer, RingBuffer};

use super::event::{MidiDecoder, MidiEvent};

/// Events buffered between the driver callback and the control thread.
const QUEUE: usize = 1024;
/// Name this program registers itself under. Some hosts show it.
const CLIENT: &str = "Henrys Looper";

/// One MIDI input port, as the system lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    /// Position in the system's list. What `--midi-device 1` means.
    pub index: usize,
    pub name: String,
}

/// One decoded event with the driver's timestamp in microseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedEvent {
    /// Microseconds since an unspecified point that does not move while the connection is open.
    pub micros: u64,
    pub event: MidiEvent,
}

/// Let the operating system tell this process about devices that came or went.
///
/// # Why this exists: CoreMIDI only learns about hot-plugging through a run loop
///
/// On macOS a process sees MIDI endpoints appear and disappear through notifications that CoreMIDI
/// delivers to the **run loop of the thread that created the first MIDI client**. The host thread
/// that does all MIDI work here has no run loop of its own, so without this the port list is frozen
/// at the state of the first call: a controller plugged in later never shows up, and one pulled out
/// stays in the list as an endpoint without a name. (Measured with a virtual port from a second
/// process: no new port in sixteen polls without the pump, the right list from the next poll with
/// it.) Running the run loop for zero seconds handles what is queued and returns at once.
///
/// Call it on the MIDI thread before [`list_ports`]. Elsewhere it does nothing: Windows' winmm and
/// ALSA enumerate afresh on every call.
pub fn refresh_system() {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::c_void;
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            static kCFRunLoopDefaultMode: *const c_void;
            fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, after_one: u8) -> i32;
        }
        /// `kCFRunLoopRunHandledSource`: one source was handled and there may be more.
        const HANDLED_SOURCE: i32 = 4;
        // Bounded, so a flood of notifications cannot keep the host thread here.
        for _ in 0..64 {
            // SAFETY: plain C call with a constant the framework exports; every thread gets a run
            // loop on first use, and a zero timeout never blocks.
            let result = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 1) };
            if result != HANDLED_SOURCE {
                break;
            }
        }
    }
}

/// Every MIDI input port. Opens nothing, so it is safe at any time - including while Ableton holds
/// the device.
///
/// An endpoint without a name is left out: on macOS that is what a just-unplugged device looks like
/// until the system has caught up, and it is nothing anybody could connect to.
pub fn list_ports() -> Result<Vec<PortInfo>, String> {
    refresh_system();
    let input = MidiInput::new(CLIENT).map_err(init_error)?;
    Ok(input
        .ports()
        .iter()
        .enumerate()
        .map(|(index, port)| PortInfo {
            index,
            name: input
                .port_name(port)
                .unwrap_or_else(|_| "<Name nicht lesbar>".to_string()),
        })
        .filter(|port| !port.name.trim().is_empty())
        .collect())
}

/// What the callback writes into.
struct Sink {
    decoder: MidiDecoder,
    tx: Producer<TimedEvent>,
    dropped: Arc<AtomicU64>,
}

/// An open MIDI input. Closes when dropped.
pub struct MidiIn {
    /// Kept alive: dropping it closes the port.
    _connection: MidiInputConnection<Sink>,
    rx: Consumer<TimedEvent>,
    dropped: Arc<AtomicU64>,
    name: String,
}

impl MidiIn {
    /// The port's name as the system reports it - what a profile's `device:` is matched against.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The next event, or `None` when the queue is empty. Never blocks.
    pub fn try_recv(&mut self) -> Option<TimedEvent> {
        self.rx.pop().ok()
    }

    /// Events lost because the control thread did not drain the queue. Zero, always, unless
    /// something is wrong.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Open a port by number or by (part of) its name.
///
/// `selector` is either a decimal index as [`list_ports`] reports it, or a piece of the port's
/// name, matched case-insensitively - "mpd" finds "2- MPD218". An empty selector takes the only
/// port if there is exactly one, and otherwise asks rather than guessing: opening the wrong device
/// takes it away from whatever was using it.
pub fn open(selector: &str) -> Result<MidiIn, String> {
    let mut input = MidiInput::new(CLIENT).map_err(init_error)?;
    // Clock, active sensing and SysEx are not buttons or knobs, and clock alone is 24 messages per
    // beat. Filtering them in the driver keeps them out of the callback entirely.
    input.ignore(Ignore::All);

    let ports = input.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|port| {
            input
                .port_name(port)
                .unwrap_or_else(|_| "<Name nicht lesbar>".to_string())
        })
        .collect();
    if ports.is_empty() {
        return Err(
            "Es ist kein MIDI-Eingang da. Haengt der Controller am Rechner, und hat ihn nicht \
             schon ein anderes Programm (z. B. Ableton als Control Surface) fuer sich?"
                .to_string(),
        );
    }

    let index = pick(selector, &names)?;
    connect(input, &ports[index], names[index].clone())
}

/// Open the port with exactly this name - what the automatic connection uses, because a name it
/// got from [`list_ports`] a moment ago must not match two ports as a fragment.
pub fn open_named(name: &str) -> Result<MidiIn, String> {
    let mut input = MidiInput::new(CLIENT).map_err(init_error)?;
    input.ignore(Ignore::All);
    let ports = input.ports();
    let port = ports
        .iter()
        .find(|port| input.port_name(port).is_ok_and(|n| n == name))
        .cloned()
        .ok_or_else(|| format!("MIDI input \"{name}\" is not available (any more)."))?;
    connect(input, &port, name.to_string())
}

fn connect(input: MidiInput, port: &MidiInputPort, name: String) -> Result<MidiIn, String> {

    let (tx, rx) = RingBuffer::<TimedEvent>::new(QUEUE);
    let dropped = Arc::new(AtomicU64::new(0));
    let sink = Sink {
        decoder: MidiDecoder::new(),
        tx,
        dropped: Arc::clone(&dropped),
    };

    let connection = input
        .connect(
            port,
            "henrys-looper-in",
            |micros, bytes, sink: &mut Sink| {
                // Split so the decoder and the queue are borrowed separately - the decoder needs
                // `&mut self` while the closure it is given writes into the queue.
                let Sink { decoder, tx, dropped } = sink;
                decoder.push(bytes, |event| {
                    if tx.push(TimedEvent { micros, event }).is_err() {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                });
            },
            sink,
        )
        .map_err(|e| connect_error(&name, &e.to_string()))?;

    Ok(MidiIn {
        _connection: connection,
        rx,
        dropped,
        name,
    })
}

/// Index of the port a selector means.
fn pick(selector: &str, names: &[String]) -> Result<usize, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        if names.len() == 1 {
            return Ok(0);
        }
        return Err(format!(
            "Es gibt {} MIDI-Eingaenge. Waehle einen aus:\n{}",
            names.len(),
            list(names)
        ));
    }
    if let Ok(index) = selector.parse::<usize>() {
        return names.get(index).map(|_| index).ok_or_else(|| {
            format!(
                "Den MIDI-Eingang {index} gibt es nicht. Vorhanden:\n{}",
                list(names)
            )
        });
    }
    let needle = selector.to_lowercase();
    let hits: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.to_lowercase().contains(&needle))
        .map(|(index, _)| index)
        .collect();
    match hits.as_slice() {
        [only] => Ok(*only),
        [] => Err(format!(
            "Kein MIDI-Eingang enthaelt \"{selector}\". Vorhanden:\n{}",
            list(names)
        )),
        many => Err(format!(
            "\"{selector}\" passt auf {} Eingaenge; bitte genauer oder mit der Nummer:\n{}",
            many.len(),
            list(names)
        )),
    }
}

fn list(names: &[String]) -> String {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| format!("  {index}  {name}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn init_error(error: midir::InitError) -> String {
    format!("Das MIDI-System liess sich nicht starten: {error}.")
}

/// Turn midir's one-line failure into something that says what to do about it.
fn connect_error(name: &str, message: &str) -> String {
    if message.contains("MMSYSERR_ALLOCATED") {
        return format!(
            "Der MIDI-Eingang \"{name}\" ist belegt. Windows gibt ein MIDI-Geraet immer nur an ein \
             Programm gleichzeitig heraus. Meistens haelt es Ableton Live als Control Surface \
             fest; auch ein zweites Fenster dieses Programms oder ein MIDI-Monitor im \
             Infobereich reicht. Das andere Programm schliessen oder dort das Geraet freigeben, \
             dann erneut versuchen."
        );
    }
    format!("Der MIDI-Eingang \"{name}\" liess sich nicht oeffnen: {message}.")
}
