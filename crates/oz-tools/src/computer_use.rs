//! computer_use — macOS desktop control tools (docs/computer-use-plan.md).
//!
//! Phase 1: `computer_screenshot` / `computer_click` / `computer_type` /
//! `computer_key` / `computer_scroll`. Phase 2: `computer_read_screen` (AX
//! tree) + element-index targeting with a per-session element cache.
//!
//! Discipline taught to the model via tool descriptions (zcode semantics):
//! observe → act → re-observe; bare coordinates only from the most recent
//! screenshot; prefer element targets from `computer_read_screen`.
//!
//! Coordinate spaces (C1 review fix): screenshots are PNG **pixels**
//! (possibly downscaled by sips); CGEvent mouse positions are display
//! **points**. Every capture writes a `<name>.meta.json` sidecar recording
//! both spaces, and click/scroll convert pixel → point through the newest
//! sidecar before dispatching. Mouse events post via direct CoreGraphics
//! FFI — enigo 0.2.1's mouse path re-derives positions from a bottom-up
//! coordinate mix that is wrong on Retina (its keyboard path is fine and
//! still used for `computer_type` / `computer_key`).
//!
//! Permissions (macOS TCC, preflighted per call):
//! - Screen Recording — screenshots (`CGPreflightScreenCaptureAccess`)
//! - Accessibility — CGEvent input + AX tree (`AXIsProcessTrusted`)

use crate::registry::ToolHandler;
use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Directory (relative to the session working dir) screenshots land in.
/// Public: the Tauri `computer_screenshot_data` preview command validates
/// read-back paths against the same directory.
pub const SCREENSHOT_DIR: &str = "computer";

/// Prompt/reason strings attached to permission errors so the model and the
/// frontend can guide the user through System Settings.
const SCREEN_RECORDING_GUIDE: &str = "Screen Recording permission missing. Open System Settings → Privacy & Security → Screen Recording, enable OpenZen, then restart the app.";
const ACCESSIBILITY_GUIDE: &str = "Accessibility permission missing. Open System Settings → Privacy & Security → Accessibility, enable OpenZen, then retry.";

fn permission_error(missing: &str, guide: &str) -> ToolError {
    ToolError::Custom(
        json!({ "error": "permission_denied", "missing": missing, "guide": guide }).to_string(),
    )
}

// ── TCC preflight ──────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod tcc {
    // Minimal raw FFI over two framework queries — no crate exposes them and
    // they are read-only booleans (project rule: minimal, commented unsafe).
    extern "C" {
        /// True when Screen Recording permission is already granted.
        fn CGPreflightScreenCaptureAccess() -> bool;
        /// True when Accessibility (AX) trust is already granted.
        fn AXIsProcessTrusted() -> bool;
    }

    /// True when Screen Recording permission is already granted.
    pub fn screen_capture_ok() -> bool {
        // SAFETY: stateless framework query, no pointers involved.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// True when Accessibility (AX) trust is already granted.
    pub fn accessibility_ok() -> bool {
        // SAFETY: stateless framework query, no pointers involved.
        unsafe { AXIsProcessTrusted() }
    }
}

#[cfg(not(target_os = "macos"))]
mod tcc {
    // Non-mac hosts never have the macOS permissions — tools error out.
    /// Always false off macOS.
    pub fn screen_capture_ok() -> bool {
        false
    }
    /// Always false off macOS.
    pub fn accessibility_ok() -> bool {
        false
    }
}

/// Err when Screen Recording permission is missing.
fn require_screen_capture() -> Result<(), ToolError> {
    if !tcc::screen_capture_ok() {
        return Err(permission_error("screen_recording", SCREEN_RECORDING_GUIDE));
    }
    Ok(())
}

/// Err when Accessibility permission is missing.
fn require_accessibility() -> Result<(), ToolError> {
    if !tcc::accessibility_ok() {
        return Err(permission_error("accessibility", ACCESSIBILITY_GUIDE));
    }
    Ok(())
}

/// Err on non-macOS hosts (the module is macOS-only in v1).
fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

fn require_macos() -> Result<(), ToolError> {
    if !is_macos() {
        return Err(ToolError::Custom(
            "computer use tools are macOS-only in this version".into(),
        ));
    }
    Ok(())
}

// ── CoreGraphics FFI: screen geometry + mouse/scroll events ────────────────

#[cfg(target_os = "macos")]
mod cg {
    /// Minimal CoreGraphics surface for mouse/scroll in true display points.
    /// enigo 0.2.1's mouse path mixes bottom-up NSEvent points with pixel
    /// heights and misplaces clicks on Retina, so mouse events post here.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    // kCGEvent* mouse / kCGMouseEventClickState / kCGHIDEventTap constants.
    pub const MOUSE_MOVED: u32 = 5;
    pub const LEFT_DOWN: u32 = 1;
    pub const LEFT_UP: u32 = 2;
    pub const RIGHT_DOWN: u32 = 3;
    pub const RIGHT_UP: u32 = 4;
    pub const MOUSE_BUTTON_LEFT: i64 = 0;
    pub const MOUSE_BUTTON_RIGHT: i64 = 1;
    pub const HID_TAP: u32 = 0;
    /// kCGMouseEventClickState — click count for double-click recognition.
    pub const CLICK_STATE_FIELD: u64 = 1;

