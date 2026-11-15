//! Audio Units through the AudioToolbox C API.
//!
//! An Audio Unit is found in the system registry (`AudioComponentFindNext`), instantiated in this
//! process (`AudioComponentInstanceNew`; an AUv3 extension is bridged by the system), fed
//! non-interleaved 32-bit float through a render callback and pulled with `AudioUnitRender`. Its
//! parameters are plain values between a min and a max, which are normalised here so the rest of
//! the program sees what it sees for VST3. Its state is the `ClassInfo` property list, stored as
//! binary plist bytes.
//!
//! The editor is the unit's own Cocoa view if it has one (`kAudioUnitProperty_CocoaUI`), otherwise
//! Apple's `AUGenericView` from CoreAudioKit - a complete slider panel for any unit, which is what
//! Logic shows for Apple's own effects as well.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::sel;
use objc2_foundation::NSSize;

use super::cf::{self, CFStringRef, CFTypeRef};
use super::editor::EditorWindow;
use super::{Format, Kind, ParamInfo, PluginInfo, PluginInstance};
use crate::engine::insert::{InsertProcessor, PluginTransport};
use crate::engine::source::{BlockEvent, SourceProcessor, StereoOut};

type OSStatus = i32;
type AudioComponent = *mut c_void;
type AudioUnit = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioComponentDescription {
    pub componentType: u32,
    pub componentSubType: u32,
    pub componentManufacturer: u32,
    pub componentFlags: u32,
    pub componentFlagsMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioStreamBasicDescription {
    mSampleRate: f64,
    mFormatID: u32,
    mFormatFlags: u32,
    mBytesPerPacket: u32,
    mFramesPerPacket: u32,
    mBytesPerFrame: u32,
    mChannelsPerFrame: u32,
    mBitsPerChannel: u32,
    mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    mNumberChannels: u32,
    mDataByteSize: u32,
    mData: *mut c_void,
}

/// `AudioBufferList` with room for two buffers - all this host ever passes.
#[repr(C)]
struct AudioBufferList2 {
    mNumberBuffers: u32,
    mBuffers: [AudioBuffer; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SMPTETime {
    mSubframes: i16,
    mSubframeDivisor: i16,
    mCounter: u32,
    mType: u32,
    mFlags: u32,
    mHours: i16,
    mMinutes: i16,
    mSeconds: i16,
    mFrames: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AudioTimeStamp {
    mSampleTime: f64,
    mHostTime: u64,
    mRateScalar: f64,
    mWordClockTime: u64,
    mSMPTETime: SMPTETime,
    mFlags: u32,
    mReserved: u32,
}

type AURenderCallback = unsafe extern "C" fn(
    refcon: *mut c_void,
    flags: *mut u32,
    time: *const AudioTimeStamp,
    bus: u32,
    frames: u32,
    data: *mut AudioBufferList2,
) -> OSStatus;

#[repr(C)]
struct AURenderCallbackStruct {
    inputProc: AURenderCallback,
    inputProcRefCon: *mut c_void,
}

#[repr(C)]
struct AudioUnitParameterInfo {
    name: [c_char; 52],
    unitName: CFStringRef,
    clumpID: u32,
    cfNameString: CFStringRef,
    unit: u32,
    minValue: f32,
    maxValue: f32,
    defaultValue: f32,
    flags: u32,
}

#[repr(C)]
struct AudioUnitParameterStringFromValue {
    inParamID: u32,
    inValue: *const f32,
    outString: CFStringRef,
}

const fn fcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const kAudioUnitType_Effect: u32 = fcc(b"aufx");
const kAudioUnitType_MusicEffect: u32 = fcc(b"aumf");
const kAudioUnitType_MusicDevice: u32 = fcc(b"aumu");

const kAudioUnitScope_Global: u32 = 0;
const kAudioUnitScope_Input: u32 = 1;
const kAudioUnitScope_Output: u32 = 2;

const kAudioUnitProperty_ClassInfo: u32 = 0;
const kAudioUnitProperty_ParameterList: u32 = 3;
const kAudioUnitProperty_ParameterInfo: u32 = 4;
const kAudioUnitProperty_StreamFormat: u32 = 8;
const kAudioUnitProperty_Latency: u32 = 12;
const kAudioUnitProperty_MaximumFramesPerSlice: u32 = 14;
const kAudioUnitProperty_SetRenderCallback: u32 = 23;
const kAudioUnitProperty_HostCallbacks: u32 = 27;
const kAudioUnitProperty_CocoaUI: u32 = 31;
const kAudioUnitProperty_ParameterStringFromValue: u32 = 33;

const kAudioFormatLinearPCM: u32 = fcc(b"lpcm");
const kAudioFormatFlagIsFloat: u32 = 1;
const kAudioFormatFlagIsPacked: u32 = 8;
const kAudioFormatFlagIsNonInterleaved: u32 = 1 << 5;
const kAudioTimeStampSampleTimeValid: u32 = 1;

const kAudioUnitParameterFlag_CFNameRelease: u32 = 1 << 4;
const kAudioUnitParameterFlag_HasCFNameString: u32 = 1 << 27;
const kAudioUnitParameterFlag_IsReadable: u32 = 1 << 30;
const kAudioUnitParameterFlag_IsWritable: u32 = 1 << 31;
const kAudioUnitParameterFlag_NonRealTime: u32 = 1 << 24;
const kAudioUnitParameterFlag_ExpertMode: u32 = 1 << 26;

const kAudioUnitParameterUnit_Indexed: u32 = 1;
const kAudioUnitParameterUnit_Boolean: u32 = 2;
const kAudioUnitParameterUnit_CustomUnit: u32 = 26;

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(
        component: AudioComponent,
        desc: *const AudioComponentDescription,
    ) -> AudioComponent;
    fn AudioComponentCopyName(component: AudioComponent, name: *mut CFStringRef) -> OSStatus;
    fn AudioComponentGetDescription(
        component: AudioComponent,
        desc: *mut AudioComponentDescription,
    ) -> OSStatus;
    fn AudioComponentGetVersion(component: AudioComponent, version: *mut u32) -> OSStatus;
    fn AudioComponentInstanceNew(component: AudioComponent, unit: *mut AudioUnit) -> OSStatus;
    fn AudioComponentInstanceDispose(unit: AudioUnit) -> OSStatus;
    fn AudioUnitInitialize(unit: AudioUnit) -> OSStatus;
    fn AudioUnitUninitialize(unit: AudioUnit) -> OSStatus;
    fn AudioUnitGetPropertyInfo(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        size: *mut u32,
        writable: *mut u8,
    ) -> OSStatus;
    fn AudioUnitGetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> OSStatus;
    fn AudioUnitSetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> OSStatus;
    fn AudioUnitGetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: *mut f32,
    ) -> OSStatus;
    fn AudioUnitSetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: f32,
        offset: u32,
    ) -> OSStatus;
    fn AudioUnitRender(
        unit: AudioUnit,
        flags: *mut u32,
        time: *const AudioTimeStamp,
        bus: u32,
        frames: u32,
        data: *mut AudioBufferList2,
    ) -> OSStatus;
    fn AudioUnitReset(unit: AudioUnit, scope: u32, element: u32) -> OSStatus;
    /// MusicDevice.h. Schedules one MIDI message for the next render, `offset` frames into it.
    fn MusicDeviceMIDIEvent(
        unit: AudioUnit,
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    ) -> OSStatus;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend();
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

// ---------------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------------

/// A four-character code as text, or as eight hex digits when it is not printable.
fn fourcc_text(v: u32) -> String {
    let b = v.to_be_bytes();
    if b.iter().all(|c| (0x20..0x7f).contains(c) && *c != b':') {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        format!("{v:08x}")
    }
}

fn fourcc_parse(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.len() == 4 {
        return Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    }
    if b.len() == 8 {
        return u32::from_str_radix(s, 16).ok();
    }
    None
}

pub fn id_of(desc: &AudioComponentDescription) -> String {
    format!(
        "au:{}:{}:{}",
        fourcc_text(desc.componentType),
        fourcc_text(desc.componentSubType),
        fourcc_text(desc.componentManufacturer)
    )
}

pub fn parse_id(id: &str) -> Option<AudioComponentDescription> {
    let mut parts = id.strip_prefix("au:")?.split(':');
    let desc = AudioComponentDescription {
        componentType: fourcc_parse(parts.next()?)?,
        componentSubType: fourcc_parse(parts.next()?)?,
        componentManufacturer: fourcc_parse(parts.next()?)?,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    parts.next().is_none().then_some(desc)
}

/// Every effect and instrument the system knows. Reads the registry only - no plugin code runs.
pub fn enumerate() -> Vec<PluginInfo> {
    let any = AudioComponentDescription::default();
    let mut out = Vec::new();
    let mut component: AudioComponent = std::ptr::null_mut();
    loop {
        component = unsafe { AudioComponentFindNext(component, &any) };
        if component.is_null() {
            break;
        }
        let mut desc = AudioComponentDescription::default();
        if unsafe { AudioComponentGetDescription(component, &mut desc) } != 0 {
            continue;
        }
        let (kind, category) = match desc.componentType {
            kAudioUnitType_Effect => (Kind::Effect, "Effect"),
            kAudioUnitType_MusicEffect => (Kind::Effect, "Music Effect"),
            kAudioUnitType_MusicDevice => (Kind::Instrument, "Instrument"),
            _ => continue,
        };
        let mut name: CFStringRef = std::ptr::null();
        unsafe { AudioComponentCopyName(component, &mut name) };
        let full = cf::take_string(name);
        // The registry names a unit "Vendor: Name".
        let (vendor, name) = match full.split_once(": ") {
            Some((v, n)) => (v.to_string(), n.to_string()),
            None => (fourcc_text(desc.componentManufacturer), full.clone()),
        };
        let mut version = 0u32;
        unsafe { AudioComponentGetVersion(component, &mut version) };
        out.push(PluginInfo {
            id: id_of(&desc),
            format: Format::Au,
            name,
            vendor,
            version: format!(
                "{}.{}.{}",
                version >> 16,
                (version >> 8) & 0xff,
                version & 0xff
            ),
            category: category.to_string(),
            kind,
            path: None,
        });
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The instance
// ---------------------------------------------------------------------------------------------

struct AuParam {
    id: u32,
    name: String,
    unit_name: String,
    unit: u32,
    min: f32,
    max: f32,
    default: f32,
    flags: u32,
}

impl AuParam {
    fn normalise(&self, plain: f32) -> f64 {
        if self.max > self.min {
            (((plain - self.min) / (self.max - self.min)) as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    fn plain(&self, normalised: f64) -> f32 {
        let v = self.min + (self.max - self.min) * normalised.clamp(0.0, 1.0) as f32;
        if self.steps() > 0 { v.round() } else { v }
    }

    fn steps(&self) -> u32 {
        match self.unit {
            kAudioUnitParameterUnit_Boolean => 1,
            kAudioUnitParameterUnit_Indexed => (self.max - self.min).max(0.0) as u32,
            _ => 0,
        }
    }
}

/// Units the AU parameter-unit enum names, as a musician writes them.
fn unit_label(unit: u32) -> &'static str {
    match unit {
        3 => "%",
        4 => "s",
        5 => "samples",
        7 => "x",
        8 => "Hz",
        9 => "cents",
        10 => "semitones",
        13 => "dB",
        15 => "°",
        21 => "oct",
        22 => "BPM",
        23 => "beats",
        24 => "ms",
        25 => ":1",
        _ => "",
    }
}

pub struct AuPlugin {
    info: PluginInfo,
    unit: AudioUnit,
    initialized: bool,
    sample_rate: f64,
    params: Vec<AuParam>,
    editor: Option<EditorWindow>,
    /// Set while a processor is out in the engine; `deactivate` waits for nobody, it trusts the
    /// caller, but a second `activate` while one is out would hand the unit to two threads.
    processing: std::sync::Arc<AtomicBool>,
    /// Last values seen, to report what the plugin's own editor changed.
    seen: Mutex<Vec<f32>>,
    /// The clock the unit asks for through its host callbacks. Owned here, not by the processor:
    /// the callbacks' user data must stay valid for as long as the unit can call them, and a unit
    /// may do that from its own view as well as from inside `AudioUnitRender`.
    host_time: Arc<HostTime>,
}

pub fn load(info: &PluginInfo) -> Result<Box<dyn PluginInstance>, String> {
    open(info).map(|plugin| Box::new(plugin) as Box<dyn PluginInstance>)
}

pub(super) fn open(info: &PluginInfo) -> Result<AuPlugin, String> {
    let desc = parse_id(&info.id).ok_or_else(|| format!("Not an Audio Unit id: {}", info.id))?;
    let component = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
    if component.is_null() {
        return Err(format!(
            "The Audio Unit \"{}\" is no longer installed.",
            info.name
        ));
    }
    let mut unit: AudioUnit = std::ptr::null_mut();
    let status = unsafe { AudioComponentInstanceNew(component, &mut unit) };
    if status != 0 || unit.is_null() {
        return Err(format!(
            "The Audio Unit \"{}\" could not be opened ({}).",
            info.name,
            status_text(status)
        ));
    }
    let mut plugin = AuPlugin {
        info: info.clone(),
        unit,
        initialized: false,
        sample_rate: 48_000.0,
        params: Vec::new(),
        editor: None,
        processing: std::sync::Arc::new(AtomicBool::new(false)),
        seen: Mutex::new(Vec::new()),
        host_time: Arc::new(HostTime::default()),
    };
    plugin.host_time.store(&PluginTransport::standing(48_000.0));
    plugin.install_host_callbacks();
    plugin.read_params();
    Ok(plugin)
}

impl AuPlugin {
    /// Tell the unit where to ask for tempo, beat and transport. A unit that has no use for them
    /// may refuse the property; that changes nothing for it.
    fn install_host_callbacks(&self) {
        let info = HostCallbackInfo::for_clock(&self.host_time);
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_HostCallbacks,
                kAudioUnitScope_Global,
                0,
                &info as *const HostCallbackInfo as *const c_void,
                std::mem::size_of::<HostCallbackInfo>() as u32,
            );
        }
    }

    /// The host callbacks as the unit itself hands them back - proof that it took them.
    #[cfg(test)]
    pub(super) fn host_callbacks(&self) -> Option<HostCallbackInfo> {
        let mut info = HostCallbackInfo {
            host_user_data: std::ptr::null_mut(),
            beat_and_tempo: None,
            musical_time_location: None,
            transport_state: None,
            transport_state2: None,
        };
        let mut size = std::mem::size_of::<HostCallbackInfo>() as u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_HostCallbacks,
                kAudioUnitScope_Global,
                0,
                &mut info as *mut HostCallbackInfo as *mut c_void,
                &mut size,
            )
        };
        (status == 0).then_some(info)
    }

    fn read_params(&mut self) {
        self.params.clear();
        let mut size = 0u32;
        let mut writable = 0u8;
        let status = unsafe {
            AudioUnitGetPropertyInfo(
                self.unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                &mut size,
                &mut writable,
            )
        };
        if status != 0 || size == 0 {
            return;
        }
        let mut ids = vec![0u32; size as usize / 4];
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                ids.as_mut_ptr() as *mut c_void,
                &mut size,
            )
        };
        if status != 0 {
            return;
        }
        ids.truncate(size as usize / 4);
        for id in ids {
            let mut info: AudioUnitParameterInfo = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of::<AudioUnitParameterInfo>() as u32;
            let status = unsafe {
                AudioUnitGetProperty(
                    self.unit,
                    kAudioUnitProperty_ParameterInfo,
                    kAudioUnitScope_Global,
                    id,
                    &mut info as *mut _ as *mut c_void,
                    &mut size,
                )
            };
            if status != 0 {
                continue;
            }
            let name = if info.flags & kAudioUnitParameterFlag_HasCFNameString != 0
                && !info.cfNameString.is_null()
            {
                let s = cf::string(info.cfNameString);
                if info.flags & kAudioUnitParameterFlag_CFNameRelease != 0 {
                    unsafe { cf::CFRelease(info.cfNameString) };
                }
                s
            } else {
                unsafe { CStr::from_ptr(info.name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            };
            let unit_name = if info.unit == kAudioUnitParameterUnit_CustomUnit {
                cf::string(info.unitName)
            } else {
                unit_label(info.unit).to_string()
            };
            self.params.push(AuParam {
                id,
                name,
                unit_name,
                unit: info.unit,
                min: info.minValue,
                max: info.maxValue,
                default: info.defaultValue,
                flags: info.flags,
            });
        }
        let values: Vec<f32> = self.params.iter().map(|p| self.plain_value(p.id)).collect();
        *self.seen.lock().unwrap() = values;
    }

    fn param_of(&self, id: u32) -> Option<&AuParam> {
        self.params.iter().find(|p| p.id == id)
    }

    fn plain_value(&self, id: u32) -> f32 {
        let mut v = 0.0f32;
        unsafe { AudioUnitGetParameter(self.unit, id, kAudioUnitScope_Global, 0, &mut v) };
        v
    }

    fn set_format(&self, scope: u32, channels: u32) -> bool {
        let format = AudioStreamBasicDescription {
            mSampleRate: self.sample_rate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat
                | kAudioFormatFlagIsPacked
                | kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4,
            mChannelsPerFrame: channels,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_StreamFormat,
                scope,
                0,
                &format as *const _ as *const c_void,
                std::mem::size_of::<AudioStreamBasicDescription>() as u32,
            ) == 0
        }
    }

    /// The unit's own Cocoa view, if it ships one. Returns an autoreleased `NSView*`.
    fn cocoa_view(&self) -> Option<*mut c_void> {
        let mut size = 0u32;
        let mut writable = 0u8;
        let status = unsafe {
            AudioUnitGetPropertyInfo(
                self.unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                &mut size,
                &mut writable,
            )
        };
        if status != 0 || (size as usize) < 2 * std::mem::size_of::<usize>() {
            return None;
        }
        let mut buf = vec![0usize; size as usize / std::mem::size_of::<usize>()];
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let url = buf[0] as *mut AnyObject;
        let class_name = buf[1] as *mut AnyObject;
        let view = unsafe {
            let ns_bundle = AnyClass::get(c"NSBundle")?;
            let bundle: *mut AnyObject = send1(
                ns_bundle as *const AnyClass as *mut AnyObject,
                sel!(bundleWithURL:),
                url,
            );
            let class: *mut AnyObject = if bundle.is_null() {
                std::ptr::null_mut()
            } else {
                send1(bundle, sel!(classNamed:), class_name)
            };
            let view = if class.is_null() {
                std::ptr::null_mut()
            } else {
                let factory: *mut AnyObject = send0(class, sel!(alloc));
                let factory: *mut AnyObject = send0(factory, sel!(init));
                let f: unsafe extern "C" fn(*mut AnyObject, Sel, AudioUnit, NSSize) -> *mut AnyObject =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let view = f(
                    factory,
                    sel!(uiViewForAudioUnit:withSize:),
                    self.unit,
                    NSSize::new(400.0, 300.0),
                );
                let _: *mut AnyObject = send0(factory, sel!(release));
                view
            };
            cf::CFRelease(buf[0] as CFTypeRef);
            cf::CFRelease(buf[1] as CFTypeRef);
            view
        };
        (!view.is_null()).then_some(view as *mut c_void)
    }

    /// Apple's generic slider panel for any unit. Returns a +1 retained `NSView*`.
    fn generic_view(&self) -> Option<*mut c_void> {
        unsafe {
            dlopen(
                c"/System/Library/Frameworks/CoreAudioKit.framework/CoreAudioKit".as_ptr(),
                1,
            );
            let class = AnyClass::get(c"AUGenericView")?;
            let view: *mut AnyObject = send0(class as *const AnyClass as *mut AnyObject, sel!(alloc));
            let f: unsafe extern "C" fn(*mut AnyObject, Sel, AudioUnit) -> *mut AnyObject =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let view = f(view, sel!(initWithAudioUnit:), self.unit);
            if view.is_null() {
                return None;
            }
            let g: unsafe extern "C" fn(*mut AnyObject, Sel, bool) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            g(view, sel!(setShowsExpertParameters:), true);
            Some(view as *mut c_void)
        }
    }
}

/// An Audio Unit error as words, with the number for searching. -1 is what the system answers
/// when the unit's code could not even be loaded into this process - for a third-party unit
/// almost always its code signature (the app's library validation) or a missing dependency.
pub(crate) fn status_text(status: i32) -> String {
    let words = match status {
        -1 => "the unit's code could not be loaded into this program - code signature or library \
               validation, or a missing dependency",
        -50 => "invalid parameter",
        -108 => "out of memory",
        -10879 => "invalid property",
        -10878 => "invalid parameter",
        -10877 => "invalid element",
        -10876 => "no connection",
        -10875 => "the unit failed to initialise",
        -10874 => "too many frames to process",
        -10871 => "invalid file",
        -10868 => "audio format not supported",
        -10867 => "the unit is not initialised",
        -10866 => "invalid scope",
        -10865 => "property not writable",
        -10863 => "cannot do this in the current context",
        -10851 => "invalid property value",
        -10850 => "property not in use",
        -10849 => "the unit is already initialised",
        -10848 => "invalid offline render",
        -10847 => "unauthorised (copy protection)",
        -66744 => "the Audio Unit extension was not found",
        -66745 => "the unit did not render in time",
        -66746 => "invalid format",
        -66747 => "the unit did not initialise in time",
        -66748 => "not permitted (sandbox or entitlement)",
        -66749 => "the unit's instance was invalidated (its process went away)",
        -66750 => "too many instances",
        -66751 => "unsupported unit type",
        -66752 => "the unit is registered twice",
        -66754 => "the unit did not start in time",
        _ => "",
    };
    let four: Option<String> = {
        let b = status.to_be_bytes();
        b.iter()
            .all(|c| c.is_ascii_graphic() || *c == b' ')
            .then(|| b.iter().map(|&c| c as char).collect())
    };
    match (words.is_empty(), four) {
        (false, _) => format!("{words}, error {status}"),
        (true, Some(code)) => format!("error '{code}' ({status})"),
        (true, None) => format!("error {status}"),
    }
}

unsafe fn send0(obj: *mut AnyObject, sel: Sel) -> *mut AnyObject {
    let f: unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { f(obj, sel) }
}

unsafe fn send1(obj: *mut AnyObject, sel: Sel, arg: *mut AnyObject) -> *mut AnyObject {
    let f: unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut AnyObject =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { f(obj, sel, arg) }
}

impl PluginInstance for AuPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn params(&self) -> Vec<ParamInfo> {
        self.params
            .iter()
            .map(|p| ParamInfo {
                id: p.id,
                name: p.name.clone(),
                unit: p.unit_name.clone(),
                default: p.normalise(p.default),
                steps: p.steps(),
                automatable: p.flags & kAudioUnitParameterFlag_NonRealTime == 0,
                read_only: p.flags & kAudioUnitParameterFlag_IsWritable == 0,
                is_bypass: false,
            })
            .collect()
    }

    fn param(&self, id: u32) -> f64 {
        match self.param_of(id) {
            Some(p) => p.normalise(self.plain_value(id)),
            None => 0.0,
        }
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        let Some(p) = self.param_of(id) else {
            return String::new();
        };
        let plain = p.plain(value);
        let mut query = AudioUnitParameterStringFromValue {
            inParamID: id,
            inValue: &plain,
            outString: std::ptr::null(),
        };
        let mut size = std::mem::size_of::<AudioUnitParameterStringFromValue>() as u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_ParameterStringFromValue,
                kAudioUnitScope_Global,
                0,
                &mut query as *mut _ as *mut c_void,
                &mut size,
            )
        };
        if status == 0 && !query.outString.is_null() {
            let text = cf::take_string(query.outString);
            if !text.is_empty() {
                return text;
            }
        }
        if p.unit == kAudioUnitParameterUnit_Boolean {
            return if plain >= 0.5 { "On" } else { "Off" }.to_string();
        }
        let digits = if plain.abs() >= 100.0 { 0 } else if plain.abs() >= 10.0 { 1 } else { 2 };
        format!("{plain:.digits$} {}", p.unit_name).trim().to_string()
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(p) = self.param_of(id) else {
            return;
        };
        let plain = p.plain(value);
        unsafe { AudioUnitSetParameter(self.unit, id, kAudioUnitScope_Global, 0, plain, 0) };
        if let Some(i) = self.params.iter().position(|p| p.id == id)
            && let Some(seen) = self.seen.lock().unwrap().get_mut(i)
        {
            *seen = plain;
        }
    }

    fn latency(&self) -> u32 {
        let mut seconds = 0.0f64;
        let mut size = std::mem::size_of::<f64>() as u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_Latency,
                kAudioUnitScope_Global,
                0,
                &mut seconds as *mut f64 as *mut c_void,
                &mut size,
            )
        };
        if status != 0 || !seconds.is_finite() || seconds <= 0.0 {
            return 0;
        }
        (seconds * self.sample_rate).round().min(u32::MAX as f64) as u32
    }

    fn save_state(&self) -> Result<Vec<u8>, String> {
        let mut plist: CFTypeRef = std::ptr::null();
        let mut size = std::mem::size_of::<CFTypeRef>() as u32;
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &mut plist as *mut CFTypeRef as *mut c_void,
                &mut size,
            )
        };
        if status != 0 || plist.is_null() {
            return Err(format!(
                "The plugin did not hand out its state ({}).",
                status_text(status)
            ));
        }
        let bytes = cf::plist_bytes(plist);
        unsafe { cf::CFRelease(plist) };
        bytes.ok_or_else(|| "The plugin's state could not be serialised.".to_string())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let plist = cf::plist_from(data).ok_or_else(|| "The saved state is damaged.".to_string())?;
        let status = unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &plist as *const CFTypeRef as *const c_void,
                std::mem::size_of::<CFTypeRef>() as u32,
            )
        };
        unsafe { cf::CFRelease(plist) };
        if status != 0 {
            return Err(format!(
                "The plugin refused the saved state ({}).",
                status_text(status)
            ));
        }
        let values: Vec<f32> = self.params.iter().map(|p| self.plain_value(p.id)).collect();
        *self.seen.lock().unwrap() = values;
        Ok(())
    }

    fn activate(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn InsertProcessor>, String> {
        if self.processing.load(Ordering::Acquire) {
            return Err("The plugin is already running.".to_string());
        }
        if self.initialized {
            unsafe { AudioUnitUninitialize(self.unit) };
            self.initialized = false;
        }
        self.sample_rate = sample_rate;
        // Stereo in and out first; units that only do mono get mono and the processor fans out.
        let (ins, outs) = [(2, 2), (1, 1), (1, 2), (2, 1)]
            .into_iter()
            .find(|&(i, o)| {
                self.set_format(kAudioUnitScope_Input, i)
                    && self.set_format(kAudioUnitScope_Output, o)
            })
            .ok_or_else(|| {
                format!(
                    "\"{}\" accepts neither stereo nor mono audio.",
                    self.info.name
                )
            })?;
        let frames = max_block as u32;
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_MaximumFramesPerSlice,
                kAudioUnitScope_Global,
                0,
                &frames as *const u32 as *const c_void,
                4,
            );
        }
        let mut input = Box::new(Input {
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
            mono: vec![0.0; max_block],
            frames: 0,
            channels: ins,
        });
        let callback = AURenderCallbackStruct {
            inputProc: render_input,
            inputProcRefCon: &mut *input as *mut Input as *mut c_void,
        };
        let status = unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                0,
                &callback as *const _ as *const c_void,
                std::mem::size_of::<AURenderCallbackStruct>() as u32,
            )
        };
        if status != 0 {
            return Err(format!(
                "\"{}\" does not take audio input ({}).",
                self.info.name,
                status_text(status)
            ));
        }
        let status = unsafe { AudioUnitInitialize(self.unit) };
        if status != 0 {
            return Err(format!(
                "\"{}\" could not be started ({}).",
                self.info.name,
                status_text(status)
            ));
        }
        self.initialized = true;
        unsafe { AudioUnitReset(self.unit, kAudioUnitScope_Global, 0) };
        let transport = PluginTransport::standing(sample_rate);
        self.host_time.store(&transport);
        self.install_host_callbacks();
        self.processing.store(true, Ordering::Release);
        Ok(Box::new(AuProcessor {
            unit: self.unit,
            input,
            outs,
            sample_time: 0.0,
            processing: self.processing.clone(),
            host_time: self.host_time.clone(),
            transport,
        }))
    }

    /// The instrument path: a `MusicDevice` has no audio input, so only the output format is set and
    /// no render callback is installed. MIDI reaches it through `MusicDeviceMIDIEvent` on the audio
    /// thread, right before each `AudioUnitRender` - the way AUGraph and JUCE drive one.
    fn activate_instrument(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn SourceProcessor>, String> {
        if self.info.kind != Kind::Instrument {
            return Err(format!("\"{}\" is not an instrument.", self.info.name));
        }
        if self.processing.load(Ordering::Acquire) {
            return Err("The plugin is already running.".to_string());
        }
        if self.initialized {
            unsafe { AudioUnitUninitialize(self.unit) };
            self.initialized = false;
        }
        self.sample_rate = sample_rate;
        let outs = [2u32, 1]
            .into_iter()
            .find(|&o| self.set_format(kAudioUnitScope_Output, o))
            .ok_or_else(|| {
                format!(
                    "\"{}\" plays neither stereo nor mono at {sample_rate} Hz.",
                    self.info.name
                )
            })?;
        let frames = max_block as u32;
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                kAudioUnitProperty_MaximumFramesPerSlice,
                kAudioUnitScope_Global,
                0,
                &frames as *const u32 as *const c_void,
                4,
            );
        }
        let status = unsafe { AudioUnitInitialize(self.unit) };
        if status != 0 {
            return Err(format!(
                "\"{}\" could not be started ({}).",
                self.info.name,
                status_text(status)
            ));
        }
        self.initialized = true;
        unsafe { AudioUnitReset(self.unit, kAudioUnitScope_Global, 0) };
        let transport = PluginTransport::standing(sample_rate);
        self.host_time.store(&transport);
        self.install_host_callbacks();
        self.processing.store(true, Ordering::Release);
        Ok(Box::new(AuInstrument {
            unit: self.unit,
            outs,
            sample_time: 0.0,
            processing: self.processing.clone(),
            host_time: self.host_time.clone(),
            transport,
        }))
    }

    fn deactivate(&mut self) {
        if self.initialized {
            unsafe { AudioUnitUninitialize(self.unit) };
            self.initialized = false;
        }
    }

    fn has_editor(&self) -> bool {
        true
    }

    fn open_editor(&mut self, title: &str, key: &str) -> Result<(), String> {
        if let Some(window) = &self.editor {
            window.show();
            return Ok(());
        }
        let (view, owned) = match self.cocoa_view() {
            Some(view) => (view, false),
            None => (
                self.generic_view()
                    .ok_or_else(|| "This Audio Unit has no view.".to_string())?,
                true,
            ),
        };
        let mut window = EditorWindow::new(title, key, 400.0, 300.0, false)?;
        let embedded = unsafe { window.embed(view) };
        if owned {
            unsafe { send0(view as *mut AnyObject, sel!(release)) };
        }
        embedded?;
        window.show();
        self.editor = Some(window);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(window) = &self.editor {
            window.hide();
        }
    }

    fn editor_open(&self) -> bool {
        self.editor.as_ref().is_some_and(|w| w.visible())
    }

    fn editor_size(&self) -> Option<(f64, f64)> {
        self.editor.as_ref().map(|w| w.view_size())
    }

    fn editor_report(&self) -> Option<String> {
        self.editor.as_ref().map(|w| w.report())
    }

    fn editor_resize_as_user(&self, width: f64, height: f64) {
        if let Some(w) = &self.editor {
            w.resize_as_user(width, height);
        }
    }

    fn editor_snapshot_png(&self) -> Option<Vec<u8>> {
        self.editor.as_ref().and_then(|w| w.snapshot_png())
    }

    fn take_changed(&mut self) -> Vec<u32> {
        let mut seen = self.seen.lock().unwrap();
        let mut changed = Vec::new();
        for (i, p) in self.params.iter().enumerate() {
            let now = self.plain_value(p.id);
            if let Some(old) = seen.get_mut(i)
                && *old != now
            {
                *old = now;
                changed.push(p.id);
            }
        }
        changed
    }
}

