//! A minimal VST3 instrument, only for the host tests.
//!
//! Built like `tools/test-vst3-gain` (after the `gain` example of `vst3-rs`, MIT OR Apache-2.0,
//! Micah Johnston), with what a host test of an instrument needs to observe exactly:
//!
//! - **no audio input**, one stereo output, one **event input** bus;
//! - **eight sine voices**: a note on starts a voice at the event's `sampleOffset` with amplitude
//!   `velocity * 0.25` and phase 0; a note off ends it on that very sample - no envelope, so
//!   "silent after the note off" is exact;
//! - the **sustain pedal** is parameter 0, which the plugin maps to CC 64 of every channel through
//!   `IMidiMapping` - VST3 has no controllers of its own; while it is down, note offs are held;
//! - parameter 1 is the **volume** (default 1), saved and restored as the plugin's **state**;
//! - the class "Looper Test Sine Latency" is the same instrument with its output delayed by
//!   [`LATENCY`] frames, and it says so (`getLatencySamples`).
//!
//! Component and controller are separate classes, as in most commercial plugins.

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::{ptr, slice};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

/// Frames of delay the latency variant adds and reports.
pub const LATENCY: usize = 32;
/// Voices; a ninth note takes the oldest voice.
pub const VOICES: usize = 8;
/// Amplitude of a voice at velocity 127.
pub const LEVEL: f32 = 0.25;

const NAME: &str = "Looper Test Sine";
const NAME_LATENCY: &str = "Looper Test Sine Latency";

const PARAM_SUSTAIN: u32 = 0;
const PARAM_VOLUME: u32 = 1;

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c = CString::new(src).unwrap_or_default();
    for (s, d) in c.as_bytes_with_nul().iter().zip(dst.iter_mut()) {
        *d = *s as c_char;
    }
    let too_long = c.as_bytes().len() >= dst.len();
    if let Some(last) = dst.last_mut()
        && too_long
    {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut n = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        n += 1;
    }
    if n < dst.len() {
        dst[n] = 0;
    }
}

unsafe fn read_volume(stream: *mut IBStream) -> Option<f64> {
    let stream = unsafe { ComRef::from_raw(stream) }?;
    let mut bytes = [0u8; 8];
    let mut read = 0;
    let r = unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) };
    (r == kResultOk && read == 8).then(|| f64::from_le_bytes(bytes))
}

#[derive(Clone, Copy, Default)]
struct Voice {
    on: bool,
    /// Note off arrived while the pedal was down.
    held: bool,
    channel: i16,
    pitch: i16,
    amp: f32,
    phase: f64,
    step: f64,
    age: u64,
}

/// One thing that happens inside a block, in the order of its offset.
#[derive(Clone, Copy)]
enum Action {
    On { channel: i16, pitch: i16, velocity: f32 },
    Off { channel: i16, pitch: i16 },
    Sustain(bool),
    Volume(f64),
}

struct Synth {
    voices: [Voice; VOICES],
    sustain: bool,
    volume: f64,
    sample_rate: f64,
    counter: u64,
    line: Vec<[f32; 2]>,
    pos: usize,
    actions: Vec<(i32, Action)>,
}

impl Synth {
    fn apply(&mut self, action: Action) {
        match action {
            Action::On {
                channel,
                pitch,
                velocity,
            } => {
                self.counter += 1;
                let slot = self
                    .voices
                    .iter()
                    .position(|v| !v.on)
                    .unwrap_or_else(|| {
                        (0..VOICES).min_by_key(|&i| self.voices[i].age).unwrap_or(0)
                    });
                let freq = 440.0 * 2f64.powf((pitch as f64 - 69.0) / 12.0);
                self.voices[slot] = Voice {
                    on: true,
                    held: false,
                    channel,
                    pitch,
                    amp: velocity * LEVEL,
                    phase: 0.0,
                    step: std::f64::consts::TAU * freq / self.sample_rate,
                    age: self.counter,
                };
            }
            Action::Off { channel, pitch } => {
                for v in self.voices.iter_mut() {
                    if v.on && !v.held && v.channel == channel && v.pitch == pitch {
                        if self.sustain {
                            v.held = true;
                        } else {
                            v.on = false;
                        }
                    }
                }
            }
            Action::Sustain(down) => {
                self.sustain = down;
                if !down {
                    for v in self.voices.iter_mut() {
                        if v.held {
                            v.on = false;
                            v.held = false;
                        }
                    }
                }
            }
            Action::Volume(v) => self.volume = v,
        }
    }

