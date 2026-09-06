//! Raw MIDI bytes -> events, and the textual id an event is addressed by.
//!
//! Nothing in this file touches a device, which is the point: everything from the wire format up
//! is provable offline, and only [`super::input`] needs hardware.
//!
//! # Running status
//!
//! A MIDI sender may leave the status byte out when it repeats: `90 24 7F 24 00` is two note
//! events on channel 1, not one event and a stray pair of bytes. On Windows the winmm driver
//! already splits the stream into complete messages before midir ever sees it (see midir's
//! `backend/winmm/handler.rs`, which reads the status byte and works out the message length), so
//! this decoder will normally get one whole message per call. It handles running status, messages
//! split across two calls, interleaved real-time bytes and SysEx anyway - a decoder that only works
//! when the layer below is friendly is a decoder that fails on the one controller that is not.
//!
//! # What is decoded and what is dropped
//!
//! Note on, note off, control change and pitch bend become events. Polyphonic and channel
//! aftertouch, program change and everything system-level are consumed correctly (so they cannot
//! desynchronise the stream) and then dropped: none of them is a control surface's button or knob.
//!
//! **A note on with velocity 0 is a note off.** Most controllers, the MPD218 included, never send
//! `0x8n` at all; a looper that treats `90 24 00` as a press would latch every pad down forever.

use std::fmt;

/// Status nibble of a note off.
const NOTE_OFF: u8 = 0x80;
/// Status nibble of a note on.
const NOTE_ON: u8 = 0x90;
/// Status nibble of a control change.
const CONTROL_CHANGE: u8 = 0xB0;
/// Status nibble of a pitch bend.
const PITCH_BEND: u8 = 0xE0;
/// Centre of the 14-bit pitch bend range.
const BEND_CENTRE: i16 = 8_192;

/// One incoming MIDI message, normalised.
///
/// `Copy` and free of allocation on purpose: these travel from the MIDI callback to the control
/// thread through a lock-free ring buffer, and the callback must not allocate.
///
/// Channels are **1-based**, the way a controller's display and the binding ids spell them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiEvent {
    /// A key or pad went down. Velocity is at least 1 - velocity 0 arrives as [`MidiEvent::NoteOff`].
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8, velocity: u8 },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// 14 bit, already centred: -8192 fully down, 0 at rest, +8191 fully up.
    PitchBend { channel: u8, value: i16 },
}

impl MidiEvent {
    pub fn channel(&self) -> u8 {
        match *self {
            MidiEvent::NoteOn { channel, .. }
            | MidiEvent::NoteOff { channel, .. }
            | MidiEvent::ControlChange { channel, .. }
            | MidiEvent::PitchBend { channel, .. } => channel,
        }
    }

    /// The id this event is looked up by. A note on and the note off that follows it share one id -
    /// that is what lets a binding be momentary or a toggle.
    pub fn id(&self) -> MidiId {
        match *self {
            MidiEvent::NoteOn { channel, note, .. } | MidiEvent::NoteOff { channel, note, .. } => {
                MidiId::note(channel, note)
            }
            MidiEvent::ControlChange {
                channel, controller, ..
            } => MidiId::cc(channel, controller),
            MidiEvent::PitchBend { channel, .. } => MidiId::bend(channel),
        }
    }

    /// True for a press: a note on, or a controller moving into its upper half.
    ///
    /// A continuous controller has no "press", so the half-way point decides. 64 is the MIDI
    /// convention for a switch pedal and what every controller that sends 0/127 lands on.
    pub fn is_high(&self) -> bool {
        match *self {
            MidiEvent::NoteOn { .. } => true,
            MidiEvent::NoteOff { .. } => false,
            MidiEvent::ControlChange { value, .. } => value >= 64,
            MidiEvent::PitchBend { value, .. } => value >= 0,
        }
    }

