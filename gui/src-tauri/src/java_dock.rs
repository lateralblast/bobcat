// bobcat: dock the Java viewer's window over the console area of the main window (macOS).
//
// The viewer is a separate program with a window of its own, which cannot be made a child of ours.
// Through the Accessibility API (the permission that also silences the viewer's keyboard library
// warning) bobcat reads its own window's frame and moves and resizes the viewer's window to the area
// under the toolbar, above the status bar, and keeps doing so while the main window moves or
// is resized or minimized. The viewer's window stays a window of another program: when the main
// window is clicked it would come to the front and hide the viewer, so the viewer is brought back to the
// front shortly after.

use crate::html5;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

type Ax = *const c_void;
type CFRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Point { x: f64, y: f64 }
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Size { w: f64, h: f64 }
#[derive(Clone, Copy, Debug, Default)]
struct Rect { x: f64, y: f64, w: f64, h: f64 }

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFRef) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ax;
    fn AXUIElementCopyAttributeValue(el: Ax, attr: CFStringRef, value: *mut CFRef) -> i32;
    fn AXUIElementSetAttributeValue(el: Ax, attr: CFStringRef, value: CFRef) -> i32;
    fn AXValueCreate(kind: i32, ptr: *const c_void) -> CFRef;
    fn AXValueGetValue(value: CFRef, kind: i32, ptr: *mut c_void) -> bool;
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    fn CGEventCreateKeyboardEvent(source: CFRef, key: u16, down: bool) -> CFRef;
    fn CGEventSetFlags(event: CFRef, flags: u64);
    fn CGEventPost(tap: u32, event: CFRef);
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
const SHIFT_FLAG: u64 = 0x0002_0000;
const KEY_SHIFT: u16 = 56;
const AX_POINT: i32 = 1;
const AX_SIZE: i32 = 2;

/// Is bobcat allowed to use the Accessibility API? With `prompt`, macOS shows its permission dialog
/// the first time.
fn trusted(prompt: bool) -> bool {
    let opts = CFDictionary::from_CFType_pairs(&[(CFString::new("AXTrustedCheckOptionPrompt").as_CFType(), CFBoolean::from(prompt).as_CFType())]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef() as CFRef) }
}

/// An element the Accessibility API handed out (retained until dropped).
struct El(CFType);

impl El {
    fn app(pid: i32) -> El {
        El(unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) })
    }
    fn raw(&self) -> Ax {
        self.0.as_CFTypeRef()
    }
    fn attr(&self, name: &str) -> Option<CFType> {
        let key = CFString::new(name);
        let mut out: CFRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(self.raw(), key.as_concrete_TypeRef(), &mut out) };
        (err == 0 && !out.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(out) })
    }
    fn set(&self, name: &str, value: CFRef) -> bool {
        let key = CFString::new(name);
        unsafe { AXUIElementSetAttributeValue(self.raw(), key.as_concrete_TypeRef(), value) == 0 }
    }
    fn flag(&self, name: &str) -> Option<bool> {
        self.attr(name)?.downcast::<CFBoolean>().map(bool::from)
    }
    fn set_flag(&self, name: &str, on: bool) -> bool {
        self.set(name, CFBoolean::from(on).as_CFTypeRef())
    }
    fn title(&self) -> String {
        self.attr("AXTitle").and_then(|t| t.downcast::<CFString>()).map(|s| s.to_string()).unwrap_or_default()
    }
    fn frame(&self) -> Option<Rect> {
        let (mut p, mut s) = (Point::default(), Size::default());
        let ok_p = unsafe { AXValueGetValue(self.attr("AXPosition")?.as_CFTypeRef(), AX_POINT, &mut p as *mut _ as *mut c_void) };
        let ok_s = unsafe { AXValueGetValue(self.attr("AXSize")?.as_CFTypeRef(), AX_SIZE, &mut s as *mut _ as *mut c_void) };
        (ok_p && ok_s).then_some(Rect { x: p.x, y: p.y, w: s.w, h: s.h })
    }
    fn set_frame(&self, r: Rect) {
        let p = Point { x: r.x, y: r.y };
        let s = Size { w: r.w, h: r.h };
        unsafe {
            let pv = CFType::wrap_under_create_rule(AXValueCreate(AX_POINT, &p as *const _ as *const c_void));
            let sv = CFType::wrap_under_create_rule(AXValueCreate(AX_SIZE, &s as *const _ as *const c_void));
            // position, size, then position again: a resize can push the window back inside the screen
            self.set("AXPosition", pv.as_CFTypeRef());
            self.set("AXSize", sv.as_CFTypeRef());
            self.set("AXPosition", pv.as_CFTypeRef());
        }
    }
    fn elements(&self, name: &str) -> Vec<El> {
        let Some(list) = self.attr(name) else { return Vec::new() };
        let Some(list) = list.downcast::<core_foundation::array::CFArray>() else { return Vec::new() };
        // the array's items are borrowed: retain each one so it outlives the array
        list.iter().map(|w| El(unsafe { CFType::wrap_under_get_rule(*w) })).collect()
    }
    fn windows(&self) -> Vec<El> {
        self.elements("AXWindows")
    }
}

