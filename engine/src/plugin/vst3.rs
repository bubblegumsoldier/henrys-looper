//! VST3 through the `vst3` bindings.
//!
//! # Life of an instance
//!
//! ```text
//!  bundle ─► bundleEntry ─► GetPluginFactory ─► createInstance(IComponent) ─► initialize(host)
//!        ─► IEditController (same object, or its own class + initialize + IConnectionPoint both ways)
//!        ─► setComponentHandler ─► component state into the controller
//!        ─► setBusArrangements(stereo) ─► setupProcessing ─► setActive ─► setProcessing ─► process …
//! ```
//!
//! The component and the controller are driven from the thread that loaded them (the main thread
//! in the app); `process` from the audio thread. Parameter changes cross over through a lock-free
//! queue: the host's own changes and those of the plugin's editor (`IComponentHandler::performEdit`)
//! go into the same queue and reach the processor as `IParameterChanges` at the start of the next
//! block - the VST3 way of telling the audio half what the edit half did.
//!
//! The module is never unloaded once loaded (see [`super::cf::Bundle`]).

#![allow(non_upper_case_globals, non_snake_case)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use rtrb::{Consumer, Producer, RingBuffer};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use super::cf::Bundle;
use super::editor::EditorWindow;
use super::{Format, Kind, ParamInfo, PluginInfo, PluginInstance};
use crate::engine::insert::InsertProcessor;

/// Parameter changes queued between two blocks. A knob turned during one block produces one value,
/// so this only has to cover many *different* parameters at once.
const PARAM_QUEUE: usize = 1024;
/// Distinct parameters one block can carry to the processor.
const CHANGES_PER_BLOCK: usize = 64;

// ---------------------------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------------------------

struct Module {
    _bundle: Bundle,
    factory: ComPtr<IPluginFactory>,
}

// The factory is only used from the thread that loads plugins; the map just keeps it alive.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

fn modules() -> &'static Mutex<HashMap<PathBuf, Arc<Module>>> {
    static MODULES: OnceLock<Mutex<HashMap<PathBuf, Arc<Module>>>> = OnceLock::new();
    MODULES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn module(path: &Path) -> Result<Arc<Module>, String> {
    if let Some(m) = modules().lock().unwrap().get(path) {
        return Ok(m.clone());
    }
    let bundle = Bundle::open(path)?;
    if let Some(entry) = bundle.symbol("bundleEntry") {
        let entry: unsafe extern "C" fn(*mut c_void) -> bool = unsafe { std::mem::transmute(entry) };
        if !unsafe { entry(bundle.raw) } {
            return Err(format!("The plugin refused to initialise: {}", path.display()));
        }
    }
    let get = bundle
        .symbol("GetPluginFactory")
        .ok_or_else(|| format!("Not a VST3 plugin (no factory): {}", path.display()))?;
    let get: unsafe extern "C" fn() -> *mut IPluginFactory = unsafe { std::mem::transmute(get) };
    let factory = unsafe { ComPtr::from_raw(get()) }
        .ok_or_else(|| format!("The plugin returned no factory: {}", path.display()))?;
    let module = Arc::new(Module {
        _bundle: bundle,
        factory,
    });
    modules()
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), module.clone());
    Ok(module)
}

fn c_text(chars: &[c_char]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    let bytes: Vec<u8> = chars[..end].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn w_text(chars: &[TChar]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end].iter().map(|&c| c as u16).collect::<Vec<u16>>())
}

fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|&b| format!("{:02X}", b as u8)).collect()
}

fn cid_parse(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()? as c_char;
    }
    Some(out)
}

const AUDIO_EFFECT_CLASS: &str = "Audio Module Class";

