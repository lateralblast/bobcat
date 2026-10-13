// iDRAC passwords in the OS credential store (macOS Keychain, Windows
// Credential Manager, Linux Secret Service). Login passwords: one entry per
// user@ip (service "bobcat"). An empty user means the iDRAC's VNC password, kept
// apart under the service "bobcat-vnc" (account: the ip). bobcat shares both.

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
            eprintln!("warning: could not read the keychain: {e}");
            None
        }
    }
}

pub fn save(user: &str, ip: &str, pass: &str) -> Result<(), String> {
    entry(user, ip)?.set_password(pass).map_err(|e| e.to_string())
}

/// Remove the saved password; Ok(false) if there was none.
pub fn forget(user: &str, ip: &str) -> Result<bool, String> {
    match entry(user, ip)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(Error::NoEntry) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