    type CGEventRef = *mut std::ffi::c_void;

    extern "C" {
        fn CGGetActiveDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGEventCreateMouseEvent(
            source: *const std::ffi::c_void,
            event_type: u32,
            point: CGPoint,
            button: i64,
        ) -> CGEventRef;
        fn CGEventCreateScrollWheelEvent(
            source: *const std::ffi::c_void,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
        ) -> CGEventRef;
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u64, value: i64);
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    /// Point size (w, h) of the n-th active display (n is 0-based).
    pub fn display_point_size(n: u32) -> Option<(f64, f64)> {
        let mut ids = [0u32; 8];
        let mut count: u32 = 0;
        // SAFETY: fixed-size out buffer + count pointer, documented CG API.
        unsafe {
            if CGGetActiveDisplayList(8, ids.as_mut_ptr(), &mut count) != 0 {
                return None;
            }
            if n >= count {
                return None;
            }
            let b = CGDisplayBounds(ids[n as usize]);
            Some((b.size.width, b.size.height))
        }
    }

    /// Post one mouse event of `ty` at `point`; `click_state` 0 = none.
    pub fn post_mouse(event_type: u32, point: CGPoint, button: i64, click_state: i64) {
        // SAFETY: CGEventCreateMouseEvent with a null source uses the default
        // event source; the event is released immediately after posting.
        unsafe {
            let ev = CGEventCreateMouseEvent(
                std::ptr::null(),
                event_type,
                point,
                button,
            );
            if ev.is_null() {
                return;
            }
            if click_state > 0 {
                CGEventSetIntegerValueField(ev, CLICK_STATE_FIELD, click_state);
            }
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }

    /// Post a vertical line-unit scroll; `lines` positive = scroll up.
    pub fn post_scroll_lines(lines: i32) {
        // SAFETY: null source = default event source; released after posting.
        unsafe {
            let ev = CGEventCreateScrollWheelEvent(std::ptr::null(), 1, 1, lines);
            if ev.is_null() {
                return;
            }
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }
}

/// Run a blocking action off the async workers. enigo holds 20ms-per-event
/// delays, hold gestures sleep up to 2s, and screencapture/sips are
/// subprocesses — none of it belongs on a shared tokio worker.
async fn run_blocking<T, F>(f: F) -> Result<T, ToolError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(inner) => inner,
        Err(e) => Err(ToolError::Custom(format!("blocking task failed: {e}"))),
    }
}

// ── Screenshot plumbing ────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct ShotMeta {
    png_w: u32,
    png_h: u32,
    point_w: f64,
    point_h: f64,
}

/// PNG pixel dimensions via `sips -g pixelWidth -g pixelHeight`.
#[cfg(target_os = "macos")]
fn png_dims(path: &Path) -> Result<(u32, u32), ToolError> {
    let out = std::process::Command::new("/usr/bin/sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight", path.to_string_lossy().as_ref()])
        .output()
        .map_err(|e| ToolError::Custom(format!("sips launch failed: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let num = |key: &str| -> Option<u32> {
        text.lines().find(|l| l.contains(key))?.split(':').nth(1)?.trim().parse().ok()
    };
    match (num("pixelWidth"), num("pixelHeight")) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(ToolError::Custom("cannot read PNG dimensions".into())),
    }
}

/// Capture `display` (1-based, main = 1) to `{working_dir}/computer/<ts>.png`
/// via the macOS `screencapture` CLI, downscaling oversized captures with
/// `sips`, and writing a pixel↔point sidecar for click conversion.
#[cfg(target_os = "macos")]
fn capture_screenshot(work_dir: &str, display: u32) -> Result<(PathBuf, u64, u32, u32), ToolError> {
    require_screen_capture()?;
    let dir = PathBuf::from(work_dir).join(SCREENSHOT_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| ToolError::Custom(format!("cannot create screenshot dir: {e}")))?;
    let path = dir.join(format!(
        "screen-{}.png",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
    ));

    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args([
            "-x", // silent (no shutter sound)
            "-D",
            &display.to_string(),
            path.to_string_lossy().as_ref(),
        ])
        .status()
        .map_err(|e| ToolError::Custom(format!("screencapture launch failed: {e}")))?;
    if !status.success() {
        return Err(ToolError::Custom(format!(
            "screencapture exited with {status}"
        )));
    }

    // Retina captures are multi-MB PNGs; downscale to a readable 1600px cap
    // when oversized. A failed downscale would serve a multi-MB image to the
    // model and the preview — fail loudly instead.
    let bytes = file_len(&path)?;
    if bytes > 1_200_000 {
        let st = std::process::Command::new("/usr/bin/sips")
            .args(["-Z", "1600", path.to_string_lossy().as_ref()])
            .status()
            .map_err(|e| ToolError::Custom(format!("sips launch failed: {e}")))?;
        if !st.success() {
            return Err(ToolError::Custom(format!("sips exited with {st}")));
        }
    }
    let bytes = file_len(&path)?;

    // Record the pixel↔point mapping so clicks can convert coordinates.
    let (png_w, png_h) = png_dims(&path)?;
    let (point_w, point_h) = cg::display_point_size(display - 1)
        .unwrap_or((png_w as f64, png_h as f64));
    let meta = ShotMeta {
        png_w,
        png_h,
        point_w,
        point_h,
    };
    let meta_path = path.with_extension("meta.json");
    std::fs::write(
        &meta_path,
        serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|e| ToolError::Custom(format!("meta write failed: {e}")))?;

    Ok((path, bytes, png_w, png_h))
}