impl Drop for AuPlugin {
    fn drop(&mut self) {
        self.editor = None;
        if self.initialized {
            unsafe { AudioUnitUninitialize(self.unit) };
        }
        unsafe { AudioComponentInstanceDispose(self.unit) };
    }
}

// ---------------------------------------------------------------------------------------------
// The audio side
// ---------------------------------------------------------------------------------------------

/// What the render callback reads. Owned by the processor; its address is the callback's refcon,
/// so it lives in a box that never moves.
struct Input {
    l: Vec<f32>,
    r: Vec<f32>,
    mono: Vec<f32>,
    frames: usize,
    channels: u32,
}

struct AuProcessor {
    unit: AudioUnit,
    input: Box<Input>,
    outs: u32,
    sample_time: f64,
    processing: std::sync::Arc<AtomicBool>,
    host_time: Arc<HostTime>,
    /// The clock for the next render: set by the engine before every call, moved on here after it
    /// for callers that never set it.
    transport: PluginTransport,
}

// The unit is driven from exactly one thread at a time on this side: the audio thread, between
// `activate` and the processor's drop. AudioToolbox allows that.
unsafe impl Send for AuProcessor {}

impl Drop for AuProcessor {
    fn drop(&mut self) {
        self.processing.store(false, Ordering::Release);
    }
}