/// The height of the title bar of the main window: the web view reaches under it, and the page
/// starts below it.
fn title_bar_height(app: &AppHandle) -> f64 {
    use objc2::encode::{Encode, Encoding};
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pt { x: f64, y: f64 }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Sz { w: f64, h: f64 }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rc { o: Pt, s: Sz }
    unsafe impl Encode for Pt { const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]); }
    unsafe impl Encode for Sz { const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]); }
    unsafe impl Encode for Rc { const ENCODING: Encoding = Encoding::Struct("CGRect", &[Pt::ENCODING, Sz::ENCODING]); }
    let Some(main) = app.get_webview_window("main") else { return 28.0 };
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = main.with_webview(move |pw| unsafe {
        let wk = pw.inner() as *mut AnyObject;
        let window: *mut AnyObject = msg_send![wk, window];
        let parent: *mut AnyObject = msg_send![wk, superview];
        if window.is_null() || parent.is_null() {
            return;
        }
        let pframe: Rc = msg_send![parent, frame];
        let layout: Rc = msg_send![window, contentLayoutRect];
        let _ = tx.send((pframe.s.h - layout.s.h).max(0.0));
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or(28.0)
}

/// The largest window of an application.
fn largest(app: &El) -> Option<(El, Rect)> {
    app.windows().into_iter().filter_map(|w| w.frame().map(|f| (w, f))).max_by(|a, b| (a.1.w * a.1.h).total_cmp(&(b.1.w * b.1.h)))
}

/// The viewer's console window (not one of its dialogs): the one titled with the iDRAC's address,
/// else the largest big one.
fn console_window(viewer: &El, ip: &str) -> Option<(El, Rect)> {
    let all: Vec<(El, Rect)> = viewer.windows().into_iter().filter_map(|w| w.frame().map(|f| (w, f))).collect();
    if let Some(i) = all.iter().position(|(w, _)| w.title().contains(ip)) {
        return all.into_iter().nth(i);
    }
    all.into_iter().filter(|(_, f)| f.w >= 640.0 && f.h >= 480.0).max_by(|a, b| (a.1.w * a.1.h).total_cmp(&(b.1.w * b.1.h)))
}

fn near(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 1.5 && (a.y - b.y).abs() < 1.5 && (a.w - b.w).abs() < 1.5 && (a.h - b.h).abs() < 1.5
}

/// The iDRAC address of the running viewer, for finding its console window.
static CURRENT_IP: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Keep the viewer's window docked while the viewer (`pid`) runs.
pub fn start(app: AppHandle, pid: u32, ip: String) {
    *CURRENT_IP.lock().unwrap() = ip.clone();
    if std::env::var("BOBCAT_JAVA_DOCK").is_ok_and(|v| v == "0") {
        return;
    }
    std::thread::spawn(move || {
        if !trusted(true) {
            crate::status(&app, "Java console: grant bobcat the Accessibility permission (System Settings > Privacy & Security) to dock its window");
            return;
        }
        let debug = html5::debug_on();
        let title = title_bar_height(&app);
        let me = El::app(std::process::id() as i32);
        let viewer = El::app(pid as i32);
        let mut minimized = false;
        let mut front_since: Option<Instant> = None;
        let mut last: Option<Rect> = None;
        while crate::java_console::viewer_running(pid) {
            std::thread::sleep(Duration::from_millis(150));
            let Some((main, mframe)) = largest(&me) else { continue };
            if main.flag("AXMinimized") == Some(true) {
                if !minimized {
                    for w in viewer.windows() {
                        w.set_flag("AXMinimized", true);
                    }
                    minimized = true;
                }
                continue;
            }
            if minimized {
                for w in viewer.windows() {
                    w.set_flag("AXMinimized", false);
                }
                minimized = false;
            }
            let (bar, whole) = html5::layout_heights();
            if bar < 20.0 {
                continue; // the page has not reported its toolbar height yet: do not cover it
            }
            let (top, status) = (bar, (whole - bar).max(0.0));
            let target = Rect {
                x: mframe.x,
                y: mframe.y + title + top,
                w: mframe.w,
                h: (mframe.h - title - top - status).max(200.0),
            };
            let Some((win, vframe)) = console_window(&viewer, &ip) else { continue };
            if !near(vframe, target) {
                if debug && last.is_none_or(|l| !near(l, target)) {
                    eprintln!("[bobcat] dock: main {mframe:?}, title bar {title}, toolbar {top}, status bar {status}: viewer {vframe:?} -> {target:?}");
                }
                win.set_frame(target);
                last = Some(target);
            }
            // clicking the main window puts it over the viewer: bring the viewer back once the
            // click is over (not while a mouse button is held, e.g. when dragging the title bar)
            if me.flag("AXFrontmost") == Some(true) {
                let since = *front_since.get_or_insert_with(Instant::now);
                let pressed = unsafe { CGEventSourceButtonState(0, 0) };
                if since.elapsed() > Duration::from_millis(400) && !pressed {
                    viewer.set_flag("AXFrontmost", true);
                    front_since = None;
                }
            } else {
                front_since = None;
            }
        }
    });
}

/// Bring the viewer to the front and wait until macOS says it is there.
fn bring_to_front(viewer: &El) -> bool {
    viewer.set_flag("AXFrontmost", true);
    for _ in 0..20 {
        if viewer.flag("AXFrontmost") == Some(true) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// US keyboard layout: the macOS virtual key code of a character and whether Shift is needed.
fn key_for(ch: char) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] = [0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    // each shifted symbol and the unshifted key that types it
    const SHIFTED: [(char, char); 21] = [
        ('~', '`'), ('!', '1'), ('@', '2'), ('#', '3'), ('$', '4'), ('%', '5'), ('^', '6'), ('&', '7'), ('*', '8'), ('(', '9'), (')', '0'),
        ('_', '-'), ('+', '='), ('{', '['), ('}', ']'), ('|', '\\'), (':', ';'), ('"', '\''), ('<', ','), ('>', '.'), ('?', '/'),
    ];
    let base = |c: char| -> Option<u16> {
        Some(match c {
            'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
            '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
            '-' => 27,
            '=' => 24,
            '[' => 33,
            ']' => 30,
            '\\' => 42,
            ';' => 41,
            '\'' => 39,
            ',' => 43,
            '.' => 47,
            '/' => 44,
            '`' => 50,
            ' ' => 49,
            '\n' => 36,
            '\t' => 48,
            _ => return None,
        })
    };
    if ch.is_ascii_uppercase() {
        return base(ch.to_ascii_lowercase()).map(|k| (k, true));
    }
    if let Some((_, plain)) = SHIFTED.iter().find(|(s, _)| *s == ch) {
        return base(*plain).map(|k| (k, true));
    }
    base(ch).map(|k| (k, false))
}

fn post_key(key: u16, down: bool, shift: bool) {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null(), key, down);
        if ev.is_null() {
            return;
        }
        let ev = CFType::wrap_under_create_rule(ev);
        CGEventSetFlags(ev.as_CFTypeRef(), if shift { SHIFT_FLAG } else { 0 });
        CGEventPost(0, ev.as_CFTypeRef()); // kCGHIDEventTap: goes to the frontmost application
    }
}

