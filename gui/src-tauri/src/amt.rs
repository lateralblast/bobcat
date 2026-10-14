// Helpers for amt_redir.rs, which is a copy of the bobcat-cli file of the same name (the crates
// share no code). The CLI's amt.rs (WS-Management client) is not needed here.
use openssl::hash::{hash, MessageDigest};

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn md5_hex(s: &str) -> String {
    hex(&hash(MessageDigest::md5(), s.as_bytes()).expect("md5"))
}