/// The unit asks for its input. Copies what `process` staged; no allocation, no lock.
unsafe extern "C" fn render_input(
    refcon: *mut c_void,
    _flags: *mut u32,
    _time: *const AudioTimeStamp,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList2,
) -> OSStatus {
    let input = unsafe { &mut *(refcon as *mut Input) };
    let n = (frames as usize).min(input.frames);
    let count = unsafe { (*data).mNumberBuffers } as usize;
    let first = unsafe { std::ptr::addr_of_mut!((*data).mBuffers) } as *mut AudioBuffer;
    for i in 0..count.min(2) {
        let buffer = unsafe { &mut *first.add(i) };
        let source: &mut Vec<f32> = if input.channels == 1 {
            &mut input.mono
        } else if i == 0 {
            &mut input.l
        } else {
            &mut input.r
        };
        if buffer.mData.is_null() {
            buffer.mData = source.as_mut_ptr() as *mut c_void;
            buffer.mDataByteSize = (n * 4) as u32;
        } else {
            let room = buffer.mDataByteSize as usize / 4;
            let m = n.min(room);
            unsafe {
                std::ptr::copy_nonoverlapping(source.as_ptr(), buffer.mData as *mut f32, m);
            }
        }
    }
    0
}

impl InsertProcessor for AuProcessor {
    fn set_transport(&mut self, transport: &PluginTransport) {
        self.host_time.follow(&self.transport, transport);
        self.transport = *transport;
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(self.input.l.len());
        if n == 0 {
            return;
        }
        self.input.l[..n].copy_from_slice(&left[..n]);
        self.input.r[..n].copy_from_slice(&right[..n]);
        if self.input.channels == 1 {
            for i in 0..n {
                self.input.mono[i] = 0.5 * (left[i] + right[i]);
            }
        }
        self.input.frames = n;
        let bytes = (n * 4) as u32;
        let mut list = AudioBufferList2 {
            mNumberBuffers: self.outs,
            mBuffers: [
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: left.as_mut_ptr() as *mut c_void,
                },
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: right.as_mut_ptr() as *mut c_void,
                },
            ],
        };
        let stamp = AudioTimeStamp {
            mSampleTime: self.sample_time,
            mFlags: kAudioTimeStampSampleTimeValid,
            ..Default::default()
        };
        let mut flags = 0u32;
        self.host_time.store(&self.transport);
        let status =
            unsafe { AudioUnitRender(self.unit, &mut flags, &stamp, 0, n as u32, &mut list) };
        self.sample_time += n as f64;
        self.transport = self.transport.advanced(n);
        if status != 0 {
            // Whatever went wrong, the dry signal is a better answer than half a buffer.
            left[..n].copy_from_slice(&self.input.l[..n]);
            right[..n].copy_from_slice(&self.input.r[..n]);
            return;
        }
        // A unit may answer with buffers of its own instead of filling ours.
        let out_l = list.mBuffers[0].mData as *const f32;
        if out_l != left.as_ptr() && !out_l.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(out_l, left.as_mut_ptr(), n) };
        }
        if self.outs == 1 {
            right[..n].copy_from_slice(&left[..n]);
        } else {
            let out_r = list.mBuffers[1].mData as *const f32;
            if out_r != right.as_ptr() && !out_r.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(out_r, right.as_mut_ptr(), n) };
            }
        }
    }
}

