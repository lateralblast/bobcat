//! A local TLS relay in front of an iLO, for the macOS web view.
//!
//! WebKit on macOS refuses this iLO's certificate (a 1024-bit RSA key) itself: the navigation is
//! cancelled (`NSURLErrorCancelled`, -999) without the server-trust challenge ever reaching the
//! app, so the usual pinning delegate (`pin_webview_tls` in `html5.rs`) never gets to accept it.
//! Plain system networking does ask, and OpenSSL has no such limit, so the web view talks to this
//! listener instead: loopback only, HTTPS with a certificate generated here (2048-bit RSA,
//! accepted by the delegate because its fingerprint is registered with `is_ours`), and every
//! connection is relayed byte for byte to the iLO over the usual pinned connection
//! (`html5::tls_connect`). The page's own WebSocket (`wss://<its host>/wss/ircport`) goes through
//! the same relay. It must be HTTPS, not plain HTTP, because the iLO page hard-codes `wss://`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslMethod};
use openssl::x509::extension::SubjectAlternativeName;
use openssl::x509::{X509NameBuilder, X509};

/// Fingerprints (SHA-256, upper case with colons) of the certificates this process made.
static OURS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// (iLO address, local port) of the relays that are running.
static RUNNING: Mutex<Vec<(String, u16)>> = Mutex::new(Vec::new());

/// Is this one of our own relay certificates? (the web view's certificate check asks)
pub(crate) fn is_ours(fingerprint: Option<&String>) -> bool {
    fingerprint.is_some_and(|f| OURS.lock().unwrap().contains(f))
}

fn certificate() -> Result<(X509, PKey<openssl::pkey::Private>), String> {
    let e = |e: openssl::error::ErrorStack| e.to_string();
    let key = PKey::from_rsa(Rsa::generate(2048).map_err(e)?).map_err(e)?;
    let mut name = X509NameBuilder::new().map_err(e)?;
    name.append_entry_by_nid(openssl::nid::Nid::COMMONNAME, "127.0.0.1").map_err(e)?;
    let name = name.build();
    let mut b = X509::builder().map_err(e)?;
    b.set_version(2).map_err(e)?;
    let serial = BigNum::from_u32(1).and_then(|n| n.to_asn1_integer()).map_err(e)?;
    b.set_serial_number(&serial).map_err(e)?;
    b.set_subject_name(&name).map_err(e)?;
    b.set_issuer_name(&name).map_err(e)?;
    b.set_pubkey(&key).map_err(e)?;
    let (from, to) = (Asn1Time::days_from_now(0).map_err(e)?, Asn1Time::days_from_now(30).map_err(e)?);
    b.set_not_before(from.as_ref()).map_err(e)?;
    b.set_not_after(to.as_ref()).map_err(e)?;
    let san = SubjectAlternativeName::new().ip("127.0.0.1").build(&b.x509v3_context(None, None)).map_err(e)?;
    b.append_extension(san).map_err(e)?;
    b.sign(&key, MessageDigest::sha256()).map_err(e)?;
    Ok((b.build(), key))
}

/// Start (or reuse) the relay for this iLO and return its local port.
pub(crate) fn ensure(ip: &str) -> Result<u16, String> {
    if let Some((_, port)) = RUNNING.lock().unwrap().iter().find(|(i, _)| i == ip) {
        return Ok(*port);
    }
    let (cert, key) = certificate()?;
    let fp = cert
        .digest(MessageDigest::sha256())
        .map_err(|e| e.to_string())?
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    OURS.lock().unwrap().push(fp);
    let mut acc = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).map_err(|e| e.to_string())?;
    acc.set_private_key(&key).map_err(|e| e.to_string())?;
    acc.set_certificate(&cert).map_err(|e| e.to_string())?;
    let acceptor = Arc::new(acc.build());
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    RUNNING.lock().unwrap().push((ip.to_string(), port));
    let host = ip.to_string();
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (acceptor, host) = (acceptor.clone(), host.clone());
            thread::spawn(move || {
                if let Err(e) = relay(&acceptor, conn, &host) {
                    if crate::html5::debug_on() {
                        eprintln!("[bobcat] ilo relay: {e}");
                    }
                }
            });
        }
    });
    if crate::html5::debug_on() {
        eprintln!("[bobcat] ilo relay: 127.0.0.1:{port} -> {ip}:443");
    }
    Ok(port)
}

