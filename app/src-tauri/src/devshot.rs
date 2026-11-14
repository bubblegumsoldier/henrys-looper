//! `dev_snapshot`: the window's web content as a PNG, taken by WebKit itself.
//!
//! For checking the user interface against the design mockups without a mouse and without the
//! system's screen-recording permission: `WKWebView::takeSnapshot` renders the app's own page, so
//! it needs no access to anyone else's pixels. The native title-bar buttons (traffic lights) are
//! drawn by the window, not by the page, and are therefore not in the picture.
//!
//! Only on macOS; elsewhere the command answers with a sentence.

#[cfg(target_os = "macos")]
pub fn snapshot(window: &tauri::WebviewWindow, path: String) -> Result<String, String> {
    use block2::RcBlock;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError, NSString};
    use objc2_web_kit::WKWebView;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    window
        .with_webview(move |wv| {
            // SAFETY: on macOS `inner()` is the WKWebView this window owns; we are on the main
            // thread inside `with_webview`, where WebKit may be called.
            let webview: &WKWebView = unsafe { &*(wv.inner() as *const WKWebView) };
            let path = path.clone();
            let tx = tx.clone();
            let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                let result = if image.is_null() {
                    let reason = if error.is_null() {
                        "no image".to_string()
                    } else {
                        unsafe { (*error).localizedDescription().to_string() }
                    };
                    Err(format!("Snapshot fehlgeschlagen: {reason}"))
                } else {
                    let image = unsafe { &*image };
                    image
                        .TIFFRepresentation()
                        .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
                        .and_then(|rep| unsafe {
                            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
                        })
                        .map(|png| png.writeToFile_atomically(&NSString::from_str(&path), true))
                        .filter(|ok| *ok)
                        .map(|_| path.clone())
                        .ok_or_else(|| format!("PNG konnte nicht nach {path} geschrieben werden"))
                };
                let _ = tx.send(result);
            });
            unsafe { webview.takeSnapshotWithConfiguration_completionHandler(None, &done) };
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "Snapshot: keine Antwort von WebKit".to_string())?
}

#[cfg(not(target_os = "macos"))]
pub fn snapshot(_window: &tauri::WebviewWindow, _path: String) -> Result<String, String> {
    Err("dev_snapshot gibt es nur auf macOS.".into())
}