/// The audio side of an Audio Unit instrument.
struct AuInstrument {
    unit: AudioUnit,
    outs: u32,
    sample_time: f64,
    processing: std::sync::Arc<AtomicBool>,
    host_time: Arc<HostTime>,
    /// See `AuProcessor::transport`.
    transport: PluginTransport,
}

// Driven from the audio thread only, between `activate_instrument` and the drop - as AuProcessor.
unsafe impl Send for AuInstrument {}

impl Drop for AuInstrument {
    fn drop(&mut self) {
        self.processing.store(false, Ordering::Release);
    }
}

impl SourceProcessor for AuInstrument {
    fn set_transport(&mut self, transport: &PluginTransport) {
        self.host_time.follow(&self.transport, transport);
        self.transport = *transport;
    }

    fn render(&mut self, events: &[BlockEvent], outs: &mut [StereoOut<'_>]) {
        let Some(out) = outs.first_mut() else {
            return;
        };
        let n = out.l.len().min(out.r.len());
        if n == 0 {
            return;
        }
        for e in events {
            let offset = e.offset.min(n as u32 - 1);
            // SAFETY: a plain call on a unit that is initialised for as long as this processor
            // lives; the message bytes are passed by value.
            unsafe {
                MusicDeviceMIDIEvent(
                    self.unit,
                    u32::from(e.msg[0]),
                    u32::from(e.msg[1]),
                    u32::from(e.msg[2]),
                    offset,
                );
            }
        }
        let bytes = (n * 4) as u32;
        let mut list = AudioBufferList2 {
            mNumberBuffers: self.outs,
            mBuffers: [
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: out.l.as_mut_ptr() as *mut c_void,
                },
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: out.r.as_mut_ptr() as *mut c_void,
                },
            ],
        };
        let stamp = AudioTimeStamp {
            mSampleTime: self.sample_time,
            mFlags: kAudioTimeStampSampleTimeValid,
            ..Default::default()
        };
        let mut flags = 0u32;
        self.host_time.store(&self.transport);
        let status =
            unsafe { AudioUnitRender(self.unit, &mut flags, &stamp, 0, n as u32, &mut list) };
        self.sample_time += n as f64;
        self.transport = self.transport.advanced(n);
        if status != 0 {
            out.l[..n].fill(0.0);
            out.r[..n].fill(0.0);
            return;
        }
        // A unit may answer with buffers of its own instead of filling ours.
        let out_l = list.mBuffers[0].mData as *const f32;
        if out_l != out.l.as_ptr() && !out_l.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(out_l, out.l.as_mut_ptr(), n) };
        }
        if self.outs == 1 {
            let (l, r) = (&out.l[..n], &mut out.r[..n]);
            r.copy_from_slice(l);
        } else {
            let out_r = list.mBuffers[1].mData as *const f32;
            if out_r != out.r.as_ptr() && !out_r.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(out_r, out.r.as_mut_ptr(), n) };
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The clock, as an Audio Unit asks for it
// ---------------------------------------------------------------------------------------------