#[cfg(target_os = "macos")]
fn file_len(path: &Path) -> Result<u64, ToolError> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| ToolError::Custom(format!("screenshot missing: {e}")))
}

/// Newest sidecar in the screenshot dir — the coordinate reference for the
/// next click/scroll (the discipline requires acting on the LATEST capture).
#[cfg(target_os = "macos")]
fn latest_shot_meta(work_dir: &str) -> Result<ShotMeta, ToolError> {
    let dir = PathBuf::from(work_dir).join(SCREENSHOT_DIR);
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| ToolError::Custom(format!("no screenshot dir: {e}")))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
                if newest.as_ref().is_none_or(|(t, _)| m > *t) {
                    newest = Some((m, p));
                }
            }
        }
    }
    let Some((_, meta_path)) = newest else {
        return Err(ToolError::Custom(
            "no screenshot reference — take computer_screenshot first (click coordinates are pixels from the LATEST screenshot)"
                .into(),
        ));
    };
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| ToolError::Custom(format!("meta read failed: {e}")))?;
    serde_json::from_str(&raw)
        .map_err(|e| ToolError::Custom(format!("meta parse failed: {e}")))
}

/// Convert screenshot pixels → display points via a sidecar's scale.
#[cfg(target_os = "macos")]
fn px_to_point(v: i32, png: u32, point: f64) -> i32 {
    if png == 0 {
        return v;
    }
    (v as f64 * (point / png as f64)).round() as i32
}

// ── Keyboard via enigo (unicode text + keycode lookups work fine) ──────────

#[cfg(target_os = "macos")]
fn new_enigo() -> Result<enigo::Enigo, ToolError> {
    enigo::Enigo::new(&enigo::Settings::default())
        .map_err(|e| ToolError::Custom(format!("enigo init failed: {e}")))
}

/// Parse `"cmd+shift+s"` / `"return"` / `"f5"` into (held modifiers, key).
/// The LAST token is the key; everything before it is held as a modifier.
/// Conservative on purpose: enigo's keycode lookup silently falls back to
/// the 'a' key for characters missing from the keyboard layout, so non-ASCII
/// and unknown tokens are rejected with guidance toward `computer_type`.
#[cfg(target_os = "macos")]
fn parse_key_combo(combo: &str) -> Result<(Vec<enigo::Key>, enigo::Key), ToolError> {
    use enigo::Key;

    let tokens: Vec<String> = combo
        .split('+')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err(ToolError::Custom(
            "empty key combo — for '+' and non-ASCII characters use computer_type".into(),
        ));
    }

    // Named keys match case-insensitively; single characters keep their case.
    let named = |t: &str| -> Option<Key> {
        let lower = t.to_lowercase();
        Some(match lower.as_str() {
            "return" | "enter" => Key::Return,
            "tab" => Key::Tab,
            "space" => Key::Space,
            "escape" | "esc" => Key::Escape,
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "up" => Key::UpArrow,
            "down" => Key::DownArrow,
            "left" => Key::LeftArrow,
            "right" => Key::RightArrow,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "f1" => Key::F1,
            "f2" => Key::F2,
            "f3" => Key::F3,
            "f4" => Key::F4,
            "f5" => Key::F5,
            "f6" => Key::F6,
            "f7" => Key::F7,
            "f8" => Key::F8,
            "f9" => Key::F9,
            "f10" => Key::F10,
            "f11" => Key::F11,
            "f12" => Key::F12,
            _ => return None,
        })
    };

    // Modifier tokens — table shared with the last-token chord arm below.
    let modifier_key = |t: &str| -> Option<Key> {
        match t.to_lowercase().as_str() {
            "cmd" | "meta" | "command" => Some(Key::Meta),
            "shift" => Some(Key::Shift),
            "ctrl" | "control" => Some(Key::Control),
            "alt" | "option" | "opt" => Some(Key::Alt),
            _ => None,
        }
    };

    let mut modifiers: Vec<Key> = Vec::new();
    for t in &tokens[..tokens.len() - 1] {
        modifiers.push(modifier_key(t).ok_or_else(|| {
            ToolError::Custom(format!("unknown modifier: {t}"))
        })?);
    }

    let key_token = tokens.last().unwrap().as_str();
    let lower = key_token.to_lowercase();
    let key = match lower.as_str() {
        "cmd" | "meta" | "command" => Key::Meta,
        "shift" => Key::Shift,
        "ctrl" | "control" => Key::Control,
        "alt" | "option" | "opt" => Key::Alt,
        "plus" => {
            return Err(ToolError::Custom(
                "use computer_type for '+' — layout-dependent keys fall back to 'a'"
                    .into(),
            ))
        }
        other => {
            // Named keys first; then exactly one ASCII alnum grapheme.
            // enigo's keycode lookup silently falls back to the 'a' key for
            // anything missing from the layout, so anything else (CJK,
            // punctuation, multi-char) is rejected with computer_type guidance.
            if let Some(k) = named(other) {
                k
            } else {
                let mut chars = key_token.chars();
                let (Some(c), 0) = (chars.next(), chars.count()) else {
                    return Err(ToolError::Custom(format!(
                        "unsupported key \"{key_token}\" — use computer_type for text and non-ASCII characters"
                    )));
                };
                match c {
                    'A'..='Z' => {
                        modifiers.push(Key::Shift);
                        Key::Unicode(c.to_ascii_lowercase())
                    }
                    'a'..='z' | '0'..='9' => Key::Unicode(c),
                    _ => {
                        return Err(ToolError::Custom(format!(
                            "unsupported key \"{key_token}\" — use computer_type for text and non-ASCII characters"
                        )));
                    }
                }
            }
        }
    };
    Ok((modifiers, key))
}

