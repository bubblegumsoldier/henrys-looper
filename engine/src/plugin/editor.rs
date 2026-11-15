//! The window a plugin's own editor lives in: a plain, opaque NSWindow with the standard title bar,
//! whose content area is **exactly** the plugin's view - no margin, nothing cut off.
//!
//! ```text
//!  NSWindow (Titled | Closable | Miniaturizable [| Resizable])   delegate: EditorDelegate
//!   └─ contentView  (plain NSView, not flipped, origin 0/0, size = content size)
//!       └─ the plugin's view at 0/0, same size
//! ```
//!
//! Who follows whom:
//!
//! * **The plugin resizes its view** (many do on their first layout, or when the user switches
//!   pages). Audio Unit: the view posts `NSViewFrameDidChangeNotification`, the delegate sets the
//!   window's content size to the view's and puts the view back to 0/0. VST3: the plugin calls
//!   `IPlugFrame::resizeView`, which ends in [`EditorWindow::resize`].
//! * **The user resizes the window** (only possible when the plugin allows it). The delegate gets
//!   `windowDidResize:` and passes the content size on - straight to the view (Audio Unit), or
//!   through `checkSizeConstraint` + `onSize` (VST3, via [`EditorWindow::on_user_resize`]).
//!
//! A flag keeps the two from bouncing off each other.
//!
//! The view is added only **after** the window has its final content size, and the content view
//! does not autoresize its subviews for an Audio Unit: a view with a flexible top or bottom margin
//! (Kontakt's) would otherwise be pushed up by exactly the amount the window grew - the top of
//! the plugin under the title bar, an empty band below, and clicks landing next to their targets.
//!
//! Main thread only - AppKit refuses anything else, and [`MainThreadMarker`] makes that a checked
//! error instead of a crash. Closing the window with its close button only **hides** it
//! (`releasedWhenClosed = NO`): the plugin's view stays attached, so opening it again is instant.
//! The view is detached and the window closed when the [`EditorWindow`] is dropped, which the
//! plugin instances do **before** the plugin itself goes away.
//!
//! Where a window stood is remembered per key (plugin slot) for the rest of the session, so a
//! plugin that is loaded again opens where its predecessor was.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSView, NSViewFrameDidChangeNotification, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};

/// Called with the content size the user dragged the window to; answers the size the plugin
/// accepts (`None`: as asked).
pub type UserResize = Box<dyn Fn(f64, f64) -> Option<(f64, f64)>>;

/// Top-left corners of editor windows, by key, for this session.
fn places() -> &'static Mutex<HashMap<String, (f64, f64)>> {
    static PLACES: OnceLock<Mutex<HashMap<String, (f64, f64)>>> = OnceLock::new();
    PLACES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Default)]
struct DelegateIvars {
    window: RefCell<Weak<NSWindow>>,
    /// The Audio Unit's view, which the window follows and which follows the window.
    view: RefCell<Option<Retained<NSView>>>,
    /// The VST3 path: the plugin decides about a size the user dragged to.
    on_user_resize: RefCell<Option<UserResize>>,
    /// Set while the window is resized on the plugin's behalf, so that change is not handed back.
    syncing: Cell<bool>,
    key: RefCell<String>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no Drop of its own.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HenrysLooperPluginEditorDelegate"]
    #[ivars = DelegateIvars]
    struct EditorDelegate;

    unsafe impl NSObjectProtocol for EditorDelegate {}

    unsafe impl NSWindowDelegate for EditorDelegate {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            self.user_resized();
        }

        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, _n: &NSNotification) {
            self.remember_place();
        }
    }

    impl EditorDelegate {
        /// `NSViewFrameDidChangeNotification` of the Audio Unit's view.
        #[unsafe(method(pluginViewFrameDidChange:))]
        fn plugin_view_frame_did_change(&self, _n: &NSNotification) {
            self.follow_view();
        }
    }
);