fn running_viewer() -> Result<u32, String> {
    crate::java_console::current_pid().ok_or_else(|| "the Java console is not running".to_string())
}

/// Type the clipboard into the viewer as key presses (US layout), the way a person would.
pub fn paste(text: &str) -> Result<usize, String> {
    if !trusted(false) {
        return Err("bobcat needs the Accessibility permission to type into the viewer (System Settings > Privacy & Security > Accessibility)".into());
    }
    let pid = running_viewer()?;
    let viewer = El::app(pid as i32);
    let debug = html5::debug_on();
    let front = bring_to_front(&viewer);
    if debug {
        let wins: Vec<String> = viewer.windows().iter().map(|w| format!("{:?} {:?} main={:?} focused={:?}", w.title(), w.frame(), w.flag("AXMain"), w.flag("AXFocused"))).collect();
        eprintln!("[bobcat] java paste: viewer pid {pid}, brought to front: {front}, windows: {wins:?}");
    }
    if !front {
        return Err("could not bring the Java viewer to the front".into());
    }
    std::thread::sleep(Duration::from_millis(200));
    let mut shift = false;
    let mut typed = 0;
    for ch in text.chars() {
        let Some((key, needs_shift)) = key_for(ch) else { continue };
        if typed % 20 == 0 && viewer.flag("AXFrontmost") != Some(true) {
            if shift {
                post_key(KEY_SHIFT, false, false);
            }
            return Err(format!("the Java viewer lost the focus after {typed} characters, so pasting stopped"));
        }
        if needs_shift != shift {
            post_key(KEY_SHIFT, needs_shift, needs_shift);
            shift = needs_shift;
            std::thread::sleep(Duration::from_millis(4));
        }
        post_key(key, true, shift);
        std::thread::sleep(Duration::from_millis(4));
        post_key(key, false, shift);
        std::thread::sleep(Duration::from_millis(14));
        typed += 1;
    }
    if shift {
        post_key(KEY_SHIFT, false, false);
    }
    if debug {
        eprintln!("[bobcat] java paste: sent {typed} key presses, viewer frontmost now: {:?}", viewer.flag("AXFrontmost"));
    }
    Ok(typed)
}