// ── AX tree (read_screen) + element cache ──────────────────────────────────

/// Per-session cache of AX elements from the last `computer_read_screen`.
/// Indices assigned during the walk stay valid until the next read. Only the
/// newest 8 sessions are retained — stale session trees would otherwise leak
/// their retained AX elements for the process lifetime.
#[cfg(target_os = "macos")]
mod ax_cache {
    use accessibility::AXUIElement;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// AXUIElement wraps a CF ref (not Send by default). Hand-off between the
    /// walking thread and the clicking thread is serialized through this
    /// mutex; crossing threads is safe because CF refs use atomic
    /// retain/release and the AX C API is out-of-process RPC with no
    /// creating-thread requirement.
    pub struct SendElement(AXUIElement);

    impl SendElement {
        /// Wrapping constructor (keeps the inner field private to this module).
        pub fn new(el: AXUIElement) -> Self {
            Self(el)
        }

        /// Retaining clone of the wrapped element.
        pub fn clone_element(&self) -> AXUIElement {
            self.0.clone()
        }
    }

    // SAFETY: per the SendElement docs — atomic CF refcounting + AX API
    // thread-neutrality, with the hand-off serialized by the cache mutex.
    unsafe impl Send for SendElement {}

    const MAX_SESSIONS: usize = 8;

    /// session_id → elements from that session's last `computer_read_screen`.
    /// Kept to the newest [`MAX_SESSIONS`] sessions (see `with_cache`).
    pub static CACHE: Mutex<Option<HashMap<String, Vec<SendElement>>>> = Mutex::new(None);

    /// Run `f` over the cache map, evicting arbitrary sessions (HashMap
    /// order) once the cap is exceeded.
    pub fn with_cache<R>(f: impl FnOnce(&mut HashMap<String, Vec<SendElement>>) -> R) -> R {
        let mut guard = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let map = guard.get_or_insert_with(HashMap::new);
        let result = f(map);
        while map.len() > MAX_SESSIONS {
            let oldest = map
                .keys()
                .next()
                .cloned()
                .expect("len > 0 implies a key exists");
            map.remove(&oldest);
        }
        result
    }
}

/// Clone the cached element `idx` out of the session cache (retains the CF ref).
#[cfg(target_os = "macos")]
fn cached_element(session_id: &str, idx: usize) -> Result<accessibility::AXUIElement, ToolError> {
    ax_cache::with_cache(|map| {
        let els = map.get(session_id).ok_or_else(|| {
            ToolError::Custom("no cached elements — run computer_read_screen first".to_string())
        })?;
        let el = els.get(idx).ok_or_else(|| {
            ToolError::Custom(format!(
                "element {idx} out of range (last read_screen cached {} elements) — re-run computer_read_screen",
                els.len()
            ))
        })?;
        Ok(el.clone_element())
    })
}

