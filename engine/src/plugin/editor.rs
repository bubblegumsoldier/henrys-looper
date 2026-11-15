//! The window a plugin's own editor lives in: a plain NSWindow whose content view gets the plugin's
//! NSView as its only subview.
//!
//! Main thread only - AppKit refuses anything else, and [`MainThreadMarker`] makes that a checked
//! error instead of a crash. Closing the window with its close button only **hides** it
//! (`releasedWhenClosed = NO`): the plugin's view stays attached, so opening it again is
//! instantaneous and nothing has to be torn down behind the user's back. The view is removed and
//! the window released when the plugin is unloaded.

use std::ffi::c_void;

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// A plugin editor window.
pub struct EditorWindow {
    window: Retained<NSWindow>,
    view: Option<Retained<NSView>>,
}

impl EditorWindow {
    /// A new, hidden window with a content area of `width` x `height` points.
    pub fn new(title: &str, width: f64, height: f64, resizable: bool) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "A plugin window can only be opened on the main thread.".to_string())?;
        let mut style =
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width.max(50.0), height.max(30.0)));
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(title));
        window.center();
        Ok(Self { window, view: None })
    }

    /// The content view, as the raw `NSView*` VST3's `IPlugView::attached` wants.
    pub fn content_view(&self) -> *mut c_void {
        match self.window.contentView() {
            Some(view) => Retained::as_ptr(&view) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    /// Put a view the plugin created into the window and size the window to it.
    ///
    /// # Safety
    /// `view` must be a valid `NSView*`.
    pub unsafe fn embed(&mut self, view: *mut c_void) -> Result<(), String> {
        let view = unsafe { Retained::retain(view as *mut NSView) }
            .ok_or_else(|| "The plugin returned no view.".to_string())?;
        let content = self
            .window
            .contentView()
            .ok_or_else(|| "The window has no content view.".to_string())?;
        let size = view.frame().size;
        view.setFrameOrigin(NSPoint::new(0.0, 0.0));
        content.addSubview(&view);
        self.window.setContentSize(size);
        self.view = Some(view);
        Ok(())
    }

    /// The window's content area changes to `width` x `height` points.
    pub fn resize(&self, width: f64, height: f64) {
        self.window.setContentSize(NSSize::new(width, height));
        if let Some(view) = &self.view {
            view.setFrameSize(NSSize::new(width, height));
        }
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn hide(&self) {
        self.window.orderOut(None);
    }

    pub fn visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Size of the embedded plugin view in points, `(0, 0)` without one.
    pub fn view_size(&self) -> (f64, f64) {
        self.view
            .as_ref()
            .map(|v| (v.frame().size.width, v.frame().size.height))
            .unwrap_or((0.0, 0.0))
    }
}

/// Hand pending window events to AppKit for up to `seconds`. For the CLI, which has no run loop of
/// its own; inside the app, Tauri runs the loop and this is never called.
pub fn pump_events(seconds: f64) {
    use objc2_app_kit::{NSApplication, NSEventMask};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let until = NSDate::dateWithTimeIntervalSinceNow(seconds);
    while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
        NSEventMask::Any,
        Some(&until),
        unsafe { NSDefaultRunLoopMode },
        true,
    ) {
        app.sendEvent(&event);
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        if let Some(view) = self.view.take() {
            view.removeFromSuperview();
        }
        self.window.orderOut(None);
        self.window.close();
    }
}
