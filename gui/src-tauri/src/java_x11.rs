// bobcat: dock the Java viewer's window over the console area of the main window, and paste into /
// copy from it (Linux, X11).
//
// The viewer is a separate program (an X11 client, also under Wayland through XWayland). Reparenting
// its window into ours did not work (the window manager and the viewer's toolkit take it back), so,
// as on macOS, it stays a window of its own: bobcat finds it by `_NET_WM_PID`, makes it frameless and
// transient for the main window (stays above it, minimizes with it) and keeps it laid over the area
// between the toolbar and the status bar, following the main window. Paste types key presses with the XTest
// extension; Copy reads the window's pixels with GetImage and gives them to tesseract.
// The main window must be an X11 window: `main()` sets GDK_BACKEND=x11 unless it is already set.
// Needs a screen where the window contents can be read back (a compositing XWayland or plain X11).

use crate::html5;
use std::sync::Mutex;
use std::time::Duration;
use tauri::AppHandle;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, *};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// The viewer's console window once it is docked, and the main window it was docked into.
static DOCKED: Mutex<Option<(Window, Window)>> = Mutex::new(None);
/// The iDRAC address of the running viewer, for finding its console window.
static CURRENT_IP: Mutex<String> = Mutex::new(String::new());

fn connect() -> Result<(RustConnection, Window), String> {
    let (conn, screen) = RustConnection::connect(None).map_err(|e| format!("cannot connect to the X server: {e}"))?;
    let root = conn.setup().roots[screen].root;
    Ok((conn, root))
}

fn atom(conn: &RustConnection, name: &str) -> Option<Atom> {
    conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok().map(|r| r.atom)
}

fn property_u32(conn: &RustConnection, win: Window, name: &str, kind: impl Into<Atom>) -> Option<u32> {
    let a = atom(conn, name)?;
    conn.get_property(false, win, a, kind, 0, 1).ok()?.reply().ok()?.value32()?.next()
}

fn title(conn: &RustConnection, win: Window) -> String {
    for (name, kind) in [("_NET_WM_NAME", "UTF8_STRING"), ("WM_NAME", "STRING")] {
        if let (Some(n), Some(k)) = (atom(conn, name), atom(conn, kind)) {
            if let Some(r) = conn.get_property(false, win, n, k, 0, 256).ok().and_then(|c| c.reply().ok()) {
                if !r.value.is_empty() {
                    return String::from_utf8_lossy(&r.value).into_owned();
                }
            }
        }
    }
    String::new()
}

/// The windows of a process: a top-level window may sit inside a window manager frame, so the
/// root's children and their children are checked.
fn windows_of(conn: &RustConnection, root: Window, pid: u32) -> Vec<Window> {
    let mut out = Vec::new();
    let Some(top) = conn.query_tree(root).ok().and_then(|c| c.reply().ok()) else { return out };
    for w in top.children {
        let mut candidates = vec![w];
        if let Some(sub) = conn.query_tree(w).ok().and_then(|c| c.reply().ok()) {
            candidates.extend(sub.children);
        }
        for c in candidates {
            if property_u32(conn, c, "_NET_WM_PID", AtomEnum::CARDINAL) == Some(pid) {
                out.push(c);
            }
        }
    }
    out
}

fn size(conn: &RustConnection, win: Window) -> Option<(i16, i16, u16, u16)> {
    conn.get_geometry(win).ok()?.reply().ok().map(|g| (g.x, g.y, g.width, g.height))
}

fn mapped(conn: &RustConnection, win: Window) -> bool {
    conn.get_window_attributes(win).ok().and_then(|c| c.reply().ok()).is_some_and(|a| a.map_state == MapState::VIEWABLE)
}

/// The main window: the biggest viewable window of this process.
fn main_window(conn: &RustConnection, root: Window) -> Option<Window> {
    windows_of(conn, root, std::process::id())
        .into_iter()
        .filter(|w| mapped(conn, *w))
        .filter_map(|w| size(conn, w).map(|g| (w, g.2 as u32 * g.3 as u32)))
        .max_by_key(|(_, a)| *a)
        .map(|(w, _)| w)
}

/// The viewer's console window (not one of its dialogs): the one titled with the iDRAC's address,
/// else the biggest one that is at least 640x480.
fn console_window(conn: &RustConnection, root: Window, pid: u32, ip: &str) -> Option<Window> {
    let all: Vec<(Window, u16, u16)> = windows_of(conn, root, pid)
        .into_iter()
        .filter(|w| mapped(conn, *w))
        .filter_map(|w| size(conn, w).map(|g| (w, g.2, g.3)))
        .collect();
    if let Some(w) = all.iter().find(|(w, _, _)| title(conn, *w).contains(ip)) {
        return Some(w.0);
    }
    all.into_iter().filter(|(_, w, h)| *w >= 640 && *h >= 480).max_by_key(|(_, w, h)| *w as u32 * *h as u32).map(|(win, _, _)| win)
}

/// Where a window's top left corner is on the screen.
fn origin(conn: &RustConnection, root: Window, win: Window) -> Option<(i16, i16)> {
    conn.translate_coordinates(win, root, 0, 0).ok()?.reply().ok().map(|r| (r.dst_x, r.dst_y))
}