/// Press an element via its AXPress action (no focus steal, no coordinates).
#[cfg(target_os = "macos")]
fn press_element(el: &accessibility::AXUIElement) -> Result<(), ToolError> {
    use core_foundation::string::CFString;
    el.perform_action(&CFString::from_static_string("AXPress"))
        .map_err(|_| {
            ToolError::Custom(
                "AXPress failed (element may be stale or not pressable) — re-run computer_read_screen"
                    .into(),
            )
        })
}

/// Walk the focused application's AX tree, emitting `  [idx] Role: label`
/// lines and caching every visited element. Depth- and count-limited, so AX
/// parent/child cycles can only waste budget on duplicates, never loop.
#[cfg(target_os = "macos")]
fn read_screen_tree(
    session_id: &str,
    max_depth: usize,
    max_elements: usize,
) -> Result<(String, usize), ToolError> {
    use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
    use core_foundation::base::CFType;
    use core_foundation::string::CFString;

    require_accessibility()?;

    let system_wide = AXUIElement::system_wide();
    // Focused application; falls back to the system-wide element when the
    // attribute is unavailable (headless/dev contexts without a focus chain).
    let root: AXUIElement = system_wide
        .attribute(&AXAttribute::<CFType>::new(&CFString::from_static_string(
            "AXFocusedApplication",
        )))
        .ok()
        .and_then(|v| v.downcast_into::<AXUIElement>())
        .unwrap_or(system_wide);

    let mut lines: Vec<String> = Vec::with_capacity(max_elements);
    let mut cache: Vec<ax_cache::SendElement> = Vec::with_capacity(max_elements);

    fn walk(
        el: &AXUIElement,
        depth: usize,
        max_depth: usize,
        max_elements: usize,
        lines: &mut Vec<String>,
        cache: &mut Vec<ax_cache::SendElement>,
    ) {
        if depth > max_depth || cache.len() >= max_elements {
            return;
        }
        let role = el.role().map(|r| r.to_string()).unwrap_or_default();
        let title = el.title().map(|t| t.to_string()).unwrap_or_default();
        let value = el
            .attribute(&AXAttribute::<CFType>::new(&CFString::from_static_string(
                "AXValue",
            )))
            .ok()
            .and_then(|v| v.downcast_into::<CFString>())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let label = if !title.is_empty() {
            title
        } else if !value.is_empty() {
            let mut v: String = value.chars().take(60).collect();
            if value.chars().count() > 60 {
                v.push('…');
            }
            v
        } else {
            String::new()
        };

        // Cache every visited element so indices stay stable across the tree.
        let idx = cache.len();
        cache.push(ax_cache::SendElement::new(el.clone()));

        if !label.is_empty() || depth <= 1 {
            lines.push(format!(
                "{}[{}] {}{}",
                "  ".repeat(depth),
                idx,
                if role.is_empty() { "Element" } else { role.trim_start_matches("AX") },
                if label.is_empty() {
                    String::new()
                } else {
                    format!(": {label}")
                }
            ));
        }

        if let Ok(children) = el.children() {
            for child in &children {
                walk(&child, depth + 1, max_depth, max_elements, lines, cache);
                if cache.len() >= max_elements {
                    return;
                }
            }
        }
    }

    walk(&root, 0, max_depth, max_elements, &mut lines, &mut cache);
    let count = cache.len();

    ax_cache::with_cache(|map| {
        map.insert(session_id.to_string(), cache);
    });

    let tree = lines.join("\n");
    Ok((tree, count))
}

// ── Shared action bodies ───────────────────────────────────────────────────

/// Arguments for [`do_click`], grouped to keep the parameter count sane.
struct ClickArgs {
    work_dir: String,
    session_id: String,
    x: Option<i32>,
    y: Option<i32>,
    element: Option<usize>,
    right: bool,
    double: bool,
    hold_ms: u32,
}