/// One browser connection: TLS from the web view, a pinned TLS connection to the iLO, bytes both ways.
fn relay(acceptor: &SslAcceptor, conn: TcpStream, ip: &str) -> Result<(), String> {
    conn.set_nodelay(true).ok();
    let mut client = acceptor.accept(conn).map_err(|e| format!("web view handshake: {e}"))?;
    let mut upstream = crate::html5::tls_connect(ip)?;
    upstream.get_ref().set_nodelay(true).ok();
    // openssl streams cannot be split: one thread owns both and polls them with short read timeouts
    client.get_ref().set_read_timeout(Some(Duration::from_millis(5))).ok();
    upstream.get_ref().set_read_timeout(Some(Duration::from_millis(5))).ok();
    let mut buf = vec![0u8; 64 * 1024];
    let debug = crate::html5::debug_on();
    let (mut up_bytes, mut down_bytes) = (0usize, 0usize);
    let mut log = Log::default();
    let waiting = |e: &std::io::Error| matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut);
    loop {
        let mut idle = true;
        match upstream.read(&mut buf) {
            Ok(0) => return done(debug, up_bytes, down_bytes),
            Ok(n) => {
                if debug {
                    log.note("iLO -> web view", &buf[..n]);
                }
                down_bytes += n;
                client.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                idle = false;
            }
            Err(e) if waiting(&e) => {}
            Err(e) => return Err(format!("iLO: {e}")),
        }
        match client.read(&mut buf) {
            Ok(0) => return done(debug, up_bytes, down_bytes),
            Ok(n) => {
                if debug {
                    log.note("web view -> iLO", &buf[..n]);
                }
                up_bytes += n;
                upstream.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                upstream.flush().ok();
                idle = false;
            }
            Err(e) if waiting(&e) => {}
            Err(e) => return Err(format!("web view: {e}")),
        }
        if idle {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn done(debug: bool, up: usize, down: usize) -> Result<(), String> {
    if debug {
        eprintln!("[bobcat] ilo relay: connection closed ({up} bytes to the iLO, {down} back)");
    }
    Ok(())
}

/// Debug output for one connection: HTTP request lines and status lines, the headers of a
/// WebSocket upgrade (Host, Origin, subprotocol; never the cookie, it holds the session key) and,
/// after an upgrade, what the web view sends (key and mouse events are small frames) and a count
/// of what comes back. Header lines the iLO sends in pieces are not shown.
#[derive(Default)]
struct Log {
    upgraded: bool,
    /// the empty line that ends the 101 answer has gone by: what follows is WebSocket frames
    frames_started: bool,
    sent: usize,
    mouse: usize,
    received: usize,
}

impl Log {
    fn note(&mut self, dir: &str, data: &[u8]) {
        let to_ilo = dir.starts_with("web");
        if self.upgraded && !self.frames_started && !to_ilo {
            // the iLO's headers arrive one line per chunk; the empty line ends them
            if data == b"\r\n" {
                self.frames_started = true;
            }
            return;
        }
        if self.frames_started {
            if to_ilo {
                self.sent += 1;
                // a browser's frames are masked: 2 header bytes, a 4 byte mask, the payload. The iLO's
                // messages start with a type: 1 = keyboard (type, 0, 8 bytes of USB HID report), 2 = mouse
                let payload: Vec<u8> = if data.len() > 6 && data[1] & 0x80 != 0 && (data[1] & 0x7f) as usize == data.len() - 6 {
                    data[6..].iter().enumerate().map(|(i, b)| b ^ data[2 + i % 4]).collect()
                } else {
                    Vec::new()
                };
                match payload.first() {
                    Some(2) => self.mouse += 1,
                    Some(1) => eprintln!("[bobcat] ilo relay: keyboard frame to the iLO: {:02x?}", payload),
                    _ if self.sent <= 200 => eprintln!("[bobcat] ilo relay: frame {} web view -> iLO: {} bytes (type {:?})", self.sent, data.len(), payload.first()),
                    _ => {}
                }
            } else {
                self.received += 1;
                if self.received <= 3 || self.received % 200 == 0 {
                    eprintln!("[bobcat] ilo relay: frame {} iLO -> web view: {} bytes (first {:02x?})", self.received, data.len(), &data[..data.len().min(12)]);
                }
            }
            return;
        }
        let head = String::from_utf8_lossy(&data[..data.len().min(1200)]);
        let first = head.lines().next().unwrap_or("");
        let request = ["GET ", "POST ", "PUT ", "DELETE ", "HEAD ", "OPTIONS "].iter().any(|m| first.starts_with(m));
        let status = first.starts_with("HTTP/1.");
        if !request && !status {
            return;
        }
        // the query may hold the session key: show it only for our own diagnostic request
        let line = if first.contains("/__bobcat") { first.to_string() } else { first.split('?').next().unwrap_or(first).to_string() };
        eprintln!("[bobcat] ilo relay: {dir}: {line}");
        if head.to_ascii_lowercase().contains("upgrade: websocket") || first.contains(" 101 ") {
            self.upgraded = true;
            for l in head.lines().skip(1) {
                let k = l.split(':').next().unwrap_or("").to_ascii_lowercase();
                if ["host", "origin", "sec-websocket-protocol", "sec-websocket-version"].contains(&k.as_str()) {
                    eprintln!("[bobcat] ilo relay:     {l}");
                }
            }
        }
    }
}