    /// The event's value as a 7-bit number, whatever kind it is: velocity, controller value, or a
    /// pitch bend scaled down from 14 bits. This is the one number a knob binding works on.
    pub fn value7(&self) -> u8 {
        match *self {
            MidiEvent::NoteOn { velocity, .. } | MidiEvent::NoteOff { velocity, .. } => velocity,
            MidiEvent::ControlChange { value, .. } => value,
            MidiEvent::PitchBend { value, .. } => {
                let raw = (i32::from(value) + i32::from(BEND_CENTRE)) * 127;
                (raw / (2 * i32::from(BEND_CENTRE) - 1)).clamp(0, 127) as u8
            }
        }
    }

    /// German one-liner for the monitor display.
    pub fn describe(&self) -> String {
        match *self {
            MidiEvent::NoteOn {
                channel,
                note,
                velocity,
            } => format!("Note an   Kanal {channel:2}  Note {note:3}  Anschlag {velocity:3}"),
            MidiEvent::NoteOff {
                channel,
                note,
                velocity,
            } => format!("Note aus  Kanal {channel:2}  Note {note:3}  Anschlag {velocity:3}"),
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => format!("Regler    Kanal {channel:2}  CC   {controller:3}  Wert     {value:3}"),
            MidiEvent::PitchBend { channel, value } => {
                format!("Pitchbend Kanal {channel:2}                Wert   {value:6}")
            }
        }
    }
}

/// What kind of control an id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MidiIdKind {
    Note,
    Cc,
    Bend,
}

/// The address of one physical control: `ch1.note36`, `ch1.cc64`, `ch10.bend`.
///
/// The spelling is the one `docs/contracts-v0.md` already fixed for the score's `midi:` block, and
/// it is deliberately the same string the monitor prints for an incoming event - so a musician can
/// read an id off the screen and type it into a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MidiId {
    /// 1-based, 1..=16.
    pub channel: u8,
    pub kind: MidiIdKind,
    /// Note or controller number; 0 for a pitch bend, which has no number.
    pub number: u8,
}

impl MidiId {
    pub fn note(channel: u8, number: u8) -> Self {
        Self {
            channel,
            kind: MidiIdKind::Note,
            number,
        }
    }

    pub fn cc(channel: u8, number: u8) -> Self {
        Self {
            channel,
            kind: MidiIdKind::Cc,
            number,
        }
    }

    pub fn bend(channel: u8) -> Self {
        Self {
            channel,
            kind: MidiIdKind::Bend,
            number: 0,
        }
    }

    /// Parse `ch1.note36` / `ch1.cc64` / `ch1.bend`. German error, because this is read out of a
    /// file a human wrote.
    pub fn parse(text: &str) -> Result<Self, String> {
        let complaint = || {
            format!(
                "'{text}' ist keine MIDI-Adresse. Erwartet wird ch<Kanal>.note<Nummer>, \
                 ch<Kanal>.cc<Nummer> oder ch<Kanal>.bend, z. B. ch1.note36."
            )
        };
        let rest = text.strip_prefix("ch").ok_or_else(complaint)?;
        let (channel, rest) = rest.split_once('.').ok_or_else(complaint)?;
        let channel: u8 = channel.parse().map_err(|_| complaint())?;
        if !(1..=16).contains(&channel) {
            return Err(format!(
                "MIDI-Kanal {channel} in '{text}' gibt es nicht - es gibt die Kanaele 1 bis 16."
            ));
        }
        let (kind, number) = if rest == "bend" {
            (MidiIdKind::Bend, 0u16)
        } else if let Some(n) = rest.strip_prefix("note") {
            (MidiIdKind::Note, n.parse().map_err(|_| complaint())?)
        } else if let Some(n) = rest.strip_prefix("cc") {
            (MidiIdKind::Cc, n.parse().map_err(|_| complaint())?)
        } else {
            return Err(complaint());
        };
        if number > 127 {
            return Err(format!(
                "Die Nummer {number} in '{text}' ist zu gross - MIDI kennt 0 bis 127."
            ));
        }
        Ok(Self {
            channel,
            kind,
            number: number as u8,
        })
    }
}