/// Click: pixel coordinates converted to display points via the newest
/// sidecar, posted as raw CGEvents; or `element` index via cached AXPress.
#[cfg(target_os = "macos")]
fn do_click(a: &ClickArgs) -> Result<serde_json::Value, ToolError> {
    require_accessibility()?;
    use cg::{
        post_mouse, LEFT_DOWN, LEFT_UP, MOUSE_BUTTON_LEFT, MOUSE_BUTTON_RIGHT, RIGHT_DOWN,
        RIGHT_UP,
    };

    let move_event = cg::MOUSE_MOVED;
    let action = if a.double {
        "double_click"
    } else if a.right {
        "right_click"
    } else {
        "click"
    };

    if let Some(idx) = a.element {
        let el = cached_element(&a.session_id, idx)?;
        press_element(&el)?;
        return Ok(json!({
            "status": "ok",
            "action": action,
            "target": { "element": idx },
            "note": "pressed the element's AXPress action; re-run computer_screenshot to verify",
        }));
    }

    let (Some(px), Some(py)) = (a.x, a.y) else {
        return Err(ToolError::Custom(
            "click needs coordinates (x,y from the latest screenshot) or an element index (from computer_read_screen)"
                .into(),
        ));
    };

    // Pixels (from the latest screenshot) → display points.
    let meta = latest_shot_meta(&a.work_dir)?;
    let point = cg::CGPoint {
        x: px_to_point(px, meta.png_w, meta.point_w) as f64,
        y: px_to_point(py, meta.png_h, meta.point_h) as f64,
    };

    let (down, up, button) = if a.right {
        (RIGHT_DOWN, RIGHT_UP, MOUSE_BUTTON_RIGHT)
    } else {
        (LEFT_DOWN, LEFT_UP, MOUSE_BUTTON_LEFT)
    };

    post_mouse(move_event, point, button, 0); // warp cursor to the target first
    if a.double {
        post_mouse(down, point, button, 1);
        post_mouse(up, point, button, 1);
        std::thread::sleep(std::time::Duration::from_millis(80));
        post_mouse(down, point, button, 2);
        post_mouse(up, point, button, 2);
    } else if a.hold_ms > 0 {
        // WKWebView hit-testing needs down/hold/up as ONE gesture — an
        // instant click is silently dropped (tauri-e2e lesson).
        post_mouse(down, point, button, 0);
        std::thread::sleep(std::time::Duration::from_millis(
            a.hold_ms.clamp(20, 2000) as u64
        ));
        post_mouse(up, point, button, 0);
    } else {
        post_mouse(down, point, button, 1);
        post_mouse(up, point, button, 1);
    }

    Ok(json!({
        "status": "ok",
        "action": action,
        "target": { "x": px, "y": py, "point": [point.x, point.y] },
        "note": "clicked; re-run computer_screenshot to verify the result",
    }))
}

const OBSERVE_ACT_VERIFY: &str = "Observe → act → re-observe: take computer_screenshot (or computer_read_screen) FIRST, act, then screenshot again to verify. Never chain blind actions.";

// ── Tool definitions ───────────────────────────────────────────────────────

/// `computer_screenshot` — capture a display as a PNG attached to the result.
pub struct ComputerScreenshotTool;

#[async_trait]
impl ToolHandler for ComputerScreenshotTool {
    fn name(&self) -> String {
        "computer_screenshot".into()
    }

    fn description(&self) -> String {
        "Take a screenshot of a display. The image is attached to the result so \
         you can see the screen. Coordinates in the result are screenshot \
         pixels — computer_click converts them automatically. ALWAYS take a \
         screenshot before any click and after every action to verify the \
         outcome; click coordinates must come from the LATEST screenshot."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "display": {"type": "integer", "description": "1-based display number (default 1 = main display)"}
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        let display = args["display"].as_u64().unwrap_or(1).clamp(1, 8) as u32;
        let work_dir = ctx.working_dir.clone();
        let (path_str, bytes, png_w, png_h, data_uri) = run_blocking(move || {
            let (path, bytes, png_w, png_h) = capture_screenshot(&work_dir, display)?;
            let path_str = path.to_string_lossy().to_string();
            // doc_reader derives the mime (png) and runs base64 — same output
            // as a hand-rolled encode, and it keeps the read off the workers.
            let data_uri = crate::doc_reader::read_image_base64(&path_str)
                .map_err(ToolError::Custom)?;
            Ok((path_str, bytes, png_w, png_h, data_uri))
        })
        .await?;
        Ok(ToolOutput {
            data: json!({
                "status": "ok",
                "path": path_str,
                "bytes": bytes,
                "png_width": png_w,
                "png_height": png_h,
                "note": "screenshot attached as an image; its pixel coordinates feed computer_click directly",
            }),
            next_prompt: Some("\n".into()),
            should_exit: false,
            images: vec![oz_core_types::ImageRef {
                url: data_uri,
                media_type: "image/png".into(),
            }],
        })
    }
}

/// `computer_read_screen` — focused app's AX tree with click-target indices.
pub struct ComputerReadScreenTool;

#[async_trait]
impl ToolHandler for ComputerReadScreenTool {
    fn name(&self) -> String {
        "computer_read_screen".into()
    }

    fn description(&self) -> String {
        "Read the focused application's accessibility tree as indexed text \
         elements. Prefer this over screenshots for buttons/fields/links: \
         pass an element index to computer_click to press it via AXPress \
         (more reliable than pixel coordinates). Elements are cached per \
         session; if the UI changed, re-run this before clicking."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "max_depth": {"type": "integer", "description": "tree depth limit (default 12)"},
                "max_elements": {"type": "integer", "description": "element cap (default 300)"}
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        let max_depth = args["max_depth"].as_u64().unwrap_or(12).clamp(1, 30) as usize;
        let max_elements = args["max_elements"]
            .as_u64()
            .unwrap_or(300)
            .clamp(10, 2000) as usize;
        let session_id = ctx.session_id.clone();
        let (tree, count) = run_blocking(move || {
            read_screen_tree(&session_id, max_depth, max_elements)
        })
        .await?;
        Ok(ToolOutput::success(json!({
            "status": "ok",
            "elements": count,
            "tree": tree,
            "note": OBSERVE_ACT_VERIFY,
        })))
    }
}