fn send_to_root(conn: &RustConnection, root: Window, win: Window, name: &str, data: [u32; 5]) {
    if let Some(kind) = atom(conn, name) {
        let ev = ClientMessageEvent::new(32, win, kind, data);
        let _ = conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, ev);
        let _ = conn.flush();
    }
}

/// Ask the window manager to make the window the active one (it gets the keyboard).
fn activate(conn: &RustConnection, root: Window, win: Window) {
    send_to_root(conn, root, win, "_NET_ACTIVE_WINDOW", [1, 0, 0, 0, 0]);
}

fn active_window(conn: &RustConnection, root: Window) -> Option<Window> {
    property_u32(conn, root, "_NET_ACTIVE_WINDOW", AtomEnum::WINDOW)
}

/// Make the viewer a plain frameless window that stays above the main window and goes with it
/// (minimize), so it can be laid over the console area. It stays a window of its own: the window
/// manager and the viewer's toolkit take back any reparenting.
fn make_dockable(conn: &RustConnection, root: Window, viewer: Window, main: Window) {
    if let Some(a) = atom(conn, "WM_TRANSIENT_FOR") {
        let _ = conn.change_property32(PropMode::REPLACE, viewer, a, AtomEnum::WINDOW, &[main]);
    }
    if let Some(a) = atom(conn, "_MOTIF_WM_HINTS") {
        // flags = decorations, decorations = none
        let _ = conn.change_property32(PropMode::REPLACE, viewer, a, a, &[2, 0, 0, 0, 0]);
    }
    if let (Some(t), Some(p)) = (atom(conn, "_NET_WM_STATE_SKIP_TASKBAR"), atom(conn, "_NET_WM_STATE_SKIP_PAGER")) {
        send_to_root(conn, root, viewer, "_NET_WM_STATE", [1, t, p, 1, 0]);
    }
    let _ = conn.flush();
}

/// Keep the viewer's window laid over the console area of the main window while the viewer (`pid`) runs.
pub fn start(app: AppHandle, pid: u32, ip: String) {
    *CURRENT_IP.lock().unwrap() = ip.clone();
    *DOCKED.lock().unwrap() = None;
    if std::env::var("BOBCAT_JAVA_DOCK").is_ok_and(|v| v == "0") {
        return;
    }
    std::thread::spawn(move || {
        let debug = html5::debug_on();
        let (conn, root) = match connect() {
            Ok(c) => c,
            Err(e) => {
                crate::status(&app, format!("Java console: {e}"));
                return;
            }
        };
        let mut told = false;
        let mut last: Option<(i16, i16, u16, u16)> = None;
        while crate::java_console::viewer_running(pid) {
            std::thread::sleep(Duration::from_millis(150));
            let (bar, whole) = html5::layout_heights();
            if bar < 20.0 {
                continue; // the page has not reported its toolbar height yet: do not cover it
            }
            let (top, status) = (bar as i16, (whole - bar).max(0.0) as i16);
            let Some(main) = main_window(&conn, root) else {
                if !told && debug {
                    eprintln!("[bobcat] dock: no X11 window of bobcat found (is it running under Wayland? set GDK_BACKEND=x11)");
                    told = true;
                }
                continue;
            };
            let (Some((mx, my)), Some((_, _, mw, mh))) = (origin(&conn, root, main), size(&conn, main)) else { continue };
            let target = (mx, my + top, mw, (mh as i16 - top - status).max(200) as u16);
            let docked = *DOCKED.lock().unwrap();
            let viewer = match docked {
                Some((v, m)) if m == main && size(&conn, v).is_some() => v,
                _ => {
                    let Some(v) = console_window(&conn, root, pid, &ip) else { continue };
                    make_dockable(&conn, root, v, main);
                    *DOCKED.lock().unwrap() = Some((v, main));
                    last = None;
                    if debug {
                        eprintln!("[bobcat] dock: viewer window {v:#x} made frameless and transient for main {main:#x}");
                    }
                    crate::status(&app, format!("Java console of {ip} (docked under the toolbar)"));
                    v
                }
            };
            // the client area's place on the screen (the window manager's frame is around it) and size
            let now = origin(&conn, root, viewer).zip(size(&conn, viewer));
            let here = now.map(|((x, y), (_, _, w, h))| (x, y, w, h));
            if here != Some(target) {
                if debug && last != Some(target) {
                    eprintln!("[bobcat] dock: main at {mx},{my} {mw}x{mh}, toolbar {top}, status bar {status}: viewer {here:?} -> {target:?}");
                }
                let _ = conn.configure_window(viewer, &ConfigureWindowAux::new().x(target.0 as i32).y(target.1 as i32).width(target.2 as u32).height(target.3 as u32));
                let _ = conn.flush();
                last = Some(target);
            }
        }
        *DOCKED.lock().unwrap() = None;
    });
}