impl EditorDelegate {
    fn new(mtm: MainThreadMarker, key: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            key: RefCell::new(key.to_string()),
            ..Default::default()
        });
        unsafe { msg_send![super(this), init] }
    }

    fn window(&self) -> Option<Retained<NSWindow>> {
        self.ivars().window.borrow().load()
    }

    /// The plugin changed its view: the window takes the view's size, the view sits at 0/0.
    fn follow_view(&self) {
        let ivars = self.ivars();
        if ivars.syncing.get() {
            return;
        }
        let (Some(window), Some(view)) = (self.window(), ivars.view.borrow().clone()) else {
            return;
        };
        let size = view.frame().size;
        if size.width < 1.0 || size.height < 1.0 {
            return;
        }
        ivars.syncing.set(true);
        if content_size(&window) != size {
            window.setContentSize(size);
        }
        if view.frame().origin != NSPoint::new(0.0, 0.0) {
            view.setFrameOrigin(NSPoint::new(0.0, 0.0));
        }
        ivars.syncing.set(false);
    }

    /// The user dragged the window to a new size: the plugin gets it, or says what it takes.
    fn user_resized(&self) {
        let ivars = self.ivars();
        if ivars.syncing.get() {
            return;
        }
        let Some(window) = self.window() else {
            return;
        };
        let size = content_size(&window);
        ivars.syncing.set(true);
        if let Some(view) = ivars.view.borrow().clone() {
            view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
            // A view that refuses (fixed size) wins: the window goes back to it.
            let got = view.frame().size;
            if got != size && got.width >= 1.0 && got.height >= 1.0 {
                window.setContentSize(got);
                view.setFrameOrigin(NSPoint::new(0.0, 0.0));
            }
        } else if let Some(callback) = ivars.on_user_resize.borrow().as_ref()
            && let Some((w, h)) = callback(size.width, size.height)
            && (w != size.width || h != size.height)
        {
            window.setContentSize(NSSize::new(w, h));
        }
        ivars.syncing.set(false);
    }

    fn remember_place(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let key = self.ivars().key.borrow().clone();
        if key.is_empty() {
            return;
        }
        let f = window.frame();
        places()
            .lock()
            .unwrap()
            .insert(key, (f.origin.x, f.origin.y + f.size.height));
    }
}

fn content_size(window: &NSWindow) -> NSSize {
    window.contentRectForFrameRect(window.frame()).size
}

/// A plugin editor window.
pub struct EditorWindow {
    window: Retained<NSWindow>,
    delegate: Retained<EditorDelegate>,
    /// The Audio Unit's view, when the window holds one (VST3 attaches its own into the content
    /// view instead).
    view: Option<Retained<NSView>>,
}