impl fmt::Display for MidiId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            MidiIdKind::Note => write!(f, "ch{}.note{}", self.channel, self.number),
            MidiIdKind::Cc => write!(f, "ch{}.cc{}", self.channel, self.number),
            MidiIdKind::Bend => write!(f, "ch{}.bend", self.channel),
        }
    }
}

/// Byte stream -> events, keeping whatever state the MIDI wire format needs.
///
/// One decoder per connection. It is fed from the MIDI callback and does nothing but arithmetic on
/// three bytes of state, so it is cheap enough to sit there.
#[derive(Debug, Default)]
pub struct MidiDecoder {
    /// Current running status, or 0 when there is none.
    status: u8,
    data: [u8; 2],
    have: usize,
    in_sysex: bool,
}

impl MidiDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; every complete message calls `emit`.
    ///
    /// Split messages are fine: state survives between calls, which is what makes this usable from
    /// a callback that gets whatever the driver felt like handing over.
    pub fn push(&mut self, bytes: &[u8], mut emit: impl FnMut(MidiEvent)) {
        for &byte in bytes {
            // System real-time bytes may appear *inside* another message and change nothing about
            // it - not even the running status. Clock and active sensing arrive constantly, so
            // getting this wrong corrupts every second message on some controllers.
            if byte >= 0xF8 {
                continue;
            }
            if self.in_sysex {
                if byte == 0xF7 {
                    self.in_sysex = false;
                }
                // A status byte other than F7 ends a truncated SysEx; fall through and handle it.
                if byte < 0x80 || byte == 0xF7 {
                    continue;
                }
                self.in_sysex = false;
            }
            if byte >= 0x80 {
                if byte >= 0xF0 {
                    // System common. It cancels running status (that is in the specification) and
                    // is otherwise of no interest here.
                    self.status = 0;
                    self.have = 0;
                    self.in_sysex = byte == 0xF0;
                } else {
                    self.status = byte;
                    self.have = 0;
                }
                continue;
            }
            if self.status == 0 {
                // A data byte with no status before it. Nothing can be made of it.
                continue;
            }
            self.data[self.have] = byte;
            self.have += 1;
            if self.have < needed(self.status) {
                continue;
            }
            self.have = 0;
            if let Some(event) = build(self.status, self.data[0], self.data[1]) {
                emit(event);
            }
        }
    }

    /// Everything in these bytes, collected. The convenience the tests are written against; the
    /// callback uses [`Self::push`] so it does not allocate.
    pub fn decode(&mut self, bytes: &[u8]) -> Vec<MidiEvent> {
        let mut out = Vec::new();
        self.push(bytes, |event| out.push(event));
        out
    }
}

/// Data bytes a channel message of this status carries.
fn needed(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        _ => 2,
    }
}

fn build(status: u8, d0: u8, d1: u8) -> Option<MidiEvent> {
    let channel = (status & 0x0F) + 1;
    match status & 0xF0 {
        NOTE_OFF => Some(MidiEvent::NoteOff {
            channel,
            note: d0,
            velocity: d1,
        }),
        // The one convention that decides whether pads work at all - see the module comment.
        NOTE_ON if d1 == 0 => Some(MidiEvent::NoteOff {
            channel,
            note: d0,
            velocity: 0,
        }),
        NOTE_ON => Some(MidiEvent::NoteOn {
            channel,
            note: d0,
            velocity: d1,
        }),
        CONTROL_CHANGE => Some(MidiEvent::ControlChange {
            channel,
            controller: d0,
            value: d1,
        }),
        PITCH_BEND => Some(MidiEvent::PitchBend {
            channel,
            value: (i16::from(d1) << 7 | i16::from(d0)) - BEND_CENTRE,
        }),
        // Aftertouch and program change are consumed above (their length is known) and dropped
        // here: neither is a button or a knob on a control surface.
        _ => None,
    }
}