type BeatAndTempoProc = unsafe extern "C" fn(*mut c_void, *mut f64, *mut f64) -> OSStatus;
type MusicalTimeLocationProc =
    unsafe extern "C" fn(*mut c_void, *mut u32, *mut f32, *mut u32, *mut f64) -> OSStatus;
type TransportStateProc =
    unsafe extern "C" fn(*mut c_void, *mut u8, *mut u8, *mut f64, *mut u8, *mut f64, *mut f64)
        -> OSStatus;
type TransportState2Proc = unsafe extern "C" fn(
    *mut c_void,
    *mut u8,
    *mut u8,
    *mut u8,
    *mut f64,
    *mut u8,
    *mut f64,
    *mut f64,
) -> OSStatus;

/// `HostCallbackInfo` from `AudioUnitProperties.h`. `Boolean` is one byte.
#[repr(C)]
pub(super) struct HostCallbackInfo {
    pub(super) host_user_data: *mut c_void,
    pub(super) beat_and_tempo: Option<BeatAndTempoProc>,
    pub(super) musical_time_location: Option<MusicalTimeLocationProc>,
    pub(super) transport_state: Option<TransportStateProc>,
    pub(super) transport_state2: Option<TransportState2Proc>,
}

impl HostCallbackInfo {
    pub(super) fn for_clock(clock: &Arc<HostTime>) -> Self {
        Self {
            host_user_data: Arc::as_ptr(clock) as *mut c_void,
            beat_and_tempo: Some(beat_and_tempo),
            musical_time_location: Some(musical_time_location),
            transport_state: Some(transport_state),
            transport_state2: Some(transport_state2),
        }
    }
}