impl EditorWindow {
    /// A new, hidden window with a content area of `width` x `height` points. `key` names the slot
    /// (for the remembered position), `resizable` whether the user may resize it.
    pub fn new(
        title: &str,
        key: &str,
        width: f64,
        height: f64,
        resizable: bool,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "A plugin window can only be opened on the main thread.".to_string())?;
        let mut style =
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width.max(50.0), height.max(30.0)),
        );
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
        window.setAcceptsMouseMovedEvents(true);
        let delegate = EditorDelegate::new(mtm, key);
        *delegate.ivars().window.borrow_mut() = Weak::from_retained(&window);
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let place = places().lock().unwrap().get(key).copied();
        match place {
            Some((x, top)) => window.setFrameTopLeftPoint(NSPoint::new(x, top)),
            None => window.center(),
        }
        Ok(Self {
            window,
            delegate,
            view: None,
        })
    }

    /// The content view, as the raw `NSView*` VST3's `IPlugView::attached` wants.
    pub fn content_view(&self) -> *mut c_void {
        match self.window.contentView() {
            Some(view) => Retained::as_ptr(&view) as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    /// Who decides about a size the user dragged the window to (VST3).
    pub fn on_user_resize(&self, callback: UserResize) {
        *self.delegate.ivars().on_user_resize.borrow_mut() = Some(callback);
    }

    /// Put a view the plugin created into the window, size the window to it, and follow it from
    /// then on. Whether the user may resize is up to the view: one with a flexible width or
    /// height (autoresizing mask) gets a resizable window.
    ///
    /// # Safety
    /// `view` must be a valid `NSView*`.
    pub unsafe fn embed(&mut self, view: *mut c_void) -> Result<(), String> {
        use objc2_app_kit::NSAutoresizingMaskOptions as Mask;
        let view = unsafe { Retained::retain(view as *mut NSView) }
            .ok_or_else(|| "The plugin returned no view.".to_string())?;
        let content = self
            .window
            .contentView()
            .ok_or_else(|| "The window has no content view.".to_string())?;
        let mask = view.autoresizingMask();
        if mask.intersects(Mask::ViewWidthSizable | Mask::ViewHeightSizable) {
            self.window.setStyleMask(self.window.styleMask() | NSWindowStyleMask::Resizable);
        }
        // Size first, then add: see the module comment.
        content.setAutoresizesSubviews(false);
        let size = view.frame().size;
        let size = NSSize::new(size.width.max(50.0), size.height.max(30.0));
        self.window.setContentSize(size);
        view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        content.addSubview(&view);
        view.setPostsFrameChangedNotifications(true);
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &self.delegate,
                sel!(pluginViewFrameDidChange:),
                Some(NSViewFrameDidChangeNotification),
                Some(&view),
            );
        }
        *self.delegate.ivars().view.borrow_mut() = Some(view.clone());
        self.view = Some(view);
        // The view may have changed its size while it was put into the window.
        self.delegate.follow_view();
        Ok(())
    }

    /// The plugin asks for a content area of `width` x `height` points (VST3 `resizeView`).
    pub fn resize(&self, width: f64, height: f64) {
        let ivars = self.delegate.ivars();
        let was = ivars.syncing.replace(true);
        self.window.setContentSize(NSSize::new(width, height));
        if let Some(view) = &self.view {
            view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)));
        }
        ivars.syncing.set(was);
    }

    /// Resize the window as a user dragging its corner would (tests, the CLI) - nothing happens
    /// when the window is not resizable.
    pub fn resize_as_user(&self, width: f64, height: f64) {
        if !self.window.styleMask().contains(NSWindowStyleMask::Resizable) {
            return;
        }
        self.window.setContentSize(NSSize::new(width, height));
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
        if let Some(view) = &self.view
            && view.acceptsFirstResponder()
        {
            self.window.makeFirstResponder(Some(view));
        }
    }

    pub fn hide(&self) {
        self.delegate.remember_place();
        self.window.orderOut(None);
    }

    pub fn visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Size of the plugin's view in points - the Audio Unit's view, or the first subview the VST3
    /// editor attached; the content size when there is neither.
    pub fn view_size(&self) -> (f64, f64) {
        let size = match self.plugin_view() {
            Some(v) => v.frame().size,
            None => content_size(&self.window),
        };
        (size.width, size.height)
    }

    fn plugin_view(&self) -> Option<Retained<NSView>> {
        if let Some(v) = &self.view {
            return Some(v.clone());
        }
        self.window
            .contentView()
            .and_then(|c| c.subviews().iter().next())
    }

    /// Everything that decides whether the window is built right, as one line for a log: content
    /// rectangle, the plugin view's frame in window coordinates, flipped, backing scale, and which
    /// view a click at five points of the content area would reach.
    pub fn report(&self) -> String {
        let w = &self.window;
        let content = w.contentRectForFrameRect(w.frame());
        let mut out = format!(
            "window {:.0}x{:.0}, content {:.0}x{:.0}, resizable {}, scale {}, title bar opaque {}, key {}",
            w.frame().size.width,
            w.frame().size.height,
            content.size.width,
            content.size.height,
            w.styleMask().contains(NSWindowStyleMask::Resizable),
            w.backingScaleFactor(),
            !w.titlebarAppearsTransparent()
                && !w.styleMask().contains(NSWindowStyleMask::FullSizeContentView),
            w.isKeyWindow(),
        );
        let Some(cv) = w.contentView() else {
            return out + ", no content view";
        };
        out += &format!(
            ", content view {} flipped {}",
            rect_text(cv.convertRect_toView(cv.bounds(), None)),
            cv.isFlipped()
        );
        match self.plugin_view() {
            Some(v) => {
                let in_window = v.convertRect_toView(v.bounds(), None);
                out += &format!(
                    ", plugin view {} ({}) flipped {} mask {:#x}, fills content {}",
                    rect_text(in_window),
                    class_name(&v),
                    v.isFlipped(),
                    v.autoresizingMask().0,
                    in_window == NSRect::new(NSPoint::new(0.0, 0.0), content.size)
                );
                let frame_view = unsafe { cv.superview() }.unwrap_or_else(|| cv.clone());
                let mut hits = Vec::new();
                for (fx, fy) in [(0.5, 0.5), (0.05, 0.05), (0.95, 0.05), (0.05, 0.95), (0.95, 0.95)] {
                    let p = NSPoint::new(content.size.width * fx, content.size.height * fy);
                    let p = frame_view.convertPoint_fromView(p, None);
                    let hit = frame_view.hitTest(p);
                    hits.push(match hit {
                        Some(h) if h.isDescendantOf(&v) => "plugin".to_string(),
                        Some(h) => format!("other({})", class_name(&h)),
                        None => "none".to_string(),
                    });
                }
                out += &format!(", clicks reach {}", hits.join("/"));
            }
            None => out += ", no plugin view",
        }
        if let Some(r) = w.firstResponder() {
            out += &format!(", first responder {}", class_name(&r));
        }
        out
    }

    /// The plugin's view drawn into a PNG, without a screen capture. Plugins that draw through
    /// Metal or OpenGL come out empty here - their pixels are not in the view hierarchy.
    pub fn snapshot_png(&self) -> Option<Vec<u8>> {
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRepPropertyKey};
        use objc2_foundation::NSDictionary;
        let view = self.window.contentView()?;
        let rect = view.bounds();
        let rep = view.bitmapImageRepForCachingDisplayInRect(rect)?;
        view.cacheDisplayInRect_toBitmapImageRep(rect, &rep);
        let props = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }?;
        Some(data.to_vec())
    }
}

