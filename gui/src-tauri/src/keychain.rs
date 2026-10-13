// iDRAC login passwords in the OS credential store (macOS Keychain, Windows
// Credential Manager, Linux Secret Service). One entry per user@ip, the same
// entries bobcat-cli uses (service "bobcat"). An empty user means the iDRAC's
// VNC password, kept apart under the service "bobcat-vnc" (account: the ip).

use keyring::{Entry, Error};

fn entry(user: &str, ip: &str) -> Result<Entry, String> {
    if user.is_empty() {
        Entry::new("bobcat-vnc", ip)
    } else {
        Entry::new("bobcat", &format!("{user}@{ip}"))
    }
    .map_err(|e| e.to_string())
}

/// The saved password, or None if there is none or the store is unavailable.
pub fn load(user: &str, ip: &str) -> Option<String> {
    match entry(user, ip).ok()?.get_password() {
        Ok(p) => Some(p),
        Err(Error::NoEntry) => None,
        Err(e) => {
            eprintln!("[bobcat-gui] could not read the keychain: {e}");
            None
        }
    }
}

/// Save a password that has just been accepted by the iDRAC.
pub fn remember(user: &str, ip: &str, pass: &str) {
    if pass.is_empty() {
        return;
    }
    if let Err(e) = entry(user, ip).and_then(|en| en.set_password(pass).map_err(|e| e.to_string())) {
        eprintln!("[bobcat-gui] could not save to the keychain: {e}");
    }
}

pub fn forget(user: &str, ip: &str) {
    if let Ok(en) = entry(user, ip) {
        let _ = en.delete_credential(); // NoEntry is fine
    }
}