/// `computer_click` — pixel click or cached-element AXPress.
pub struct ComputerClickTool;

#[async_trait]
impl ToolHandler for ComputerClickTool {
    fn name(&self) -> String {
        "computer_click".into()
    }

    fn description(&self) -> String {
        "Click at pixel coordinates (x,y from the LATEST computer_screenshot) \
         or press a cached element index (from computer_read_screen). \
         button: left|right; double: true for double-click. On web content a \
         short hold (hold_ms=100) is more reliable than an instant click. \
         After clicking, take computer_screenshot to verify."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "x": {"type": "integer", "description": "pixel x (integer pixels from the latest screenshot)"},
                "y": {"type": "integer", "description": "pixel y"},
                "element": {"type": "integer", "description": "element index from computer_read_screen (overrides x/y)"},
                "button": {"type": "string", "enum": ["left", "right"], "description": "default left"},
                "double": {"type": "boolean", "description": "double-click (default false)"},
                "hold_ms": {"type": "integer", "description": "press hold duration in ms; use 100 for web content (default 0)"}
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        let work_dir = ctx.working_dir.clone();
        let session_id = ctx.session_id.clone();
        let data = run_blocking(move || {
            do_click(&ClickArgs {
                work_dir,
                session_id,
                x: args["x"].as_i64().map(|v| v as i32),
                y: args["y"].as_i64().map(|v| v as i32),
                element: args["element"].as_u64().map(|v| v as usize),
                right: args["button"].as_str() == Some("right"),
                double: args["double"].as_bool().unwrap_or(false),
                hold_ms: args["hold_ms"].as_u64().unwrap_or(0) as u32,
            })
        })
        .await?;
        Ok(ToolOutput::success(data))
    }
}

/// `computer_type` — type text into the focused field.
pub struct ComputerTypeTool;

#[async_trait]
impl ToolHandler for ComputerTypeTool {
    fn name(&self) -> String {
        "computer_type".into()
    }

    fn description(&self) -> String {
        "Type text into the currently focused input. Click the field first \
         (computer_click) if it is not focused. If a Chinese IME is active, \
         press cmd+ctrl+space via computer_key first to switch to ABC, or the \
         typed text becomes pinyin candidates. Verify with \
         computer_screenshot afterwards."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "text to type (newline types Enter)"}
            },
            "required": ["text"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        require_accessibility()?;
        let text = args["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ToolError::Custom("text is required".into()))?
            .to_string();
        let typed = text.chars().count();
        run_blocking(move || {
            use enigo::Keyboard as _;
            new_enigo()?
                .text(&text)
                .map_err(|e| ToolError::Custom(format!("typing failed: {e}")))?;
            Ok(())
        })
        .await?;
        Ok(ToolOutput::success(json!({
            "status": "ok",
            "typed_chars": typed,
            "note": "typed into the focused element; re-run computer_screenshot to verify",
        })))
    }
}

/// `computer_key` — press a key or chord, e.g. `cmd+shift+s`, `return`.
pub struct ComputerKeyTool;

#[async_trait]
impl ToolHandler for ComputerKeyTool {
    fn name(&self) -> String {
        "computer_key".into()
    }

    fn description(&self) -> String {
        "Press a key or modifier chord: \"return\", \"escape\", \"tab\", \
         \"cmd+c\", \"cmd+shift+p\", \"f5\", \"alt+left\". Modifiers: \
         cmd/meta, shift, ctrl/control, alt/option. Named keys: return, tab, \
         space, escape, backspace, delete, up, down, left, right, home, end, \
         pageup, pagedown, f1-f12, plus ASCII letters/digits (uppercase OK). \
         For '+' and non-ASCII text use computer_type."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "description": "key or + separated chord"},
                "repeat": {"type": "integer", "description": "repeat count (default 1, max 20)"}
            },
            "required": ["key"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        require_accessibility()?;
        let combo = args["key"]
            .as_str()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| ToolError::Custom("key is required".into()))?
            .trim()
            .to_string();
        let repeat = args["repeat"].as_u64().unwrap_or(1).clamp(1, 20) as usize;
        let (modifiers, key) = parse_key_combo(&combo)?;

        run_blocking(move || {
            let mut enigo = new_enigo()?;
            use enigo::{Direction, Keyboard as _};
            for m in &modifiers {
                enigo
                    .key(*m, Direction::Press)
                    .map_err(|e| ToolError::Custom(format!("modifier press failed: {e}")))?;
            }
            for _ in 0..repeat {
                enigo
                    .key(key, Direction::Click)
                    .map_err(|e| ToolError::Custom(format!("key press failed: {e}")))?;
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            for m in modifiers.iter().rev() {
                enigo
                    .key(*m, Direction::Release)
                    .map_err(|e| ToolError::Custom(format!("modifier release failed: {e}")))?;
            }
            Ok(())
        })
        .await?;
        Ok(ToolOutput::success(json!({
            "status": "ok",
            "key": combo,
            "repeat": repeat,
            "note": "key pressed; re-run computer_screenshot to verify",
        })))
    }
}