/// Read the viewer's console screen with OCR: capture the window's area on screen (needs the Screen
/// Recording permission) and give it to tesseract; the text goes on the clipboard.
pub fn copy_text() -> Result<String, String> {
    if !trusted(false) {
        return Err("bobcat needs the Accessibility permission to find the viewer's window".into());
    }
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        unsafe { CGRequestScreenCaptureAccess() };
        return Err("bobcat needs the Screen Recording permission to read the console (System Settings > Privacy & Security > Screen & System Audio Recording); grant it, restart bobcat and try again".into());
    }
    let pid = running_viewer()?;
    let viewer = El::app(pid as i32);
    let ip = CURRENT_IP.lock().unwrap().clone();
    let (_, frame) = console_window(&viewer, &ip).ok_or("the Java console window is not showing yet")?;
    if !bring_to_front(&viewer) {
        return Err("could not bring the Java viewer to the front".into());
    }
    std::thread::sleep(Duration::from_millis(300));
    // only the console picture: leave out the window's title bar and the viewer's information bar
    // at the bottom ("Current User(s): ..."); BOBCAT_JAVA_COPY_CROP="top,bottom" (points) adjusts it
    let (crop_top, crop_bottom) = std::env::var("BOBCAT_JAVA_COPY_CROP")
        .ok()
        .and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))))
        .unwrap_or((28.0, 26.0));
    let (x, y, w, h) = (frame.x, frame.y + crop_top, frame.w, frame.h - crop_top - crop_bottom);
    let file = std::env::temp_dir().join(format!("bobcat-java-copy-{}.png", std::process::id()));
    let st = std::process::Command::new("screencapture")
        .args(["-x", "-t", "png", "-R", &format!("{},{},{},{}", x.round(), y.round(), w.round(), h.round())])
        .arg(&file)
        .status()
        .map_err(|e| e.to_string())?;
    let png = std::fs::read(&file).map_err(|_| "screencapture produced no image".to_string());
    let _ = std::fs::remove_file(&file);
    if !st.success() {
        return Err("screencapture failed".into());
    }
    let png = png?;
    let clean = crate::java_console::prepare_for_ocr(&png).unwrap_or_else(|_| png.clone());
    if html5::debug_on() {
        let dir = std::env::temp_dir();
        let _ = std::fs::write(dir.join("bobcat-java-copy-raw.png"), &png);
        let _ = std::fs::write(dir.join("bobcat-java-copy-clean.png"), &clean);
        eprintln!("[bobcat] java copy: region {x},{y} {w}x{h}; images saved as {}/bobcat-java-copy-{{raw,clean}}.png", dir.display());
    }
    crate::ocr_png_to_clipboard_args(&clean, &["-c", "preserve_interword_spaces=1"])
}
