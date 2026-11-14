//! What differs between the two machines the app runs on, decided once.
//!
//! The WebView reports its platform, so this needs no round trip to Rust and is known before the
//! first render - which matters for the setup defaults, read synchronously from `localStorage`.

/** True in the macOS WebView (WKWebView). */
export const IS_MAC: boolean =
  typeof navigator !== "undefined" &&
  /mac/i.test(
    (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform ??
      navigator.platform ??
      navigator.userAgent,
  );

/**
 * The modifier for shortcuts and for learn-by-click: ⌘ on the Mac, Ctrl elsewhere. Ctrl+click on
 * a Mac is a right click, so it cannot be the learn gesture there.
 */
export const MOD_LABEL = IS_MAC ? "⌘" : "Ctrl";

/** Whether the platform's shortcut modifier is held in this event. */
export function modHeld(e: { ctrlKey: boolean; metaKey: boolean }): boolean {
  return IS_MAC ? e.metaKey : e.ctrlKey;
}