    fn sample(&mut self) -> f32 {
        let mut s = 0.0f32;
        for v in self.voices.iter_mut().filter(|v| v.on) {
            s += v.amp * v.phase.sin() as f32;
            v.phase = (v.phase + v.step) % std::f64::consts::TAU;
        }
        s * self.volume as f32
    }

    fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
        self.sustain = false;
        for f in self.line.iter_mut() {
            *f = [0.0; 2];
        }
        self.pos = 0;
    }
}

struct Processor {
    latency: usize,
    synth: RefCell<Synth>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl Processor {
    const CID: TUID = uid(0x4C6F6F70, 0x65725465, 0x73745369, 0x6E655031);
    const CID_LATENCY: TUID = uid(0x4C6F6F70, 0x65725465, 0x73745369, 0x6E654C31);

    fn new(latency: usize) -> Self {
        Self {
            latency,
            synth: RefCell::new(Synth {
                voices: [Voice::default(); VOICES],
                sustain: false,
                volume: 1.0,
                sample_rate: 48_000.0,
                counter: 0,
                line: vec![[0.0; 2]; latency.max(1)],
                pos: 0,
                actions: Vec::with_capacity(1024),
            }),
        }
    }
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = Controller::CID };
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        let audio = media_type == MediaTypes_::kAudio as MediaType;
        let output = dir == BusDirections_::kOutput as BusDirection;
        // One stereo output, one event input, nothing else.
        (audio == output) as i32
    }
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 || unsafe { self.getBusCount(media_type, dir) } == 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media_type;
        bus.direction = dir;
        let audio = media_type == MediaTypes_::kAudio as MediaType;
        bus.channelCount = if audio { 2 } else { 16 };
        copy_wstring(if audio { "Output" } else { "MIDI In" }, &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _t: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        self.synth.borrow_mut().reset();
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_volume(state) } {
            Some(v) => {
                self.synth.borrow_mut().volume = v;
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kResultFalse;
        };
        let mut bytes = self.synth.borrow().volume.to_le_bytes();
        let mut written = 0;
        unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut written) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins == 0 && num_outs == 1 && unsafe { *outputs } == SpeakerArr::kStereo {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 || dir != BusDirections_::kOutput as BusDirection {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        self.latency as u32
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.synth.borrow_mut().sample_rate = unsafe { (*setup).sampleRate };
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        let mut synth = self.synth.borrow_mut();
        let synth = &mut *synth;
        synth.actions.clear();
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                let id = unsafe { queue.getParameterId() };
                for p in 0..unsafe { queue.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(p, &mut offset, &mut value) } != kResultOk {
                        continue;
                    }
                    let action = match id {
                        PARAM_SUSTAIN => Action::Sustain(value >= 0.5),
                        PARAM_VOLUME => Action::Volume(value),
                        _ => continue,
                    };
                    synth.actions.push((offset, action));
                }
            }
        }
        if let Some(events) = unsafe { ComRef::from_raw(data.inputEvents) } {
            for i in 0..unsafe { events.getEventCount() } {
                let mut e: Event = unsafe { std::mem::zeroed() };
                if unsafe { events.getEvent(i, &mut e) } != kResultOk {
                    continue;
                }
                let action = match e.r#type as u32 {
                    t if t == Event_::EventTypes_::kNoteOnEvent as u32 => {
                        let n = unsafe { e.__field0.noteOn };
                        Action::On {
                            channel: n.channel,
                            pitch: n.pitch,
                            velocity: n.velocity,
                        }
                    }
                    t if t == Event_::EventTypes_::kNoteOffEvent as u32 => {
                        let n = unsafe { e.__field0.noteOff };
                        Action::Off {
                            channel: n.channel,
                            pitch: n.pitch,
                        }
                    }
                    _ => continue,
                };
                synth.actions.push((e.sampleOffset, action));
            }
        }
        // Stable: at equal offsets parameter points come first, then events in their order.
        synth.actions.sort_by_key(|(offset, _)| *offset);

        if data.numOutputs < 1 || data.outputs.is_null() {
            for k in 0..synth.actions.len() {
                let action = synth.actions[k].1;
                synth.apply(action);
            }
            return kResultOk;
        }
        let n = data.numSamples as usize;
        let outs = unsafe { &*data.outputs };
        if outs.numChannels != 2 {
            return kResultOk;
        }
        let (out_l, out_r) = unsafe {
            let o = slice::from_raw_parts(outs.__field0.channelBuffers32, 2);
            (
                slice::from_raw_parts_mut(o[0], n),
                slice::from_raw_parts_mut(o[1], n),
            )
        };
        let mut next = 0;
        for k in 0..n {
            while next < synth.actions.len() && synth.actions[next].0 as usize <= k {
                let action = synth.actions[next].1;
                synth.apply(action);
                next += 1;
            }
            let s = synth.sample();
            if self.latency == 0 {
                out_l[k] = s;
                out_r[k] = s;
            } else {
                let [l, r] = synth.line[synth.pos];
                let pos = synth.pos;
                synth.line[pos] = [s, s];
                out_l[k] = l;
                out_r[k] = r;
                synth.pos = (pos + 1) % self.latency;
            }
        }
        while next < synth.actions.len() {
            let action = synth.actions[next].1;
            synth.apply(action);
            next += 1;
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for Processor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct Controller {
    values: [Cell<f64>; 2],
}

impl Class for Controller {
    type Interfaces = (IEditController, IMidiMapping);
}

impl Controller {
    const CID: TUID = uid(0x4C6F6F70, 0x65725465, 0x73745369, 0x6E654331);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IMidiMappingTrait for Controller {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        controller: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus == 0 && controller == ControllerNumbers_::kCtrlSustainOnOff as CtrlNumber {
            unsafe { *id = PARAM_SUSTAIN };
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        if let Some(v) = unsafe { read_volume(state) } {
            self.values[PARAM_VOLUME as usize].set(v);
        }
        kResultOk
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        2
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let (title, steps, default) = match index as u32 {
            PARAM_SUSTAIN => ("Sustain", 1, 0.0),
            PARAM_VOLUME => ("Volume", 0, 1.0),
            _ => return kInvalidArgument,
        };
        let info = unsafe { &mut *info };
        info.id = index as u32;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = steps;
        info.defaultNormalizedValue = default;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        let text = match id {
            PARAM_SUSTAIN => (if value >= 0.5 { "On" } else { "Off" }).to_string(),
            PARAM_VOLUME => format!("{:.2}", value),
            _ => return kInvalidArgument,
        };
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }
    unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _id: u32, v: f64) -> f64 {
        v
    }
    unsafe fn plainParamToNormalized(&self, _id: u32, v: f64) -> f64 {
        v
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.values.get(id as usize).map(|v| v.get()).unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match self.values.get(id as usize) {
            Some(v) => {
                v.set(value);
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

/// (class id, category, name, sub-categories) of every class in the bundle.
const CLASSES: [(TUID, &str, &str, &str); 3] = [
    (Processor::CID, "Audio Module Class", NAME, "Instrument|Synth"),
    (Processor::CID_LATENCY, "Audio Module Class", NAME_LATENCY, "Instrument|Synth"),
    (Controller::CID, "Component Controller Class", NAME, ""),
];

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("Henrys Looper Tests", &mut info.vendor);
        copy_cstring("", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        CLASSES.len() as i32
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((cid, category, name, _)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            Processor::CID => ComWrapper::new(Processor::new(0)).to_com_ptr::<FUnknown>(),
            Processor::CID_LATENCY => {
                ComWrapper::new(Processor::new(LATENCY)).to_com_ptr::<FUnknown>()
            }
            Controller::CID => ComWrapper::new(Controller {
                values: [Cell::new(0.0), Cell::new(1.0)],
            })
            .to_com_ptr::<FUnknown>(),
            _ => None,
        };
        match instance {
            Some(instance) => {
                let p = instance.as_ptr();
                unsafe { ((*(*p).vtbl).queryInterface)(p, iid as *mut TUID, obj) }
            }
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((cid, category, name, sub)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        info.classFlags = 0;
        copy_cstring(sub, &mut info.subCategories);
        copy_cstring("Henrys Looper Tests", &mut info.vendor);
        copy_cstring("0.1.0", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map(|p| p.into_raw())
        .unwrap_or(ptr::null_mut())
}