fn docked_viewer() -> Result<Window, String> {
    crate::java_console::current_pid().ok_or_else(|| "the Java console is not running".to_string())?;
    DOCKED.lock().unwrap().map(|(v, _)| v).ok_or_else(|| "the Java console window is not showing yet".to_string())
}

/// The key code and shift state that types a character on the current X keyboard layout.
fn key_for(map: &GetKeyboardMappingReply, min: u8, ch: char) -> Option<(u8, bool)> {
    let keysym: u32 = match ch {
        '\n' => 0xff0d,
        '\t' => 0xff09,
        c if (c as u32) < 0x100 => c as u32,
        _ => return None,
    };
    let per = map.keysyms_per_keycode as usize;
    for (i, chunk) in map.keysyms.chunks(per).enumerate() {
        if chunk.first() == Some(&keysym) {
            return Some((min + i as u8, false));
        }
        if chunk.get(1) == Some(&keysym) {
            return Some((min + i as u8, true));
        }
    }
    None
}

/// Type the text into the viewer as key presses (XTest), as a person would.
pub fn paste(text: &str) -> Result<usize, String> {
    let viewer = docked_viewer()?;
    let (conn, root) = connect()?;
    if conn.query_extension(b"XTEST").ok().and_then(|c| c.reply().ok()).is_none_or(|e| !e.present) {
        return Err("the X server has no XTEST extension, so bobcat cannot type into the viewer".into());
    }
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    // Shift_L (keysym 0xffe1) is not Latin-1: look it up by keysym
    let shift_key = map.keysyms.chunks(map.keysyms_per_keycode as usize).position(|c| c.first() == Some(&0xffe1)).map(|i| min + i as u8).ok_or("no Shift key in the keyboard layout")?;
    let debug = html5::debug_on();
    activate(&conn, root, viewer);
    for _ in 0..20 {
        if active_window(&conn, root) == Some(viewer) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if active_window(&conn, root) != Some(viewer) {
        return Err("could not bring the Java viewer to the front".into());
    }
    std::thread::sleep(Duration::from_millis(200));
    let fake = |down: bool, key: u8| {
        let _ = conn.xtest_fake_input(if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT }, key, 0, root, 0, 0, 0);
        let _ = conn.flush();
    };
    let mut shifted = false;
    let mut typed = 0;
    for ch in text.chars() {
        let Some((key, needs_shift)) = key_for(&map, min, ch) else { continue };
        if typed % 20 == 0 && active_window(&conn, root) != Some(viewer) {
            if shifted {
                fake(false, shift_key);
            }
            return Err(format!("the Java viewer lost the focus after {typed} characters, so pasting stopped"));
        }
        if needs_shift != shifted {
            fake(needs_shift, shift_key);
            shifted = needs_shift;
            std::thread::sleep(Duration::from_millis(4));
        }
        fake(true, key);
        std::thread::sleep(Duration::from_millis(4));
        fake(false, key);
        std::thread::sleep(Duration::from_millis(14));
        typed += 1;
    }
    if shifted {
        fake(false, shift_key);
    }
    if debug {
        eprintln!("[bobcat] java paste: sent {typed} key presses to {viewer:#x}");
    }
    Ok(typed)
}

/// Read the viewer's console screen with OCR: read the window's pixels and give them to tesseract;
/// the text goes on the clipboard.
pub fn copy_text() -> Result<String, String> {
    let viewer = docked_viewer()?;
    let (conn, _) = connect()?;
    let (_, _, w, h) = size(&conn, viewer).ok_or("the Java console window is gone")?;
    // only the console picture: leave out the viewer's information bar at the bottom
    // ("Current User(s): ..."); BOBCAT_JAVA_COPY_CROP="top,bottom" (pixels) adjusts it
    let (crop_top, crop_bottom) = std::env::var("BOBCAT_JAVA_COPY_CROP")
        .ok()
        .and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?))))
        .unwrap_or((0, 26));
    let ch = h.saturating_sub(crop_top + crop_bottom);
    if ch < 50 {
        return Err("the Java console window is too small to read".into());
    }
    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, viewer, 0, crop_top as i16, w, ch, !0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("could not read the viewer's window: {e}"))?;
    // 24-bit colour in 32-bit pixels, blue first
    let mut rgb = Vec::with_capacity(w as usize * ch as usize * 3);
    for px in img.data.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let buf = image::RgbImage::from_raw(w as u32, ch as u32, rgb).ok_or("unexpected image format from the X server")?;
    let mut png = std::io::Cursor::new(Vec::new());
    buf.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let png = png.into_inner();
    let clean = crate::java_console::prepare_for_ocr(&png).unwrap_or_else(|_| png.clone());
    if html5::debug_on() {
        let dir = std::env::temp_dir();
        let _ = std::fs::write(dir.join("bobcat-java-copy-raw.png"), &png);
        let _ = std::fs::write(dir.join("bobcat-java-copy-clean.png"), &clean);
        eprintln!("[bobcat] java copy: {w}x{ch}; images saved as {}/bobcat-java-copy-{{raw,clean}}.png", dir.display());
    }
    crate::ocr_png_to_clipboard_args(&clean, &["-c", "preserve_interword_spaces=1"])
}
