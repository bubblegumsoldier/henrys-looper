//! A minimal VST3 effect, only for the host tests.
//!
//! Derived from the `gain` example of `vst3-rs` (MIT OR Apache-2.0, Micah Johnston), extended with
//! what a host test needs to observe: a **known latency** (the signal is delayed by
//! [`LATENCY`] frames and the plugin says so), and **state** (the gain is saved and restored as
//! eight bytes). Component and controller are separate classes, as in most commercial plugins.
//!
//! Behaviour: `out = gain * in` delayed by `LATENCY` frames, stereo in and out. Parameter 0 is the
//! gain, normalised 0..1, default 1.
//!
//! Parameter [`PROBE_PARAM`] (hidden, a switch) turns the plugin into a probe of the host's
//! `ProcessContext`: instead of audio, the first [`PROBE_FIELDS`] frames of each block carry what
//! the host said - field `k` as an `f64`, its low 32 bits in `out_l[k]` and its high 32 bits in
//! `out_r[k]` (`f32::from_bits`), so nothing is rounded on the way. See [`probe_fields`].

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ptr, slice};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

/// Frames of delay the plugin adds and reports.
pub const LATENCY: usize = 64;
/// The hidden switch that turns the plugin into a probe of the `ProcessContext`.
pub const PROBE_PARAM: u32 = 1;
/// Fields a probe block carries, in the order of [`probe_fields`].
pub const PROBE_FIELDS: usize = 12;

/// What a probe block reports, field by field: tempo, time signature numerator and denominator,
/// `projectTimeMusic`, `barPositionMusic`, `projectTimeSamples`, playing (0/1), whether tempo,
/// time signature, musical time and bar position are all flagged valid (0/1), the sample rate,
/// `continousTimeSamples`, whether a cycle is flagged active (0/1), and 1 if there was a context.
pub fn probe_fields(context: *const ProcessContext) -> [f64; PROBE_FIELDS] {
    use ProcessContext_::StatesAndFlags_ as F;
    let Some(c) = (unsafe { context.as_ref() }) else {
        return [0.0; PROBE_FIELDS];
    };
    let valid = (F::kTempoValid | F::kTimeSigValid | F::kProjectTimeMusicValid | F::kBarPositionValid)
        as u32;
    [
        c.tempo,
        c.timeSigNumerator as f64,
        c.timeSigDenominator as f64,
        c.projectTimeMusic,
        c.barPositionMusic,
        c.projectTimeSamples as f64,
        (c.state & F::kPlaying as u32 != 0) as u8 as f64,
        (c.state & valid == valid) as u8 as f64,
        c.sampleRate,
        c.continousTimeSamples as f64,
        (c.state & F::kCycleActive as u32 != 0) as u8 as f64,
        1.0,
    ]
}
const NAME: &str = "Looper Test Gain";

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

unsafe fn read_gain(stream: *mut IBStream) -> Option<f64> {
    let stream = unsafe { ComRef::from_raw(stream) }?;
    let mut bytes = [0u8; 8];
    let mut read = 0;
    let r = unsafe { stream.read(bytes.as_mut_ptr() as *mut c_void, 8, &mut read) };
    (r == kResultOk && read == 8).then(|| f64::from_le_bytes(bytes))
}