/// Every audio processor class in a bundle. Runs the plugin's module code - call it in the probe
/// child, not in the program.
pub fn enumerate(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let module = module(path)?;
    let factory = &module.factory;
    let mut vendor = String::new();
    let mut finfo: PFactoryInfo = unsafe { std::mem::zeroed() };
    if unsafe { factory.getFactoryInfo(&mut finfo) } == kResultOk {
        vendor = c_text(&finfo.vendor);
    }
    let factory2 = factory.cast::<IPluginFactory2>();
    let count = unsafe { factory.countClasses() };
    let mut out = Vec::new();
    for i in 0..count {
        let (cid, category, name, sub, class_vendor, version) = match &factory2 {
            Some(f2) => {
                let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
                if unsafe { f2.getClassInfo2(i, &mut info) } != kResultOk {
                    continue;
                }
                (
                    info.cid,
                    c_text(&info.category),
                    c_text(&info.name),
                    c_text(&info.subCategories),
                    c_text(&info.vendor),
                    c_text(&info.version),
                )
            }
            None => {
                let mut info: PClassInfo = unsafe { std::mem::zeroed() };
                if unsafe { factory.getClassInfo(i, &mut info) } != kResultOk {
                    continue;
                }
                (
                    info.cid,
                    c_text(&info.category),
                    c_text(&info.name),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            }
        };
        if category != AUDIO_EFFECT_CLASS {
            continue;
        }
        let kind = if sub.split('|').any(|s| s == "Instrument") {
            Kind::Instrument
        } else {
            Kind::Effect
        };
        out.push(PluginInfo {
            id: format!("vst3:{}", cid_hex(&cid)),
            format: Format::Vst3,
            name,
            vendor: if class_vendor.is_empty() { vendor.clone() } else { class_vendor },
            version,
            category: sub,
            kind,
            path: Some(path.display().to_string()),
        });
    }
    if out.is_empty() {
        return Err("The bundle contains no audio plugin.".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Host objects the plugin talks to
// ---------------------------------------------------------------------------------------------

struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let name = unsafe { &mut *name };
        for (i, c) in "Henrys Looper".encode_utf16().chain(std::iter::once(0)).enumerate() {
            name[i] = c as TChar;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        let cid: [u8; 16] = unsafe { *cid }.map(|c| c as u8);
        let instance = if cid == IMessage::IID {
            ComWrapper::new(Message::new())
                .to_com_ptr::<IMessage>()
                .map(|p| p.into_raw() as *mut c_void)
        } else if cid == IAttributeList::IID {
            ComWrapper::new(AttributeList::default())
                .to_com_ptr::<IAttributeList>()
                .map(|p| p.into_raw() as *mut c_void)
        } else {
            None
        };
        match instance {
            Some(p) => {
                unsafe { *obj = p };
                kResultOk
            }
            None => {
                unsafe { *obj = std::ptr::null_mut() };
                kNoInterface
            }
        }
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<TChar>),
    Bin(Vec<u8>),
}

#[derive(Default)]
struct AttributeList {
    map: RefCell<HashMap<String, Attr>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

fn attr_key(id: IAttributeList_::AttrID) -> String {
    if id.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned()
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        self.map.borrow_mut().insert(attr_key(id), Attr::Int(value));
        kResultOk
    }

    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Int(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        self.map.borrow_mut().insert(attr_key(id), Attr::Float(value));
        kResultOk
    }

    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Float(v)) => {
                unsafe { *value = *v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let mut v = Vec::new();
        if !string.is_null() {
            let mut i = 0;
            loop {
                let c = unsafe { *string.add(i) };
                if c == 0 {
                    break;
                }
                v.push(c);
                i += 1;
            }
        }
        self.map.borrow_mut().insert(attr_key(id), Attr::Str(v));
        kResultOk
    }

    unsafe fn getString(
        &self,
        id: IAttributeList_::AttrID,
        string: *mut TChar,
        sizeInBytes: uint32,
    ) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Str(v)) => {
                let room = sizeInBytes as usize / std::mem::size_of::<TChar>();
                if room == 0 {
                    return kResultFalse;
                }
                let n = v.len().min(room - 1);
                unsafe {
                    std::ptr::copy_nonoverlapping(v.as_ptr(), string, n);
                    *string.add(n) = 0;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *const c_void,
        sizeInBytes: uint32,
    ) -> tresult {
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, sizeInBytes as usize) }.to_vec()
        };
        self.map.borrow_mut().insert(attr_key(id), Attr::Bin(bytes));
        kResultOk
    }

    unsafe fn getBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *mut *const c_void,
        sizeInBytes: *mut uint32,
    ) -> tresult {
        match self.map.borrow().get(&attr_key(id)) {
            Some(Attr::Bin(v)) => {
                unsafe {
                    *data = v.as_ptr() as *const c_void;
                    *sizeInBytes = v.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

struct Message {
    id: RefCell<CString>,
    attributes: ComPtr<IAttributeList>,
}

impl Message {
    fn new() -> Self {
        Self {
            id: RefCell::new(CString::default()),
            attributes: ComWrapper::new(AttributeList::default())
                .to_com_ptr::<IAttributeList>()
                .expect("IAttributeList"),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.borrow().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = if id.is_null() {
            CString::default()
        } else {
            unsafe { CStr::from_ptr(id) }.to_owned()
        };
        *self.id.borrow_mut() = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes.as_ptr()
    }
}

/// A growable byte stream - how VST3 hands out and takes back state.
#[derive(Default)]
struct Stream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl Stream {
    fn with(data: &[u8]) -> ComWrapper<Stream> {
        ComWrapper::new(Stream {
            data: RefCell::new(data.to_vec()),
            pos: Cell::new(0),
        })
    }
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, numBytes: int32, numBytesRead: *mut int32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = (numBytes.max(0) as usize).min(data.len() - pos);
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer as *mut u8, n) };
        self.pos.set(pos + n);
        if !numBytesRead.is_null() {
            unsafe { *numBytesRead = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        numBytes: int32,
        numBytesWritten: *mut int32,
    ) -> tresult {
        let n = numBytes.max(0) as usize;
        let src = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        data[pos..pos + n].copy_from_slice(src);
        self.pos.set(pos + n);
        if !numBytesWritten.is_null() {
            unsafe { *numBytesWritten = n as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode {
            0 => 0,
            1 => self.pos.get() as i64,
            2 => len,
            _ => return kInvalidArgument,
        };
        let new = (base + pos).max(0);
        self.pos.set(new as usize);
        if !result.is_null() {
            unsafe { *result = new };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if !pos.is_null() {
            unsafe { *pos = self.pos.get() as int64 };
        }
        kResultOk
    }
}

/// What the plugin's editor reports back.
struct Handler {
    queue: Arc<Mutex<Producer<(u32, f64)>>>,
    changed: Rc<RefCell<Vec<u32>>>,
    latency_changed: Rc<Cell<bool>>,
}

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, valueNormalized: ParamValue) -> tresult {
        let _ = self.queue.lock().unwrap().push((id, valueNormalized));
        let mut changed = self.changed.borrow_mut();
        if !changed.contains(&id) {
            changed.push(id);
        }
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        if flags & RestartFlags_::kLatencyChanged as int32 != 0 {
            self.latency_changed.set(true);
        }
        kResultOk
    }
}

/// Resizes the editor window when the plugin asks.
struct Frame {
    window: Rc<RefCell<Option<EditorWindow>>>,
}

impl Class for Frame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, newSize: *mut ViewRect) -> tresult {
        if newSize.is_null() {
            return kInvalidArgument;
        }
        let rect = unsafe { *newSize };
        if let Some(window) = self.window.borrow().as_ref() {
            window.resize(
                (rect.right - rect.left) as f64,
                (rect.bottom - rect.top) as f64,
            );
        }
        if let Some(view) = unsafe { ComRef::from_raw(view) } {
            let mut rect = rect;
            unsafe { view.onSize(&mut rect) };
        }
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter changes for one block, preallocated
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct ValueQueue {
    id: Cell<ParamID>,
    value: Cell<f64>,
}

impl Class for ValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        1
    }

    unsafe fn getPoint(
        &self,
        index: int32,
        sampleOffset: *mut int32,
        value: *mut ParamValue,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        unsafe {
            if !sampleOffset.is_null() {
                *sampleOffset = 0;
            }
            if !value.is_null() {
                *value = self.value.get();
            }
        }
        kResultOk
    }

    unsafe fn addPoint(&self, _offset: int32, _value: ParamValue, _index: *mut int32) -> tresult {
        kResultFalse
    }
}

struct Changes {
    queues: Vec<ComPtr<IParamValueQueue>>,
    wrappers: Vec<ComWrapper<ValueQueue>>,
    count: Cell<usize>,
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl Changes {
    fn new() -> Self {
        let wrappers: Vec<ComWrapper<ValueQueue>> = (0..CHANGES_PER_BLOCK)
            .map(|_| ComWrapper::new(ValueQueue::default()))
            .collect();
        let queues = wrappers
            .iter()
            .map(|w| w.to_com_ptr::<IParamValueQueue>().expect("IParamValueQueue"))
            .collect();
        Self {
            queues,
            wrappers,
            count: Cell::new(0),
        }
    }

    /// Add or overwrite the value of one parameter for this block.
    fn set(&self, id: u32, value: f64) {
        let n = self.count.get();
        for w in &self.wrappers[..n] {
            if w.id.get() == id {
                w.value.set(value);
                return;
            }
        }
        if n < self.wrappers.len() {
            self.wrappers[n].id.set(id);
            self.wrappers[n].value.set(value);
            self.count.set(n + 1);
        }
    }
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> int32 {
        self.count.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        match self.queues.get(index as usize) {
            Some(q) if (index as usize) < self.count.get() => q.as_ptr(),
            _ => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(&self, _id: *const ParamID, _index: *mut int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

// ---------------------------------------------------------------------------------------------
// The instance
// ---------------------------------------------------------------------------------------------

pub struct Vst3Plugin {
    info: PluginInfo,
    _module: Arc<Module>,
    component: ComPtr<IComponent>,
    controller: Option<ComPtr<IEditController>>,
    /// Only set when component and controller are two objects that were connected.
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    separate_controller: bool,
    processor: ComPtr<IAudioProcessor>,
    queue: Arc<Mutex<Producer<(u32, f64)>>>,
    /// The consumer half, waiting for the next `activate`.
    pending_consumer: Option<Consumer<(u32, f64)>>,
    consumer_home: Arc<Mutex<Option<Consumer<(u32, f64)>>>>,
    changed: Rc<RefCell<Vec<u32>>>,
    /// Set by `restartComponent(kLatencyChanged)`. The host re-reads [`PluginInstance::latency`]
    /// on every poll anyway, so this is informational.
    _latency_changed: Rc<Cell<bool>>,
    _handler: ComWrapper<Handler>,
    ins: usize,
    outs: usize,
    has_input: bool,
    active: bool,
    view: Option<ComPtr<IPlugView>>,
    window: Rc<RefCell<Option<EditorWindow>>>,
    _frame: Option<ComWrapper<Frame>>,
}

pub fn load(info: &PluginInfo) -> Result<Box<dyn PluginInstance>, String> {
    let path = info
        .path
        .as_deref()
        .ok_or_else(|| format!("No bundle known for {}.", info.name))?;
    let hex = info
        .id
        .strip_prefix("vst3:")
        .ok_or_else(|| format!("Not a VST3 id: {}", info.id))?;
    let cid = cid_parse(hex).ok_or_else(|| format!("Damaged VST3 id: {}", info.id))?;
    let module = module(Path::new(path))?;
    let host = ComWrapper::new(HostApp)
        .to_com_ptr::<FUnknown>()
        .expect("FUnknown");

    let component: ComPtr<IComponent> = unsafe {
        let mut obj: *mut c_void = std::ptr::null_mut();
        let r = module.factory.createInstance(
            cid.as_ptr(),
            IComponent::IID.as_ptr() as FIDString,
            &mut obj,
        );
        if r != kResultOk {
            return Err(format!("\"{}\" could not be created (error {r}).", info.name));
        }
        ComPtr::from_raw(obj as *mut IComponent)
            .ok_or_else(|| format!("\"{}\" could not be created.", info.name))?
    };
    if unsafe { component.initialize(host.as_ptr()) } != kResultOk {
        return Err(format!("\"{}\" failed to initialise.", info.name));
    }

    let mut separate_controller = false;
    let controller: Option<ComPtr<IEditController>> = match component.cast::<IEditController>() {
        Some(c) => Some(c),
        None => unsafe {
            let mut ccid: TUID = [0; 16];
            if component.getControllerClassId(&mut ccid) == kResultOk {
                let mut obj: *mut c_void = std::ptr::null_mut();
                let r = module.factory.createInstance(
                    ccid.as_ptr(),
                    IEditController::IID.as_ptr() as FIDString,
                    &mut obj,
                );
                match ComPtr::from_raw(obj as *mut IEditController) {
                    Some(c) if r == kResultOk => {
                        if c.initialize(host.as_ptr()) == kResultOk {
                            separate_controller = true;
                            Some(c)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            } else {
                None
            }
        },
    };

    let mut connections = None;
    if separate_controller
        && let Some(ctrl) = &controller
        && let (Some(a), Some(b)) = (
            component.cast::<IConnectionPoint>(),
            ctrl.cast::<IConnectionPoint>(),
        )
    {
        unsafe {
            a.connect(b.as_ptr());
            b.connect(a.as_ptr());
        }
        connections = Some((a, b));
    }

    let (producer, consumer) = RingBuffer::<(u32, f64)>::new(PARAM_QUEUE);
    let queue = Arc::new(Mutex::new(producer));
    let changed = Rc::new(RefCell::new(Vec::new()));
    let latency_changed = Rc::new(Cell::new(false));
    let handler = ComWrapper::new(Handler {
        queue: queue.clone(),
        changed: changed.clone(),
        latency_changed: latency_changed.clone(),
    });
    if let Some(ctrl) = &controller {
        let h = handler
            .to_com_ptr::<IComponentHandler>()
            .expect("IComponentHandler");
        unsafe { ctrl.setComponentHandler(h.as_ptr()) };
        // The controller learns the component's current state, so its parameters show the truth.
        let stream = ComWrapper::new(Stream::default());
        let s = stream.to_com_ptr::<IBStream>().expect("IBStream");
        if unsafe { component.getState(s.as_ptr()) } == kResultOk {
            unsafe {
                s.seek(0, 0, std::ptr::null_mut());
                ctrl.setComponentState(s.as_ptr());
            }
        }
    }

    let processor = component
        .cast::<IAudioProcessor>()
        .ok_or_else(|| format!("\"{}\" processes no audio.", info.name))?;

    let audio = MediaTypes_::kAudio as MediaType;
    let input = BusDirections_::kInput as BusDirection;
    let output = BusDirections_::kOutput as BusDirection;
    let n_in = unsafe { component.getBusCount(audio, input) }.max(0);
    let n_out = unsafe { component.getBusCount(audio, output) }.max(0);
    if n_out == 0 {
        return Err(format!("\"{}\" has no audio output.", info.name));
    }
    // Main buses on, everything else (side-chains, extra outputs) off.
    for i in 0..n_in {
        unsafe { component.activateBus(audio, input, i, (i == 0) as TBool) };
    }
    for i in 0..n_out {
        unsafe { component.activateBus(audio, output, i, (i == 0) as TBool) };
    }
    let (ins, outs) = arrange(&processor, n_in, n_out);

    Ok(Box::new(Vst3Plugin {
        info: info.clone(),
        _module: module,
        component,
        controller,
        connections,
        separate_controller,
        processor,
        queue,
        pending_consumer: Some(consumer),
        consumer_home: Arc::new(Mutex::new(None)),
        changed,
        _latency_changed: latency_changed,
        _handler: handler,
        ins,
        outs,
        has_input: n_in > 0,
        active: false,
        view: None,
        window: Rc::new(RefCell::new(None)),
        _frame: None,
    }))
}

/// Ask for stereo on the main buses; settle for mono where the plugin insists. Returns the channel
/// counts of the main input and output.
fn arrange(processor: &ComPtr<IAudioProcessor>, n_in: i32, n_out: i32) -> (usize, usize) {
    let current = |dir: BusDirection, i: i32| {
        let mut arr: SpeakerArrangement = 0;
        unsafe { processor.getBusArrangement(dir, i, &mut arr) };
        arr
    };
    let input = BusDirections_::kInput as BusDirection;
    let output = BusDirections_::kOutput as BusDirection;
    let mut ins: Vec<SpeakerArrangement> = (0..n_in).map(|i| current(input, i)).collect();
    let mut outs: Vec<SpeakerArrangement> = (0..n_out).map(|i| current(output, i)).collect();
    for (main_in, main_out) in [
        (SpeakerArr::kStereo, SpeakerArr::kStereo),
        (SpeakerArr::kMono, SpeakerArr::kStereo),
        (SpeakerArr::kMono, SpeakerArr::kMono),
    ] {
        if let Some(first) = ins.first_mut() {
            *first = main_in;
        }
        outs[0] = main_out;
        let ok = unsafe {
            processor.setBusArrangements(
                ins.as_mut_ptr(),
                ins.len() as i32,
                outs.as_mut_ptr(),
                outs.len() as i32,
            )
        } == kResultTrue;
        if ok {
            break;
        }
    }
    // Whatever was agreed, read it back rather than trust the attempt.
    let channels = |arr: SpeakerArrangement| (arr.count_ones() as usize).clamp(1, 2);
    let ins = if n_in > 0 { channels(current(input, 0)) } else { 0 };
    let outs = channels(current(output, 0));
    (ins, outs)
}

impl Vst3Plugin {
    fn params_raw(&self) -> Vec<ParameterInfo> {
        let Some(ctrl) = &self.controller else {
            return Vec::new();
        };
        let n = unsafe { ctrl.getParameterCount() };
        (0..n)
            .filter_map(|i| {
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                (unsafe { ctrl.getParameterInfo(i, &mut info) } == kResultOk).then_some(info)
            })
            .collect()
    }

    fn close_view(&mut self) {
        if let Some(view) = self.view.take() {
            unsafe {
                view.removed();
                view.setFrame(std::ptr::null_mut());
            }
        }
        self._frame = None;
        self.window.borrow_mut().take();
    }
}

impl PluginInstance for Vst3Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn params(&self) -> Vec<ParamInfo> {
        use ParameterInfo_::ParameterFlags_ as F;
        self.params_raw()
            .into_iter()
            .filter(|p| p.flags & F::kIsHidden as int32 == 0)
            .map(|p| ParamInfo {
                id: p.id,
                name: {
                    let t = w_text(&p.title);
                    if t.is_empty() { w_text(&p.shortTitle) } else { t }
                },
                unit: w_text(&p.units),
                default: p.defaultNormalizedValue,
                steps: p.stepCount.max(0) as u32,
                automatable: p.flags & F::kCanAutomate as int32 != 0,
                read_only: p.flags & F::kIsReadOnly as int32 != 0,
                is_bypass: p.flags & F::kIsBypass as int32 != 0,
            })
            .collect()
    }

    fn param(&self, id: u32) -> f64 {
        match &self.controller {
            Some(c) => unsafe { c.getParamNormalized(id) },
            None => 0.0,
        }
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        let Some(c) = &self.controller else {
            return String::new();
        };
        let mut text: String128 = [0; 128];
        if unsafe { c.getParamStringByValue(id, value, &mut text) } == kResultOk {
            let t = w_text(&text);
            let unit = self
                .params_raw()
                .into_iter()
                .find(|p| p.id == id)
                .map(|p| w_text(&p.units))
                .unwrap_or_default();
            if unit.is_empty() || t.ends_with(&unit) {
                t
            } else {
                format!("{t} {unit}")
            }
        } else {
            super::plain_text(value)
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if let Some(c) = &self.controller {
            unsafe { c.setParamNormalized(id, value) };
        }
        let _ = self.queue.lock().unwrap().push((id, value));
    }

    fn latency(&self) -> u32 {
        unsafe { self.processor.getLatencySamples() }
    }

    fn save_state(&self) -> Result<Vec<u8>, String> {
        let comp = ComWrapper::new(Stream::default());
        let cs = comp.to_com_ptr::<IBStream>().expect("IBStream");
        if unsafe { self.component.getState(cs.as_ptr()) } != kResultOk {
            return Err(format!("\"{}\" did not hand out its state.", self.info.name));
        }
        let ctrl = ComWrapper::new(Stream::default());
        if self.separate_controller
            && let Some(c) = &self.controller
        {
            let s = ctrl.to_com_ptr::<IBStream>().expect("IBStream");
            unsafe { c.getState(s.as_ptr()) };
        }
        let comp = comp.data.borrow();
        let ctrl = ctrl.data.borrow();
        let mut out = Vec::with_capacity(12 + comp.len() + ctrl.len());
        out.extend_from_slice(b"HLV3");
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp);
        out.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctrl);
        Ok(out)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let damaged = || "The saved state is damaged.".to_string();
        if data.get(..4) != Some(b"HLV3") {
            return Err(damaged());
        }
        let read_len = |at: usize| -> Option<usize> {
            Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize)
        };
        let comp_len = read_len(4).ok_or_else(damaged)?;
        let comp = data.get(8..8 + comp_len).ok_or_else(damaged)?;
        let ctrl_len = read_len(8 + comp_len).ok_or_else(damaged)?;
        let ctrl = data
            .get(12 + comp_len..12 + comp_len + ctrl_len)
            .ok_or_else(damaged)?;
        // Changes still waiting for a processor are older than this state and would undo it the
        // moment the plugin is started again.
        for consumer in [
            self.pending_consumer.as_mut(),
            self.consumer_home.lock().unwrap().as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            while consumer.pop().is_ok() {}
        }
        let cs = Stream::with(comp);
        let cp = cs.to_com_ptr::<IBStream>().expect("IBStream");
        if unsafe { self.component.setState(cp.as_ptr()) } != kResultOk {
            return Err(format!("\"{}\" refused the saved state.", self.info.name));
        }
        if let Some(c) = &self.controller {
            let again = Stream::with(comp);
            let ap = again.to_com_ptr::<IBStream>().expect("IBStream");
            unsafe { c.setComponentState(ap.as_ptr()) };
            if self.separate_controller && !ctrl.is_empty() {
                let s = Stream::with(ctrl);
                let sp = s.to_com_ptr::<IBStream>().expect("IBStream");
                unsafe { c.setState(sp.as_ptr()) };
            }
        }
        Ok(())
    }

    fn activate(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn InsertProcessor>, String> {
        if self.active {
            return Err("The plugin is already running.".to_string());
        }
        let consumer = self
            .pending_consumer
            .take()
            .or_else(|| self.consumer_home.lock().unwrap().take())
            .ok_or_else(|| "The plugin is still running elsewhere.".to_string())?;
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            maxSamplesPerBlock: max_block as int32,
            sampleRate: sample_rate,
        };
        if unsafe { self.processor.setupProcessing(&mut setup) } != kResultOk {
            *self.consumer_home.lock().unwrap() = Some(consumer);
            return Err(format!(
                "\"{}\" does not run at {sample_rate} Hz with blocks of {max_block}.",
                self.info.name
            ));
        }
        if unsafe { self.component.setActive(1) } != kResultOk {
            *self.consumer_home.lock().unwrap() = Some(consumer);
            return Err(format!("\"{}\" could not be started.", self.info.name));
        }
        unsafe { self.processor.setProcessing(1) };
        self.active = true;
        let mut context: Box<ProcessContext> = Box::new(unsafe { std::mem::zeroed() });
        context.sampleRate = sample_rate;
        context.state = ProcessContext_::StatesAndFlags_::kPlaying as u32;
        context.tempo = 120.0;
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        Ok(Box::new(Vst3Processor {
            processor: self.processor.clone(),
            consumer: Some(consumer),
            home: self.consumer_home.clone(),
            changes: ComWrapper::new(Changes::new()),
            in_l: vec![0.0; max_block],
            in_r: vec![0.0; max_block],
            in_ptrs: [std::ptr::null_mut(); 2],
            out_ptrs: [std::ptr::null_mut(); 2],
            ins: self.ins,
            outs: self.outs,
            has_input: self.has_input,
            context,
        }))
    }

    fn deactivate(&mut self) {
        if self.active {
            unsafe {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            self.active = false;
        }
    }

    fn has_editor(&self) -> bool {
        self.controller.is_some()
    }

    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        if let Some(window) = self.window.borrow().as_ref() {
            window.show();
            return Ok(());
        }
        let ctrl = self
            .controller
            .as_ref()
            .ok_or_else(|| "This plugin has no editor.".to_string())?;
        let view = unsafe { ComPtr::from_raw(ctrl.createView(c"editor".as_ptr())) }
            .ok_or_else(|| "This plugin has no editor of its own.".to_string())?;
        if unsafe { view.isPlatformTypeSupported(kPlatformTypeNSView) } != kResultTrue {
            return Err("The plugin's editor does not support macOS views.".to_string());
        }
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        };
        unsafe { view.getSize(&mut rect) };
        let resizable = unsafe { view.canResize() } == kResultTrue;
        let window = EditorWindow::new(
            title,
            (rect.right - rect.left) as f64,
            (rect.bottom - rect.top) as f64,
            resizable,
        )?;
        let parent = window.content_view();
        *self.window.borrow_mut() = Some(window);
        let frame = ComWrapper::new(Frame {
            window: self.window.clone(),
        });
        let fp = frame.to_com_ptr::<IPlugFrame>().expect("IPlugFrame");
        unsafe { view.setFrame(fp.as_ptr()) };
        if unsafe { view.attached(parent, kPlatformTypeNSView) } != kResultOk {
            unsafe { view.setFrame(std::ptr::null_mut()) };
            self.window.borrow_mut().take();
            return Err("The plugin's editor could not be opened.".to_string());
        }
        // Some editors only know their real size once attached.
        let mut rect2 = rect;
        if unsafe { view.getSize(&mut rect2) } == kResultOk
            && let Some(w) = self.window.borrow().as_ref()
        {
            w.resize(
                (rect2.right - rect2.left) as f64,
                (rect2.bottom - rect2.top) as f64,
            );
        }
        if let Some(w) = self.window.borrow().as_ref() {
            w.show();
        }
        self.view = Some(view);
        self._frame = Some(frame);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(window) = self.window.borrow().as_ref() {
            window.hide();
        }
    }

    fn editor_open(&self) -> bool {
        self.window.borrow().as_ref().is_some_and(|w| w.visible())
    }

    fn editor_size(&self) -> Option<(f64, f64)> {
        self.window.borrow().as_ref().map(|w| w.view_size())
    }

    fn take_changed(&mut self) -> Vec<u32> {
        std::mem::take(&mut *self.changed.borrow_mut())
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.close_view();
        self.deactivate();
        unsafe {
            if let Some((a, b)) = self.connections.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate_controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The audio side
// ---------------------------------------------------------------------------------------------

struct Vst3Processor {
    processor: ComPtr<IAudioProcessor>,
    consumer: Option<Consumer<(u32, f64)>>,
    /// Where the consumer goes back when this processor is dropped, for the next activation.
    home: Arc<Mutex<Option<Consumer<(u32, f64)>>>>,
    changes: ComWrapper<Changes>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    in_ptrs: [*mut f32; 2],
    out_ptrs: [*mut f32; 2],
    ins: usize,
    outs: usize,
    has_input: bool,
    context: Box<ProcessContext>,
}

// Driven by one thread at a time: the audio thread while it is installed.
unsafe impl Send for Vst3Processor {}

impl Drop for Vst3Processor {
    fn drop(&mut self) {
        if let Some(c) = self.consumer.take()
            && let Ok(mut home) = self.home.lock()
        {
            *home = Some(c);
        }
    }
}

impl InsertProcessor for Vst3Processor {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(self.in_l.len());
        if n == 0 {
            return;
        }
        if self.ins == 1 {
            for i in 0..n {
                self.in_l[i] = 0.5 * (left[i] + right[i]);
            }
        } else {
            self.in_l[..n].copy_from_slice(&left[..n]);
            self.in_r[..n].copy_from_slice(&right[..n]);
        }
        self.changes.count.set(0);
        if let Some(consumer) = self.consumer.as_mut() {
            for _ in 0..CHANGES_PER_BLOCK * 4 {
                match consumer.pop() {
                    Ok((id, value)) => self.changes.set(id, value),
                    Err(_) => break,
                }
            }
        }
        self.in_ptrs = [self.in_l.as_mut_ptr(), self.in_r.as_mut_ptr()];
        self.out_ptrs = [left.as_mut_ptr(), right.as_mut_ptr()];
        let mut input = AudioBusBuffers {
            numChannels: self.ins as int32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: self.in_ptrs.as_mut_ptr(),
            },
        };
        let mut output = AudioBusBuffers {
            numChannels: self.outs as int32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: self.out_ptrs.as_mut_ptr(),
            },
        };
        let changes = self
            .changes
            .as_com_ref::<IParameterChanges>()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null_mut());
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as int32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as int32,
            numSamples: n as int32,
            numInputs: self.has_input as int32,
            numOutputs: 1,
            inputs: if self.has_input { &mut input } else { std::ptr::null_mut() },
            outputs: &mut output,
            inputParameterChanges: changes,
            outputParameterChanges: std::ptr::null_mut(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut *self.context,
        };
        let result = unsafe { self.processor.process(&mut data) };
        self.context.projectTimeSamples += n as i64;
        self.context.continousTimeSamples += n as i64;
        if result != kResultOk {
            // Dry rather than half a buffer of whatever.
            if self.ins == 2 {
                left[..n].copy_from_slice(&self.in_l[..n]);
                right[..n].copy_from_slice(&self.in_r[..n]);
            }
            return;
        }
        if self.outs == 1 {
            right[..n].copy_from_slice(&left[..n]);
        }
    }
}
