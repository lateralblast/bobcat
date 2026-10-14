// Intel AMT power, boot and redirection controls for the desktop app, over WS-Management (amt.rs, a
// copy of the bobcat-cli file). The commands return at once; results reach the page as "amt-note"
// events (a line of text and whether it went well).

use crate::{amt, amt_media, amt_redir, check_pin};
use serde::Serialize;
use std::thread;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
struct Note {
    text: String,
    ok: bool,
}

fn note(app: &AppHandle, ok: bool, text: impl Into<String>) {
    let text = text.into();
    eprintln!("[bobcat] AMT: {text}");
    let _ = app.emit("amt-note", Note { text, ok });
}

/// A WS-Management client for the device: TLS (16993) when the device answers there, else plain (16992).
fn device(ip: &str, user: &str, pass: &str) -> amt::Amt {
    let user = if user.is_empty() { "admin" } else { user };
    if amt_redir::tls_available(ip) {
        let host = ip.to_string();
        let pin: amt_redir::Pin = Box::new(move |t| check_pin(&host, t));
        amt::Amt::new(ip, 16993, user, pass, Some(pin))
    } else {
        amt::Amt::new(ip, 16992, user, pass, None)
    }
}

/// Run `f` on a background thread and report a failure as a note.
fn spawn(app: AppHandle, f: impl FnOnce(&AppHandle) -> Result<(), String> + Send + 'static) {
    thread::spawn(move || {
        if let Err(e) = f(&app) {
            note(&app, false, e);
        }
    });
}

/// Power state and which redirections are on.
#[tauri::command]
pub fn amt_status(app: AppHandle, ip: String, user: String, pass: String) {
    spawn(app, move |app| {
        let d = device(&ip, &user, &pass);
        let power = amt::power_name(d.power_state()?);
        let (sol, ider) = d.redirection()?;
        note(app, true, format!("power: {power}; Serial over LAN {}; IDE-R {}", if sol { "on" } else { "off" }, if ider { "on" } else { "off" }));
        Ok(())
    });
}

/// A power action: on, off (soft), off-hard, cycle or reset.
#[tauri::command]
pub fn amt_power(app: AppHandle, ip: String, user: String, pass: String, action: String) {
    spawn(app, move |app| {
        let state = match action.as_str() {
            "on" => 2,
            "off" => 8,
            "off-hard" => 6,
            "cycle" => 5,
            "reset" => 10,
            other => return Err(format!("unknown power action {other}")),
        };
        let d = device(&ip, &user, &pass);
        d.set_power(state)?;
        note(app, true, format!("power {action} requested; the machine is now {}", amt::power_name(d.power_state()?)));
        Ok(())
    });
}

fn boot_source(what: &str) -> Result<amt::BootSource, String> {
    Ok(match what {
        "hdd" => amt::BootSource::Hdd,
        "pxe" => amt::BootSource::Pxe,
        "cd" => amt::BootSource::Cd,
        "diag" => amt::BootSource::Diag,
        "bios" => amt::BootSource::Bios,
        "ider-cd" => amt::BootSource::IderCd,
        "normal" => amt::BootSource::Normal,
        other => return Err(format!("unknown boot choice {other}")),
    })
}

/// What the next start does (it does not power anything).
#[tauri::command]
pub fn amt_boot(app: AppHandle, ip: String, user: String, pass: String, what: String, sol: bool) {
    spawn(app, move |app| {
        device(&ip, &user, &pass).set_boot(boot_source(&what)?, sol)?;
        note(app, true, format!("next start: {what}; it applies at the next power-on or reset"));
        Ok(())
    });
}

/// Switch IDE-R on (Serial over LAN stays as it is).
#[tauri::command]
pub fn amt_ider_enable(app: AppHandle, ip: String, user: String, pass: String) {
    spawn(app, move |app| {
        let d = device(&ip, &user, &pass);
        let (sol, _) = d.redirection()?;
        d.set_redirection(sol, true)?;
        note(app, true, "IDE-R is on");
        Ok(())
    });
}

/// Mount the ISO, boot from it and start (or reset) the machine: IDE-R on if it is off, the session
/// opened first, the boot settings, then a reset (or a power-on when the machine is off).
#[tauri::command]
pub fn amt_boot_iso(app: AppHandle, ip: String, user: String, pass: String, iso: String, sol: bool) {
    spawn(app, move |app| {
        let d = device(&ip, &user, &pass);
        let (s, i) = d.redirection()?;
        if !i {
            note(app, true, "switching IDE-R on...");
            d.set_redirection(s, true).map_err(|e| format!("switching IDE-R on: {e}"))?;
        }
        note(app, true, "opening the virtual media session...");
        amt_media::start_session(app, &ip, &user, &pass, &iso, "reboot", false).map_err(|e| format!("virtual media: {e}"))?;
        note(app, true, "setting the boot source...");
        d.set_boot(amt::BootSource::IderCd, sol)?;
        let on = d.power_state()? == 2;
        d.set_power(if on { 10 } else { 2 })?;
        note(app, true, format!("{}: the machine boots from the ISO; keep this window open while it does", if on { "reset requested" } else { "power on requested" }));
        Ok(())
    });
}