fn rect_text(r: NSRect) -> String {
    format!(
        "{:.0},{:.0} {:.0}x{:.0}",
        r.origin.x, r.origin.y, r.size.width, r.size.height
    )
}

fn class_name(obj: &impl std::ops::Deref<Target = impl objc2::Message>) -> String {
    let obj: &AnyObject = unsafe { &*(&**obj as *const _ as *const AnyObject) };
    obj.class().name().to_string_lossy().into_owned()
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

/// The window's rules checked with plain NSViews standing in for a plugin's: one that stretches
/// (Kontakt's AU view: flexible width and height) and one of fixed size; each embedded, resized by
/// "the plugin", resized by "the user", hidden, shown, dropped, opened again at the same place.
/// Needs the main thread, so it is a CLI command (`plugins editor-selftest`), not a unit test.
/// Returns one line per check; an `Err` names the first one that failed.
pub fn self_test() -> Result<Vec<String>, String> {
    use objc2_app_kit::NSAutoresizingMaskOptions as Mask;
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let mut lines = Vec::new();
    let mut check = |ok: bool, what: String| -> Result<(), String> {
        lines.push(format!("{} {what}", if ok { "ok  " } else { "FAIL" }));
        if ok { Ok(()) } else { Err(what) }
    };
    let fills = |w: &EditorWindow, width: f64, height: f64| {
        let content = content_size(&w.window);
        let view = w.view.as_ref().unwrap().frame();
        content == NSSize::new(width, height)
            && view == NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height))
    };
    for (name, mask) in [
        ("stretching view", Mask::ViewWidthSizable | Mask::ViewHeightSizable),
        ("fixed view", Mask::ViewNotSizable),
    ] {
        let stretches = !mask.is_empty();
        let view = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1010.0, 647.0)),
        );
        view.setAutoresizingMask(mask);
        let mut w = EditorWindow::new("self test", "selftest", 400.0, 300.0, false)?;
        unsafe { w.embed(Retained::as_ptr(&view) as *mut c_void)? };
        check(fills(&w, 1010.0, 647.0), format!("{name}: window takes the view's size"))?;
        check(
            w.window.styleMask().contains(NSWindowStyleMask::Resizable) == stretches,
            format!("{name}: resizable = {stretches}"),
        )?;
        check(
            !w.window.styleMask().contains(NSWindowStyleMask::FullSizeContentView)
                && !w.window.titlebarAppearsTransparent(),
            format!("{name}: plain title bar"),
        )?;
        w.show();
        view.setFrameSize(NSSize::new(700.0, 500.0));
        check(fills(&w, 700.0, 500.0), format!("{name}: window follows the plugin's resize"))?;
        w.resize_as_user(900.0, 600.0);
        let (ew, eh) = if stretches { (900.0, 600.0) } else { (700.0, 500.0) };
        check(fills(&w, ew, eh), format!("{name}: user resize gives {ew}x{eh}"))?;
        w.window.setFrameTopLeftPoint(NSPoint::new(123.0, 654.0));
        w.hide();
        check(!w.visible(), format!("{name}: hidden"))?;
        w.show();
        check(w.visible() && fills(&w, ew, eh), format!("{name}: shown again, same size"))?;
        // Where it really stood: AppKit keeps a shown window clear of the Dock.
        let last = w.window.frame();
        let (x, top) = (last.origin.x, last.origin.y + last.size.height);
        drop(w);
        check(unsafe { view.superview() }.is_none(), format!("{name}: view detached on drop"))?;
        let again = EditorWindow::new("self test", "selftest", 300.0, 200.0, false)?;
        let f = again.window.frame();
        check(
            (f.origin.x - x).abs() < 1.0 && (f.origin.y + f.size.height - top).abs() < 1.0,
            format!(
                "{name}: a new window of the slot opens where the last one stood ({})",
                rect_text(f)
            ),
        )?;
    }
    places().lock().unwrap().remove("selftest");
    Ok(lines)
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        self.delegate.remember_place();
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(&self.delegate) };
        let ivars = self.delegate.ivars();
        ivars.view.borrow_mut().take();
        ivars.on_user_resize.borrow_mut().take();
        self.window.setDelegate(None);
        if let Some(view) = self.view.take() {
            view.removeFromSuperview();
        }
        self.window.orderOut(None);
        self.window.close();
    }
}