/// `computer_scroll` — scroll at the current or given position.
pub struct ComputerScrollTool;

#[async_trait]
impl ToolHandler for ComputerScrollTool {
    fn name(&self) -> String {
        "computer_scroll".into()
    }

    fn description(&self) -> String {
        "Scroll the window under the cursor. positive amount scrolls down, \
         negative scrolls up (one amount unit ≈ one wheel notch). Optionally \
         move the mouse to pixel x,y (from the latest screenshot) first so \
         the scroll lands on the right pane. Verify with computer_screenshot."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "amount": {"type": "integer", "description": "positive = down, negative = up"},
                "x": {"type": "integer", "description": "optional pixel cursor x before scrolling"},
                "y": {"type": "integer", "description": "optional pixel cursor y before scrolling"}
            },
            "required": ["amount"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        require_macos()?;
        require_accessibility()?;
        let amount = args["amount"].as_i64().unwrap_or(0);
        if amount == 0 {
            return Ok(ToolOutput::bad_json(
                "computer_scroll: amount is required (positive = down, negative = up)",
            ));
        }
        let work_dir = ctx.working_dir.clone();
        let x = args["x"].as_i64().map(|v| v as i32);
        let y = args["y"].as_i64().map(|v| v as i32);
        let lines = -(amount.clamp(-100, 100) as i32); // CG positive line = scroll up
        run_blocking(move || {
            if let (Some(px), Some(py)) = (x, y) {
                let meta = latest_shot_meta(&work_dir)?;
                let point = cg::CGPoint {
                    x: px_to_point(px, meta.png_w, meta.point_w) as f64,
                    y: px_to_point(py, meta.png_h, meta.point_h) as f64,
                };
                cg::post_mouse(cg::MOUSE_MOVED, point, 0, 0);
            }
            cg::post_scroll_lines(lines);
            Ok(())
        })
        .await?;
        Ok(ToolOutput::success(json!({
            "status": "ok",
            "amount": amount,
            "note": "scrolled; re-run computer_screenshot to verify",
        })))
    }
}

/// Register every computer-use tool (called from `registry::build_manual`).
pub fn register_all(reg: &mut crate::registry::ToolRegistry) {
    reg.register(ComputerScreenshotTool);
    reg.register(ComputerReadScreenTool);
    reg.register(ComputerClickTool);
    reg.register(ComputerTypeTool);
    reg.register(ComputerKeyTool);
    reg.register(ComputerScrollTool);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn key_combo_parses_modifiers_and_key() {
        let (mods, key) = parse_key_combo("cmd+shift+s").expect("parse");
        assert_eq!(mods.len(), 2);
        assert!(matches!(mods[0], enigo::Key::Meta));
        assert!(matches!(mods[1], enigo::Key::Shift));
        assert!(matches!(key, enigo::Key::Unicode('s')));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn key_combo_uppercase_applies_shift() {
        let (mods, key) = parse_key_combo("A").expect("parse");
        assert_eq!(mods.len(), 1);
        assert!(matches!(mods[0], enigo::Key::Shift));
        assert!(matches!(key, enigo::Key::Unicode('a')));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn key_combo_named_keys() {
        let (mods, key) = parse_key_combo("return").expect("parse");
        assert!(mods.is_empty());
        assert!(matches!(key, enigo::Key::Return));

        let (_, key) = parse_key_combo("alt+Left").expect("parse");
        assert!(matches!(key, enigo::Key::LeftArrow));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn key_combo_rejects_unsafe_fallbacks() {
        // enigo's keycode lookup falls back to the 'a' key for layout-unknown
        // characters — these must be rejected, not silently mistyped.
        assert!(parse_key_combo("中").is_err());
        assert!(parse_key_combo("+").is_err());
        assert!(parse_key_combo("cmd+banana").is_err());
        assert!(parse_key_combo("").is_err());
    }

    #[test]
    fn tool_metadata_is_consistent() {
        let mut reg = crate::registry::ToolRegistry::new();
        register_all(&mut reg);
        for name in [
            "computer_screenshot",
            "computer_read_screen",
            "computer_click",
            "computer_type",
            "computer_key",
            "computer_scroll",
        ] {
            let t = reg
                .get(name)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            assert_eq!(t.name(), name);
            let schema = t.parameters();
            assert_eq!(schema["type"], "object", "{name} schema root");
            assert!(
                t.description().len() > 60,
                "{name} description should teach the observe-act-verify loop"
            );
        }
    }
}