struct Processor {
    gain: AtomicU64,
    probe: Cell<bool>,
    line: RefCell<Vec<[f32; 2]>>,
    pos: Cell<usize>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl Processor {
    const CID: TUID = uid(0x4C6F6F70, 0x65725465, 0x73744761, 0x696E5031);

    fn gain(&self) -> f64 {
        f64::from_bits(self.gain.load(Ordering::Relaxed))
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
    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        (media_type == MediaTypes_::kAudio as MediaType) as i32
    }
    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type != MediaTypes_::kAudio as MediaType || index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring(if dir == 0 { "Input" } else { "Output" }, &mut bus.name);
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
        for f in self.line.borrow_mut().iter_mut() {
            *f = [0.0; 2];
        }
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match unsafe { read_gain(state) } {
            Some(g) => {
                self.gain.store(g.to_bits(), Ordering::Relaxed);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kResultFalse;
        };
        let mut bytes = self.gain().to_le_bytes();
        let mut written = 0;
        unsafe { stream.write(bytes.as_mut_ptr() as *mut c_void, 8, &mut written) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins == 1
            && num_outs == 1
            && unsafe { *inputs } == SpeakerArr::kStereo
            && unsafe { *outputs } == SpeakerArr::kStereo
        {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
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
        LATENCY as u32
    }
    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                if let Some(queue) = unsafe { ComRef::from_raw(changes.getParameterData(i)) } {
                    let n = unsafe { queue.getPointCount() };
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(n - 1, &mut offset, &mut value) } != kResultTrue {
                        continue;
                    }
                    match unsafe { queue.getParameterId() } {
                        0 => self.gain.store(value.to_bits(), Ordering::Relaxed),
                        PROBE_PARAM => self.probe.set(value >= 0.5),
                        _ => {}
                    }
                }
            }
        }
        if data.numInputs != 1 || data.numOutputs != 1 {
            return kResultOk;
        }
        let n = data.numSamples as usize;
        let (ins, outs) = unsafe { (&*data.inputs, &*data.outputs) };
        if ins.numChannels != 2 || outs.numChannels != 2 {
            return kResultOk;
        }
        let gain = self.gain() as f32;
        let (in_l, in_r, out_l, out_r) = unsafe {
            let i = slice::from_raw_parts(ins.__field0.channelBuffers32, 2);
            let o = slice::from_raw_parts(outs.__field0.channelBuffers32, 2);
            (
                slice::from_raw_parts(i[0], n),
                slice::from_raw_parts(i[1], n),
                slice::from_raw_parts_mut(o[0], n),
                slice::from_raw_parts_mut(o[1], n),
            )
        };
        if self.probe.get() {
            let fields = probe_fields(data.processContext);
            for k in 0..n {
                let bits = fields.get(k).map_or(0, |f| f.to_bits());
                out_l[k] = f32::from_bits(bits as u32);
                out_r[k] = f32::from_bits((bits >> 32) as u32);
            }
            return kResultOk;
        }
        let mut line = self.line.borrow_mut();
        let mut pos = self.pos.get();
        for k in 0..n {
            let [l, r] = line[pos];
            line[pos] = [in_l[k] * gain, in_r[k] * gain];
            out_l[k] = l;
            out_r[k] = r;
            pos = (pos + 1) % LATENCY;
        }
        self.pos.set(pos);
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
    gain: Cell<f64>,
    probe: Cell<f64>,
}

impl Class for Controller {
    type Interfaces = (IEditController,);
}

impl Controller {
    const CID: TUID = uid(0x4C6F6F70, 0x65725465, 0x73744761, 0x696E4331);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        if let Some(g) = unsafe { read_gain(state) } {
            self.gain.set(g);
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
        if index == PROBE_PARAM as i32 {
            let info = unsafe { &mut *info };
            info.id = PROBE_PARAM;
            copy_wstring("Probe", &mut info.title);
            copy_wstring("Probe", &mut info.shortTitle);
            copy_wstring("", &mut info.units);
            info.stepCount = 1;
            info.defaultNormalizedValue = 0.0;
            info.unitId = 0;
            info.flags = ParameterInfo_::ParameterFlags_::kIsHidden as i32;
            return kResultOk;
        }
        if index != 0 {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.id = 0;
        copy_wstring("Gain", &mut info.title);
        copy_wstring("Gain", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = 1.0;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 {
            return kInvalidArgument;
        }
        copy_wstring(&format!("{:.2}", value), unsafe { &mut *string });
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
        match id {
            0 => self.gain.get(),
            PROBE_PARAM => self.probe.get(),
            _ => 0.0,
        }
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match id {
            0 => self.gain.set(value),
            PROBE_PARAM => self.probe.set(value),
            _ => return kInvalidArgument,
        }
        kResultOk
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
    type Interfaces = (IPluginFactory,);
}

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
        2
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = unsafe { &mut *info };
        let (cid, category) = match index {
            0 => (Processor::CID, "Audio Module Class"),
            1 => (Controller::CID, "Component Controller Class"),
            _ => return kInvalidArgument,
        };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(category, &mut info.category);
        copy_cstring(NAME, &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            Processor::CID => ComWrapper::new(Processor {
                gain: AtomicU64::new(1.0f64.to_bits()),
                probe: Cell::new(false),
                line: RefCell::new(vec![[0.0; 2]; LATENCY]),
                pos: Cell::new(0),
            })
            .to_com_ptr::<FUnknown>(),
            Controller::CID => ComWrapper::new(Controller {
                gain: Cell::new(1.0),
                probe: Cell::new(0.0),
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

/// Set in the environment, the plugin crashes the moment its factory is asked for - a stand-in for
/// the broken plugin the scan has to survive.
pub const CRASH_ENV: &str = "LOOPER_TEST_VST3_CRASH";

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    if std::env::var_os(CRASH_ENV).is_some() {
        std::process::abort();
    }
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map(|p| p.into_raw())
        .unwrap_or(ptr::null_mut())
}
