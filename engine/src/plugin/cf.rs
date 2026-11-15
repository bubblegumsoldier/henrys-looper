//! The few CoreFoundation calls the plugin hosts need, declared by hand.
//!
//! A handful of stable C functions: strings, data, property lists and bundles. Declaring them here
//! keeps the two hosts free of a binding crate whose types would have to be converted at every call.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDataRef = *const c_void;
pub type CFURLRef = *const c_void;
pub type CFBundleRef = *mut c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFIndex = isize;
pub type Boolean = u8;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFPropertyListImmutable: usize = 0;
pub const kCFPropertyListBinaryFormat_v1_0: isize = 200;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    pub fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    pub fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    pub fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    pub fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    pub fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    pub fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    pub fn CFPropertyListCreateData(
        alloc: CFAllocatorRef,
        plist: CFTypeRef,
        format: isize,
        options: usize,
        error: *mut *mut c_void,
    ) -> CFDataRef;
    pub fn CFPropertyListCreateWithData(
        alloc: CFAllocatorRef,
        data: CFDataRef,
        options: usize,
        format: *mut isize,
        error: *mut *mut c_void,
    ) -> CFTypeRef;
    pub fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const u8,
        len: CFIndex,
        is_directory: Boolean,
    ) -> CFURLRef;
    pub fn CFBundleCreate(alloc: CFAllocatorRef, url: CFURLRef) -> CFBundleRef;
    pub fn CFBundleLoadExecutable(bundle: CFBundleRef) -> Boolean;
    pub fn CFBundleLoadExecutableAndReturnError(
        bundle: CFBundleRef,
        error: *mut *mut c_void,
    ) -> Boolean;
    pub fn CFErrorCopyDescription(err: *mut c_void) -> CFStringRef;
    pub fn CFErrorCopyUserInfo(err: *mut c_void) -> CFTypeRef;
    pub fn CFDictionaryGetValue(dict: CFTypeRef, key: *const c_void) -> *const c_void;
    pub fn CFBundleGetFunctionPointerForName(
        bundle: CFBundleRef,
        name: CFStringRef,
    ) -> *mut c_void;
}

/// A CoreFoundation string as a Rust one. Does not release `s`.
pub fn string(s: CFStringRef) -> String {
    if s.is_null() {
        return String::new();
    }
    unsafe {
        let len = CFStringGetLength(s);
        let size = CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) + 1;
        let mut buf = vec![0 as c_char; size.max(1) as usize];
        if CFStringGetCString(s, buf.as_mut_ptr(), size, kCFStringEncodingUTF8) == 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// A CoreFoundation string as a Rust one, releasing it.
pub fn take_string(s: CFStringRef) -> String {
    let out = string(s);
    if !s.is_null() {
        unsafe { CFRelease(s) };
    }
    out
}

/// A new CoreFoundation string; the caller releases it.
pub fn new_string(text: &str) -> CFStringRef {
    let c = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) }
}

/// Serialise a property list to binary plist bytes. Does not release `plist`.
pub fn plist_bytes(plist: CFTypeRef) -> Option<Vec<u8>> {
    unsafe {
        let data = CFPropertyListCreateData(
            std::ptr::null(),
            plist,
            kCFPropertyListBinaryFormat_v1_0,
            0,
            std::ptr::null_mut(),
        );
        if data.is_null() {
            return None;
        }
        let len = CFDataGetLength(data) as usize;
        let ptr = CFDataGetBytePtr(data);
        let out = std::slice::from_raw_parts(ptr, len).to_vec();
        CFRelease(data);
        Some(out)
    }
}

/// Parse plist bytes back into a property list; the caller releases it.
pub fn plist_from(bytes: &[u8]) -> Option<CFTypeRef> {
    unsafe {
        let data = CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex);
        if data.is_null() {
            return None;
        }
        let plist = CFPropertyListCreateWithData(
            std::ptr::null(),
            data,
            kCFPropertyListImmutable,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        CFRelease(data);
        (!plist.is_null()).then_some(plist)
    }
}

/// What `CFBundleLoadExecutableAndReturnError` says, with the loader's own reason: the user-facing
/// description alone is "damaged or missing necessary resources" for nearly everything, while the
/// debug description carries dyld's message ("code signature ... not valid for use in process",
/// "incompatible architecture", "library not loaded"). Releases `error`.
unsafe fn load_error_text(error: *mut c_void) -> String {
    if error.is_null() {
        return "no reason given".to_string();
    }
    unsafe {
        let mut text = take_string(CFErrorCopyDescription(error));
        let info = CFErrorCopyUserInfo(error);
        if !info.is_null() {
            let key = new_string("NSDebugDescription");
            let debug = CFDictionaryGetValue(info, key);
            CFRelease(key);
            let debug = string(debug);
            if !debug.is_empty() {
                text = format!("{text} - {debug}");
            }
            CFRelease(info);
        }
        CFRelease(error);
        text
    }
}

/// A loaded bundle. Never unloaded: unloading plugin code that may still have threads or
/// callbacks registered is the classic way to crash a host on the way out.
pub struct Bundle {
    pub raw: CFBundleRef,
}

unsafe impl Send for Bundle {}
unsafe impl Sync for Bundle {}

impl Bundle {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = path.as_os_str().as_encoded_bytes();
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as CFIndex,
                1,
            );
            if url.is_null() {
                return Err(format!("Invalid path: {}", path.display()));
            }
            let raw = CFBundleCreate(std::ptr::null(), url);
            CFRelease(url);
            if raw.is_null() {
                return Err(format!("Not a bundle: {}", path.display()));
            }
            let mut error: *mut c_void = std::ptr::null_mut();
            if CFBundleLoadExecutableAndReturnError(raw, &mut error) == 0 {
                return Err(format!(
                    "The plugin's code could not be loaded: {} ({})",
                    load_error_text(error),
                    path.display()
                ));
            }
            Ok(Self { raw })
        }
    }

    pub fn symbol(&self, name: &str) -> Option<*mut c_void> {
        let cf = new_string(name);
        let ptr = unsafe { CFBundleGetFunctionPointerForName(self.raw, cf) };
        unsafe { CFRelease(cf) };
        (!ptr.is_null()).then_some(ptr)
    }
}