/// The engine's clock where the host callbacks can read it: atomics, because a unit may ask from
/// its view's thread as well as from inside `AudioUnitRender`. Each field is read on its own, so a
/// reader on another thread can see one block's tempo with the next block's position - harmless,
/// and the audio thread itself, where nearly every question is asked, always sees one block.
#[derive(Debug, Default)]
pub(super) struct HostTime {
    sample_rate: AtomicU64,
    bpm: AtomicU64,
    /// `beats_per_bar << 32 | beat_unit`.
    signature: AtomicU64,
    position: AtomicI64,
    playing: AtomicBool,
    /// The transport started, stopped or jumped since a unit last asked.
    changed: AtomicBool,
}

impl HostTime {
    pub(super) fn store(&self, t: &PluginTransport) {
        self.sample_rate.store(t.sample_rate.to_bits(), Ordering::Relaxed);
        self.bpm.store(t.bpm.to_bits(), Ordering::Relaxed);
        self.signature.store(
            u64::from(t.beats_per_bar) << 32 | u64::from(t.beat_unit),
            Ordering::Relaxed,
        );
        self.position.store(t.position, Ordering::Relaxed);
        self.playing.store(t.playing, Ordering::Relaxed);
    }

    /// The engine sent `next` where the processor expected `expected` (the last clock moved on by
    /// what was rendered since): a start, a stop or a jump is a transport change.
    pub(super) fn follow(&self, expected: &PluginTransport, next: &PluginTransport) {
        if expected.playing != next.playing || expected.position != next.position {
            self.changed.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn load(&self) -> PluginTransport {
        let signature = self.signature.load(Ordering::Relaxed);
        PluginTransport {
            sample_rate: f64::from_bits(self.sample_rate.load(Ordering::Relaxed)),
            bpm: f64::from_bits(self.bpm.load(Ordering::Relaxed)),
            beats_per_bar: (signature >> 32) as u32,
            beat_unit: signature as u32,
            playing: self.playing.load(Ordering::Relaxed),
            position: self.position.load(Ordering::Relaxed),
            continuous: 0,
        }
    }

    fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

/// Write `value` where the unit asked for it; every out pointer of these callbacks may be null.
unsafe fn put<T>(out: *mut T, value: T) {
    if !out.is_null() {
        unsafe { *out = value };
    }
}

unsafe fn clock<'a>(user: *mut c_void) -> Option<&'a HostTime> {
    unsafe { (user as *const HostTime).as_ref() }
}

/// Current beat (in quarter notes since bar 1) and tempo.
pub(super) unsafe extern "C" fn beat_and_tempo(
    user: *mut c_void,
    beat: *mut f64,
    tempo: *mut f64,
) -> OSStatus {
    let Some(clock) = (unsafe { clock(user) }) else {
        return -50; // paramErr
    };
    let t = clock.load();
    unsafe {
        put(beat, t.quarters());
        put(tempo, t.bpm);
    }
    0
}

/// Samples to the next quarter note, the time signature, and the downbeat of the current bar in
/// quarter notes.
pub(super) unsafe extern "C" fn musical_time_location(
    user: *mut c_void,
    to_next_beat: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    downbeat: *mut f64,
) -> OSStatus {
    let Some(clock) = (unsafe { clock(user) }) else {
        return -50;
    };
    let t = clock.load();
    let q = t.quarters();
    let next = if t.playing {
        ((q.ceil() - q) * t.samples_per_quarter()).round().max(0.0) as u32
    } else {
        0
    };
    unsafe {
        put(to_next_beat, next);
        put(numerator, t.beats_per_bar as f32);
        put(denominator, t.beat_unit);
        put(downbeat, t.bar_start_quarters());
    }
    0
}

/// Playing, whether that changed since the last question, the position in samples since bar 1,
/// and no cycle - each track loops at its own length, so there is no single loop range.
pub(super) unsafe extern "C" fn transport_state(
    user: *mut c_void,
    playing: *mut u8,
    changed: *mut u8,
    sample: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus {
    unsafe {
        transport_state2(
            user,
            playing,
            std::ptr::null_mut(),
            changed,
            sample,
            cycling,
            cycle_start,
            cycle_end,
        )
    }
}

/// [`transport_state`] plus "recording", which is never set: a looper records into its own
/// loops, not into the plugin's idea of a timeline.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn transport_state2(
    user: *mut c_void,
    playing: *mut u8,
    recording: *mut u8,
    changed: *mut u8,
    sample: *mut f64,
    cycling: *mut u8,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus {
    let Some(clock) = (unsafe { clock(user) }) else {
        return -50;
    };
    let t = clock.load();
    let was_changed = if changed.is_null() {
        false
    } else {
        clock.take_changed()
    };
    unsafe {
        put(playing, u8::from(t.playing));
        put(recording, 0);
        put(changed, u8::from(was_changed));
        put(sample, t.position as f64);
        put(cycling, 0);
        put(cycle_start, 0.0);
        put(cycle_end, 0.0);
    }
    0
}
