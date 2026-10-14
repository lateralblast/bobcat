// Intel AMT virtual media (IDE-R) for the desktop app: serves a CD/DVD image from this computer to
// the AMT machine over the redirection channel, next to the KVM / SOL session (a separate connection).
// The protocol is in amt_ider.rs (a copy of the bobcat-cli file). Results reach the page as
// "ider-status" events; the commands return at once.

use crate::{amt_ider, amt_target, keychain};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

struct Active {
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

static SESSION: Mutex<Option<Active>> = Mutex::new(None);

#[derive(Serialize, Clone)]
struct IderStatus {
    mounted: bool,
    text: String,
}

fn emit(app: &AppHandle, mounted: bool, text: impl Into<String>) {
    let text = text.into();
    eprintln!("[bobcat] virtual media: {text}");
    let _ = app.emit("ider-status", IderStatus { mounted, text });
}

/// End the running session (if any) and wait a moment for it to hang up.
pub fn stop() {
    let active = SESSION.lock().unwrap().take();
    if let Some(a) = active {
        a.stop.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !a.thread.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A native file dialog for the image (the page cannot see a file's path).
#[tauri::command]
pub fn pick_iso() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Choose a CD/DVD image")
        .add_filter("Disc images", &["iso"])
        .add_filter("All files", &["*"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Start serving `iso` as the machine's CD/DVD drive. `start` is reboot, graceful or now.
#[tauri::command]
pub fn ider_mount(app: AppHandle, ip: String, user: String, pass: String, iso: String, start: String, remember: bool) {
    thread::spawn(move || {
        let _ = start_session(&app, &ip, &user, &pass, &iso, &start, remember);
    });
}

/// Open the IDE-R session and keep serving it from a background thread; returns once the device has
/// accepted it (Ok) or with why it did not. Progress goes to the page as "ider-status".
pub fn start_session(app: &AppHandle, ip: &str, user: &str, pass: &str, iso: &str, start: &str, remember: bool) -> Result<(), String> {
    let (app, ip, user, pass, iso, start) = (app.clone(), ip.to_string(), user.to_string(), pass.to_string(), iso.to_string(), start.to_string());
    {
        stop();
        let user = if user.is_empty() { "admin".to_string() } else { user };
        let name = Path::new(&iso).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| iso.clone());
        let start_mode = match start.as_str() {
            "graceful" => amt_ider::IderStart::Graceful,
            "now" => amt_ider::IderStart::Now,
            _ => amt_ider::IderStart::OnReboot,
        };
        emit(&app, false, format!("connecting to {ip} to serve {name}..."));
        let (rport, pin) = amt_target(&ip);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(amt_ider::IderStats::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let (app2, stop2, stats2, ip2, user2, pass2, iso2) = (app.clone(), stop_flag.clone(), stats.clone(), ip.clone(), user.clone(), pass.clone(), iso.clone());
        let done = Arc::new(AtomicBool::new(false));
        let done2 = done.clone();
        let handle = thread::spawn(move || {
            let r = amt_ider::run(&ip2, rport, &user2, &pass2, pin, None, Some(Path::new(&iso2)), start_mode, stop2.clone(), stats2, Some(tx));
            done2.store(true, Ordering::SeqCst);
            match r {
                // stopped on request: the unmount command says so itself
                Ok(()) => emit(&app2, false, "virtual media ended"),
                Err(e) => emit(&app2, false, format!("virtual media stopped: {e}{}", if e.contains("refused") { " (is IDE-R enabled? bobcat-cli --amt --enable ider)" } else { "" })),
            }
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                stop_flag.store(true, Ordering::SeqCst);
                return Err(e); // the session thread has reported it
            }
            Err(_) => {
                stop_flag.store(true, Ordering::SeqCst);
                emit(&app, false, "the device did not accept the virtual media session in 30 s");
                return Err("no answer from the device".into());
            }
        }
        if remember {
            keychain::remember(&user, &ip, &pass);
        }
        *SESSION.lock().unwrap() = Some(Active { stop: stop_flag.clone(), thread: handle });
        let when = match start.as_str() {
            "graceful" => "after a graceful shutdown",
            "now" => "now",
            _ => "at the next boot",
        };
        emit(&app, true, format!("{name} is served; the machine uses it {when}"));
        // keep the page informed while it runs
        let app3 = app.clone();
        thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) && !done.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(2));
                if stop_flag.load(Ordering::SeqCst) || done.load(Ordering::SeqCst) {
                    break;
                }
                emit_quiet(&app3, format!("{name} is served ({} MB read by the machine)", stats.image_bytes.load(Ordering::Relaxed) / 1_048_576));
            }
        });
    }
    Ok(())
}

/// A status update that is not worth a log line.
fn emit_quiet(app: &AppHandle, text: String) {
    let _ = app.emit("ider-status", IderStatus { mounted: true, text });
}

/// Stop serving the image.
#[tauri::command]
pub fn ider_unmount(app: AppHandle) {
    thread::spawn(move || {
        stop();
        emit(&app, false, "virtual media unmounted");
    });
}
