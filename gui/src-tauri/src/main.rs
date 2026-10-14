// bobcat - iDRAC VNC console viewer (Tauri 2)
// Version: 1.1.9
// License: CC BY-NC-SA 4.0 (see LICENSE.md)

mod amt;
#[allow(dead_code)]
mod amt_redir;
mod html5;
mod html5_tools;
mod java_console;
#[cfg(target_os = "macos")]
mod java_dock;
#[cfg(target_os = "linux")]
mod java_x11;
mod keychain;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use vnc::client::{AuthChoice, AuthMethod};
use vnc::{Client, Encoding, PixelFormat, Rect};

/// Details of the TLS certificate most recently presented by the iDRAC.
#[derive(Serialize, Clone)]
struct CertInfo {
    /// "tls" (VNC certificate) or "ssh" (SSH host key)
    kind: String,
    /// key under which the fingerprint is saved in known_certs.json
    pin_key: String,
    /// SSH host key type (empty for TLS)
    key_type: String,
    host: String,
    subject: String,
    issuer: String,
    not_before: String,
    not_after: String,
    sha256: String,
    /// "new" (saved on first sight), "pinned" (matches the saved one) or "changed"
    state: String,
    pinned_sha256: Option<String>,
}

static LAST_CERT: Mutex<Option<CertInfo>> = Mutex::new(None);
/// The running text-session thread, so we can wait for it to hang up cleanly.
static TEXT_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

/// Wait (up to `secs`) for the current text session to finish hanging up.
fn wait_text_session(secs: u64) {
    let handle = TEXT_THREAD.lock().unwrap().take();
    if let Some(h) = handle {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while !h.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        if h.is_finished() {
            let _ = h.join();
        }
    }
}

/// Hang up any session cleanly (used on window close and on Ctrl+C / SIGTERM).
fn shutdown(app: &AppHandle) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<Session>() {
        if let Some(tx) = state.0.lock().unwrap().take() {
            let _ = tx.send(Cmd::Stop);
        }
    }
    html5::close_window(app);
    java_console::stop();
    html5::logout_all();
    wait_text_session(8);
}

static LAST_HOSTKEY: Mutex<Option<CertInfo>> = Mutex::new(None);
/// Why the TLS forwarder gave up, so run() can report it instead of a bare reset.
static FWD_ERR: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn pins_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("bobcat").join("known_certs.json")
}

/// Where dracon-gui (the old name of this program) kept the pins; still read if the new file is missing.
fn legacy_pins_path() -> std::path::PathBuf {
    let mut p = pins_path();
    p.pop();
    p.pop();
    p.join("dracon-gui").join("known_certs.json")
}

pub(crate) fn load_pins() -> HashMap<String, String> {
    // first run under the new name: copy the old file across, then use the new one
    if !pins_path().exists() && legacy_pins_path().exists() {
        if let Some(dir) = pins_path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::copy(legacy_pins_path(), pins_path());
    }
    let path = if pins_path().exists() { pins_path() } else { legacy_pins_path() };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_pin(host: &str, fp: &str) -> Result<(), String> {
    let mut pins = load_pins();
    pins.insert(host.to_string(), fp.to_string());
    let path = pins_path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_string_pretty(&pins).unwrap()).map_err(|e| e.to_string())
}

fn name_string(n: &openssl::x509::X509NameRef) -> String {
    n.entries()
        .map(|e| {
            format!(
                "{}={}",
                e.object().nid().short_name().unwrap_or("?"),
                e.data().to_string().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One long-lived clipboard handle. On Linux the process owns the clipboard
/// contents, so dropping the handle right after a write can lose them.
static CLIP: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub(crate) fn with_clip<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>) -> Result<T, String> {
    let mut g = CLIP.lock().map_err(|e| e.to_string())?;
    if g.is_none() {
        *g = Some(arboard::Clipboard::new().map_err(|e| format!("clipboard: {e}"))?);
    }
    f(g.as_mut().unwrap()).map_err(|e| format!("clipboard: {e}"))
}

#[derive(Default)]
struct Session(Mutex<Option<Sender<Cmd>>>);

enum Cmd {
    Stop,
    Key { down: bool, keysym: u32 },
    Pointer { buttons: u8, x: u16, y: u16 },
    /// Keyboard bytes for a text console (SSH / SOL)
    Input(Vec<u8>),
    Resize(u16, u16),
}

#[derive(Serialize, Clone)]
struct Frame {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    /// base64 RGBA pixels
    data: String,
}

#[derive(Serialize, Clone)]
struct Size {
    w: u16,
    h: u16,
}

pub(crate) fn status(app: &AppHandle, msg: impl Into<String>) {
    let msg = msg.into();
    eprintln!("[bobcat] {msg}");
    let _ = app.emit("vnc-status", msg);
}

/// The iDRAC's encrypted VNC mode is TLS wrapped around the whole connection
/// (TLS 1.2, static-RSA AES256-GCM, self-signed certificate, ~10s handshake)
/// with plain RFB inside. The `vnc` crate only speaks plain TCP, so run the
/// TLS leg here and hand it a local plain socket to connect to.
/// The certificate is NOT verified (self-signed); see ISSUES.md.
/// Trust on first use: save the certificate's fingerprint the first time a
/// host is seen, silently; refuse to connect if it later differs.
pub(crate) fn check_pin(host: &str, tls: &openssl::ssl::SslStream<TcpStream>) -> Result<(), String> {
    let cert = tls.ssl().peer_certificate().ok_or("the iDRAC presented no certificate")?;
    let fp = cert
        .digest(openssl::hash::MessageDigest::sha256())
        .map_err(|e| e.to_string())?
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let pinned = load_pins().get(host).cloned();
    let state = match &pinned {
        None => {
            save_pin(host, &fp)?;
            "new"
        }
        Some(p) if *p == fp => "pinned",
        Some(_) => "changed",
    };
    *LAST_CERT.lock().unwrap() = Some(CertInfo {
        kind: "tls".into(),
        pin_key: host.to_string(),
        key_type: String::new(),
        host: host.to_string(),
        subject: name_string(cert.subject_name()),
        issuer: name_string(cert.issuer_name()),
        not_before: cert.not_before().to_string(),
        not_after: cert.not_after().to_string(),
        sha256: fp,
        state: state.into(),
        pinned_sha256: pinned.clone(),
    });
    if state == "changed" {
        return Err(format!(
            "WARNING: the iDRAC certificate for {host} has changed since it was first saved; connection blocked. \
             If you expected this (e.g. the certificate was replaced), open Certificate and choose Trust this certificate."
        ));
    }
    Ok(())
}

/// Trust on first use for the iDRAC's SSH host key; must run before any
/// password is sent. Returns an error (starting "WARNING") if the key changed.
fn check_hostkey(host: &str, sess: &ssh2::Session) -> Result<(), String> {
    let fp = sess
        .host_key_hash(ssh2::HashType::Sha256)
        .ok_or("the iDRAC presented no SSH host key")?
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let key_type = sess.host_key().map(|(_, t)| format!("{t:?}")).unwrap_or_default();
    let pin_key = format!("ssh:{host}");
    let pinned = load_pins().get(&pin_key).cloned();
    let state = match &pinned {
        None => {
            save_pin(&pin_key, &fp)?;
            "new"
        }
        Some(p) if *p == fp => "pinned",
        Some(_) => "changed",
    };
    *LAST_HOSTKEY.lock().unwrap() = Some(CertInfo {
        kind: "ssh".into(),
        pin_key,
        key_type,
        host: host.to_string(),
        subject: String::new(),
        issuer: String::new(),
        not_before: String::new(),
        not_after: String::new(),
        sha256: fp,
        state: state.into(),
        pinned_sha256: pinned,
    });
    if state == "changed" {
        return Err(format!(
            "WARNING: the SSH host key for {host} has changed since it was first saved; connection blocked. \
             If you expected this, open Host key and choose Trust this key."
        ));
    }
    Ok(())
}

fn tls_forwarder(ip: &str, port: u16) -> Result<u16, String> {
    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};

    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    let addr = (ip, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("bad address")?;
    let host = ip.to_string();
    *FWD_ERR.lock().unwrap() = None;
    thread::spawn(move || {
        let run = || -> Result<(), String> {
            let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
                .map_err(|e| format!("connect failed: {e}"))?;
            let mut b = SslConnector::builder(SslMethod::tls()).map_err(|e| e.to_string())?;
            b.set_verify(SslVerifyMode::NONE);
            b.set_max_proto_version(Some(SslVersion::TLS1_2)).map_err(|e| e.to_string())?;
            b.set_cipher_list("ALL:@SECLEVEL=0").map_err(|e| e.to_string())?;
            let conn = b.build();
            let mut cfg = conn.configure().map_err(|e| e.to_string())?;
            cfg.set_verify_hostname(false);
            cfg.set_use_server_name_indication(false);
            // the iDRAC takes ~10s to answer the ClientHello
            tcp.set_read_timeout(Some(Duration::from_secs(60))).ok();
            let tls = cfg.connect("idrac", tcp).map_err(|e| format!("tls handshake: {e}"))?;
            check_pin(&host, &tls)?;
            let (local_conn, _) = listener.accept().map_err(|e| e.to_string())?;

            // openssl streams aren't splittable: share one behind a lock and
            // poll with short read timeouts so both directions make progress.
            let tcp = tls.get_ref().try_clone().map_err(|e| e.to_string())?;
            tcp.set_read_timeout(Some(Duration::from_millis(5))).ok();
            let tls = std::sync::Arc::new(Mutex::new(tls));
            let mut local_rd = local_conn.try_clone().map_err(|e| e.to_string())?;
            let mut local_wr = local_conn;
            local_rd.set_read_timeout(Some(Duration::from_millis(5))).ok();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut idle = true;
                // iDRAC -> viewer
                let n = { tls.lock().unwrap().read(&mut buf) };
                match n {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        local_wr.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                        idle = false;
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(e) => return Err(e.to_string()),
                }
                // viewer -> iDRAC
                match local_rd.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        let mut t = tls.lock().unwrap();
                        t.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                        t.flush().ok();
                        idle = false;
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(e) => return Err(e.to_string()),
                }
                if idle {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        };
        match run() {
            Ok(()) => eprintln!("[bobcat] tls forwarder: connection closed"),
            Err(e) => {
                eprintln!("[bobcat] tls forwarder: {e}");
                *FWD_ERR.lock().unwrap() = Some(e);
            }
        }
    });
    Ok(local)
}

/// Zrle (compressed) first, Raw as fallback. BOBCAT_ENCODING=raw forces Raw.
/// CopyRect is not requested: we don't apply it locally.
fn encodings() -> Vec<Encoding> {
    match std::env::var("BOBCAT_ENCODING").as_deref() {
        Ok("raw") => vec![Encoding::Raw],
        _ => vec![Encoding::Zrle, Encoding::Raw],
    }
}

/// Convert server pixels (true colour, 16 or 32 bpp) to RGBA8.
fn to_rgba(f: &PixelFormat, px: &[u8]) -> Vec<u8> {
    let bpp = f.bits_per_pixel as usize / 8;
    let scale = |v: u32, max: u16| -> u8 { ((v * 255 + max as u32 / 2) / max.max(1) as u32) as u8 };
    let mut out = Vec::with_capacity(px.len() / bpp * 4);
    for p in px.chunks_exact(bpp) {
        let v = match (bpp, f.big_endian) {
            (4, false) => u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
            (4, true) => u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
            (_, false) => u16::from_le_bytes([p[0], p[1]]) as u32,
            (_, true) => u16::from_be_bytes([p[0], p[1]]) as u32,
        };
        let r = (v >> f.red_shift) & f.red_max as u32;
        let g = (v >> f.green_shift) & f.green_max as u32;
        let b = (v >> f.blue_shift) & f.blue_max as u32;
        out.extend_from_slice(&[scale(r, f.red_max), scale(g, f.green_max), scale(b, f.blue_max), 255]);
    }
    out
}

fn run(app: &AppHandle, ip: &str, port: u16, tls: bool, pass: &str, remember: bool, cmds: &std::sync::mpsc::Receiver<Cmd>) -> Result<(), String> {
    let real_ip = ip;
    let (ip, port) = if tls {
        status(app, "negotiating TLS (the iDRAC takes ~10-15s)...");
        ("127.0.0.1".to_string(), tls_forwarder(ip, port)?)
    } else {
        (ip.to_string(), port)
    };
    let ip = ip.as_str();
    let addr = (ip, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("bad address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("connect failed: {e}"))?;

    let mut key = [0u8; 8];
    for (i, b) in pass.bytes().take(8).enumerate() {
        key[i] = b;
    }
    let mut client = Client::from_tcp_stream(tcp, true, |methods| {
        methods.iter().find_map(|m| match m {
            AuthMethod::Password => Some(AuthChoice::Password(key)),
            AuthMethod::None => Some(AuthChoice::None), // the AMT bridge (login already done)
            _ => None,
        })
    })
    .map_err(|e| {
        thread::sleep(Duration::from_millis(300)); // let the forwarder record why it quit
        FWD_ERR.lock().unwrap().take().unwrap_or_else(|| format!("vnc handshake: {e}"))
    })?;

    // the iDRAC accepted the VNC password
    if remember {
        keychain::remember("", real_ip, pass);
    }

    // Force a known 32-bit little-endian layout (bytes B,G,R,X). The iDRAC's
    // native 16-bit format gave wrong colours, so don't trust it.
    // Client::set_format waits for ONE full-screen rectangle, which only
    // arrives untiled with Raw, so switch format before enabling Zrle.
    client
        .set_format(PixelFormat {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        })
        .map_err(|e| e.to_string())?;
    client.set_encodings(&encodings()).map_err(|e| e.to_string())?;
    let fmt = client.format();
    if std::env::var("BOBCAT_DEBUG").is_ok() {
        eprintln!("[bobcat] pixel format: {fmt:?}");
    }

    let (mut w, mut h) = client.size();
    status(app, format!("connected: {} ({w}x{h})", client.name()));
    let _ = app.emit("vnc-resize", Size { w, h });
    let full = |w, h| Rect { left: 0, top: 0, width: w, height: h };
    client.request_update(full(w, h), false).map_err(|e| e.to_string())?;

    let b64 = base64::engine::general_purpose::STANDARD;
    let started = std::time::Instant::now();
    let (mut rects, mut pixels_total) = (0u64, 0u64);
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stop => {
                    let _ = client.disconnect();
                    return Ok(());
                }
                Cmd::Key { down, keysym } => {
                    client.send_key_event(down, keysym).map_err(|e| e.to_string())?
                }
                Cmd::Pointer { buttons, x, y } => {
                    client.send_pointer_event(buttons, x, y).map_err(|e| e.to_string())?
                }
                Cmd::Input(_) | Cmd::Resize(..) => {}
            }
        }
        match client.poll_event() {
            None => thread::sleep(Duration::from_millis(5)),
            Some(vnc::client::Event::PutPixels(r, px)) => {
                rects += 1;
                pixels_total += r.width as u64 * r.height as u64;
                if std::env::var("BOBCAT_DEBUG").is_ok() && (rects <= 3 || rects % 200 == 0) {
                    eprintln!(
                        "[bobcat] +{:.1}s rect #{rects} {},{} {}x{} ({} Mpx total)",
                        started.elapsed().as_secs_f32(), r.left, r.top, r.width, r.height, pixels_total / 1_000_000
                    );
                }
                let rgba = to_rgba(&fmt, &px);
                let _ = app.emit(
                    "vnc-frame",
                    Frame { x: r.left, y: r.top, w: r.width, h: r.height, data: b64.encode(rgba) },
                );
            }
            Some(vnc::client::Event::CopyPixels { .. }) => {
                // Not applied locally; ask for a full refresh instead.
                client.request_update(full(w, h), false).map_err(|e| e.to_string())?;
            }
            Some(vnc::client::Event::Resize(nw, nh)) => {
                w = nw;
                h = nh;
                let _ = app.emit("vnc-resize", Size { w, h });
                client.request_update(full(w, h), false).map_err(|e| e.to_string())?;
            }
            Some(vnc::client::Event::EndOfFrame) => {
                client.request_update(full(w, h), true).map_err(|e| e.to_string())?;
            }
            Some(vnc::client::Event::Disconnected(e)) => {
                return Err(match e {
                    Some(e) => format!("disconnected: {e}"),
                    None => "disconnected".into(),
                });
            }
            Some(vnc::client::Event::Clipboard(text)) => {
                // Server-side clipboard (only sent by servers with a guest agent).
                if with_clip(|c| c.set_text(text)).is_ok() {
                    status(app, "clipboard updated from server");
                }
            }
            Some(_) => {}
        }
    }
}

fn cert_slot(kind: &Option<String>) -> &'static Mutex<Option<CertInfo>> {
    if kind.as_deref() == Some("ssh") { &LAST_HOSTKEY } else { &LAST_CERT }
}

/// The certificate (kind "tls") or SSH host key (kind "ssh") last presented.
#[tauri::command]
fn view_cert(kind: Option<String>) -> Result<CertInfo, String> {
    cert_slot(&kind).lock().unwrap().clone().ok_or_else(|| {
        if kind.as_deref() == Some("ssh") {
            "no SSH host key seen yet; connect with SSH console first".to_string()
        } else {
            "no certificate seen yet; connect with TLS first".to_string()
        }
    })
}

/// Replace the saved fingerprint with the one most recently presented.
#[tauri::command]
fn trust_cert(kind: Option<String>) -> Result<(), String> {
    let mut g = cert_slot(&kind).lock().unwrap();
    let c = g.as_mut().ok_or("nothing seen yet")?;
    save_pin(&c.pin_key, &c.sha256)?;
    c.state = "pinned".into();
    c.pinned_sha256 = Some(c.sha256.clone());
    Ok(())
}

#[tauri::command]
fn default_password(protocol: Option<String>) -> String {
    let var = match protocol.as_deref() {
        Some("ssh") | Some("sol") | Some("html5") | Some("java") | Some("amt") | Some("amtsol") => "BOBCAT_PASS",
        _ => "BOBCAT_VNC_PASS",
    };
    std::env::var(var).unwrap_or_default()
}

/// Set BOBCAT_TLS=1 to start with the TLS option ticked.
#[tauri::command]
fn default_tls() -> bool {
    std::env::var("BOBCAT_TLS").is_ok()
}

/// Set BOBCAT_AUTOCONNECT=1 to connect on startup (needs BOBCAT_VNC_PASS).
#[tauri::command]
fn autoconnect() -> bool {
    std::env::var("BOBCAT_AUTOCONNECT").is_ok()
}

/// BOBCAT_DEBUG=1 logs byte counts; BOBCAT_DEBUG=2 also the bytes (escaped).
fn debug_bytes(dir: &str, bytes: &[u8]) {
    match std::env::var("BOBCAT_DEBUG").as_deref() {
        Ok("2") => eprintln!("[bobcat] {dir} {} bytes: {}", bytes.len(), bytes.escape_ascii()),
        Ok(_) => eprintln!("[bobcat] {dir} {} bytes", bytes.len()),
        Err(_) => {}
    }
}

/// End an SSH console session cleanly: send the iDRAC's quit key (Ctrl+\\) so it
/// releases the serial device, wait briefly for the server to finish, then close
/// the channel and say goodbye. Dropping the connection instead can leave the
/// console "in use" on the iDRAC until it times out.
fn ssh_goodbye(sess: &ssh2::Session, ch: &mut ssh2::Channel, send_quit: bool) {
    sess.set_blocking(false);
    if send_quit && !ch.eof() {
        let _ = ch.write(b"\x1c");
        let mut buf = [0u8; 1024];
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline && !ch.eof() {
            match ch.read(&mut buf) {
                Ok(n) if n > 0 => {}
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
    }
    sess.set_blocking(true);
    let _ = ch.close();
    let _ = ch.wait_close();
    let _ = sess.disconnect(Some(ssh2::DisconnectCode::ByApplication), "bobcat closing", None);
}

fn emit_term(app: &AppHandle, bytes: &[u8]) {
    debug_bytes("term-data", bytes);
    let _ = app.emit("term-data", base64::engine::general_purpose::STANDARD.encode(bytes));
}

/// Text console through the iDRAC's SSH shell (`console com2`). Credentials are
/// handled in-process by ssh2. NOTE: the SSH host key is not verified (ISSUES.md).
fn text_ssh(app: &AppHandle, ip: &str, user: &str, pass: &str, remember: bool, cols: u16, rows: u16, cmds: &std::sync::mpsc::Receiver<Cmd>) -> Result<(), String> {
    let addr = (ip, 22)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("bad address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("connect failed: {e}"))?;
    let mut sess = ssh2::Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|e| format!("ssh handshake: {e}"))?;
    check_hostkey(ip, &sess)?; // before the password is sent
    sess.userauth_password(user, pass)
        .map_err(|e| format!("authentication failed: {e}"))?;
    if remember {
        keychain::remember(user, ip, pass);
    }
    let mut ch = sess.channel_session().map_err(|e| e.to_string())?;
    ch.request_pty("xterm-256color", None, Some((cols as u32, rows as u32, 0, 0)))
        .map_err(|e| e.to_string())?;
    ch.exec("console com2").map_err(|e| e.to_string())?;
    status(app, format!("connected: serial console of {ip} (SSH). Ctrl+\\ ends the session"));
    sess.set_blocking(false);
    let mut buf = vec![0u8; 8192];
    loop {
        match ch.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => emit_term(app, &buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        if ch.eof() {
            break;
        }
        let mut idle = true;
        while let Ok(cmd) = cmds.try_recv() {
            idle = false;
            match cmd {
                Cmd::Stop => {
                    ssh_goodbye(&sess, &mut ch, true);
                    return Ok(());
                }
                Cmd::Input(bytes) => {
                    debug_bytes("term-input", &bytes);
                    let mut off = 0;
                    while off < bytes.len() {
                        match ch.write(&bytes[off..]) {
                            Ok(w) => off += w,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(1))
                            }
                            Err(_) => return Ok(()),
                        }
                    }
                }
                Cmd::Resize(c, r) => {
                    sess.set_blocking(true);
                    let _ = ch.request_pty_size(c as u32, r as u32, None, None);
                    sess.set_blocking(false);
                }
                _ => {}
            }
        }
        if idle {
            thread::sleep(Duration::from_millis(5));
        }
    }
    ssh_goodbye(&sess, &mut ch, false);
    Ok(())
}

/// Intel AMT Serial over LAN through the redirection port (ipmitool cannot talk to AMT's SOL).
fn text_amtsol(app: &AppHandle, ip: &str, user: &str, pass: &str, remember: bool, cmds: &std::sync::mpsc::Receiver<Cmd>) -> Result<(), String> {
    let user = if user.is_empty() { "admin" } else { user };
    let (writer, mut reader) = amt_redir::sol_open(ip, 16994, user, pass)?;
    if remember {
        keychain::remember(user, ip, pass);
    }
    status(app, format!("connected to the AMT serial console of {ip}"));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (app, done) = (app.clone(), done.clone());
        thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                match reader.next() {
                    Ok(Some(d)) => emit_term(&app, &d),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let result = loop {
        if done.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stop => {
                    done.store(true, std::sync::atomic::Ordering::SeqCst);
                    writer.close();
                    return Ok(());
                }
                Cmd::Input(bytes) => {
                    debug_bytes("term-input", &bytes);
                    if writer.send(&bytes).is_err() {
                        done.store(true, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    writer.close();
    result
}

/// Text console through IPMI Serial over LAN, by running `ipmitool sol activate`
/// in a pseudo-terminal. The password goes in the child's environment, never
/// on a command line.
fn text_sol(app: &AppHandle, ip: &str, user: &str, pass: &str, remember: bool, cols: u16, rows: u16, cmds: &std::sync::mpsc::Receiver<Cmd>) -> Result<(), String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let size = |c, r| PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 };
    let pty = native_pty_system().openpty(size(cols, rows)).map_err(|e| e.to_string())?;
    let mut cmd = CommandBuilder::new("ipmitool");
    cmd.args(["-I", "lanplus", "-H", ip, "-U", user, "-E", "sol", "activate"]);
    cmd.env("IPMI_PASSWORD", pass);
    let mut child = pty.slave.spawn_command(cmd).map_err(|e| {
        format!("could not start ipmitool (sudo apt install ipmitool): {e}")
    })?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pty.master.take_writer().map_err(|e| e.to_string())?;
    status(app, format!("connecting to the serial console of {ip} (SOL)..."));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (app, done) = (app.clone(), done.clone());
        let (ip, user, pass) = (ip.to_string(), user.to_string(), pass.to_string());
        thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            let mut saved = !remember;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // ipmitool says this once the iDRAC has accepted the login
                        if !saved && String::from_utf8_lossy(&buf[..n]).contains("SOL Session operational") {
                            keychain::remember(&user, &ip, &pass);
                            saved = true;
                        }
                        emit_term(&app, &buf[..n]);
                    }
                }
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    loop {
        if done.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stop => {
                    // ipmitool must exit by itself (via its ~. escape): that is what
                    // deactivates the SOL payload on the iDRAC. Killing it leaves
                    // the session "active" there.
                    let _ = writer.write_all(b"\r~.");
                    let _ = writer.flush();
                    let deadline = std::time::Instant::now() + Duration::from_secs(4);
                    while std::time::Instant::now() < deadline {
                        if let Ok(Some(_)) = child.try_wait() {
                            return Ok(());
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    // still running: deactivate from outside, then stop it
                    let _ = std::process::Command::new("ipmitool")
                        .args(["-I", "lanplus", "-H", ip, "-U", user, "-E", "sol", "deactivate"])
                        .env("IPMI_PASSWORD", pass)
                        .output();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                Cmd::Input(bytes) => {
                    debug_bytes("term-input", &bytes);
                    let _ = writer.write_all(&bytes);
                    let _ = writer.flush();
                }
                Cmd::Resize(c, r) => {
                    let _ = pty.master.resize(size(c, r));
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = child.wait();
    Ok(())
}

#[tauri::command]
fn connect_text(app: AppHandle, state: State<Session>, protocol: String, ip: String, user: String, pass: String, remember: bool, cols: u16, rows: u16) {
    disconnect(state.clone());
    wait_text_session(8); // let the previous session release the console first
    let (tx, rx) = channel();
    *state.0.lock().unwrap() = Some(tx);
    if std::env::var("BOBCAT_DEBUG").is_ok() {
        eprintln!("[bobcat] terminal size {cols}x{rows}");
    }
    let handle = thread::spawn(move || {
        status(&app, format!("connecting to {ip} ({protocol})..."));
        let r = match protocol.as_str() {
            "sol" => text_sol(&app, &ip, &user, &pass, remember, cols, rows, &rx),
            "amtsol" => text_amtsol(&app, &ip, &user, &pass, remember, &rx),
            _ => text_ssh(&app, &ip, &user, &pass, remember, cols, rows, &rx),
        };
        match r {
            Ok(()) => status(&app, "disconnected"),
            Err(e) => status(&app, format!("error: {e}")),
        }
    });
    *TEXT_THREAD.lock().unwrap() = Some(handle);
}

#[tauri::command]
fn term_input(state: State<Session>, data: String) {
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
        send(&state, Cmd::Input(bytes));
    }
}

#[tauri::command]
fn term_resize(state: State<Session>, cols: u16, rows: u16) {
    if std::env::var("BOBCAT_DEBUG").is_ok() {
        eprintln!("[bobcat] terminal resized to {cols}x{rows}");
    }
    send(&state, Cmd::Resize(cols, rows));
}

/// Local clipboard text (for pasting into the text consoles).
#[tauri::command]
fn clipboard_text() -> Result<String, String> {
    with_clip(|c| c.get_text())
}

/// Put text on the local clipboard (copying from the text consoles).
#[tauri::command]
fn set_clipboard(text: String) -> Result<(), String> {
    with_clip(|c| c.set_text(text))
}

/// BOBCAT_PROTOCOL=vnc|html5|ssh|sol selects the starting protocol.
#[tauri::command]
fn default_protocol() -> String {
    std::env::var("BOBCAT_PROTOCOL").unwrap_or_else(|_| "vnc".into())
}

/// The password saved in the keychain for this user and iDRAC, if any.
#[tauri::command]
fn saved_password(user: String, ip: String) -> Option<String> {
    keychain::load(&user, &ip)
}

#[tauri::command]
fn forget_password(user: String, ip: String) {
    keychain::forget(&user, &ip);
}

/// Set BOBCAT_IP to start with that iDRAC address.
#[tauri::command]
fn default_ip() -> String {
    std::env::var("BOBCAT_IP").unwrap_or_default()
}

#[tauri::command]
fn default_user() -> String {
    std::env::var("BOBCAT_USER").unwrap_or_default()
}

#[tauri::command]
fn connect(app: AppHandle, state: State<Session>, ip: String, port: u16, tls: bool, pass: String, remember: bool) {
    if std::env::var("BOBCAT_DEBUG").is_ok() {
        eprintln!("[bobcat] connect() called");
    }
    disconnect(state.clone());
    let (tx, rx) = channel();
    *state.0.lock().unwrap() = Some(tx);
    thread::spawn(move || {
        // The iDRAC allows one VNC client and takes a few seconds to release a
        // session (it may reset or refuse connections meanwhile), so retry.
        const ATTEMPTS: u32 = 6;
        for attempt in 1..=ATTEMPTS {
            status(&app, format!("connecting to {ip}:{port}..."));
            match run(&app, &ip, port, tls, &pass, remember, &rx) {
                Ok(()) => return status(&app, "disconnected"),
                Err(e) => {
                    let retry = attempt < ATTEMPTS
                        && !e.contains("uthentication")
                        && !e.starts_with("WARNING")
                        && (e.contains("reset") || e.contains("refused") || e.contains("handshake") || e.contains("timed out") || e.contains("failed to fill"));
                    if !retry {
                        return status(&app, format!("error: {e}"));
                    }
                    status(&app, format!("{e}; retrying in 5s ({attempt}/{ATTEMPTS})"));
                    for _ in 0..50 {
                        if let Ok(Cmd::Stop) = rx.try_recv() {
                            return status(&app, "disconnected");
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    });
}

/// Intel AMT KVM: log in on the redirection port (16994), bridge the device's RFB stream to a
/// local port and show it in the VNC canvas. The bridge serves one viewer, so there are no retries.
#[tauri::command]
fn connect_amt(app: AppHandle, state: State<Session>, ip: String, user: String, pass: String, remember: bool) {
    disconnect(state.clone());
    let (tx, rx) = channel();
    *state.0.lock().unwrap() = Some(tx);
    thread::spawn(move || {
        status(&app, format!("connecting to the AMT KVM at {ip}..."));
        let user = if user.is_empty() { "admin".to_string() } else { user };
        let port = match amt_redir::kvm_bridge(&ip, 16994, &user, &pass) {
            Ok(p) => p,
            Err(e) => return status(&app, format!("error: {e}")),
        };
        if remember {
            keychain::remember(&user, &ip, &pass);
        }
        match run(&app, "127.0.0.1", port, false, "", false, &rx) {
            Ok(()) => status(&app, "disconnected"),
            Err(e) => status(&app, format!("error: {e}")),
        }
    });
}

#[tauri::command]
fn disconnect(state: State<Session>) {
    if let Some(tx) = state.0.lock().unwrap().take() {
        let _ = tx.send(Cmd::Stop);
    }
}

/// Map a character to (X11 keysym, needs Shift) for a US layout.
fn char_keysym(c: char) -> Option<(u32, bool)> {
    match c {
        '\n' => Some((0xff0d, false)),
        '\t' => Some((0xff09, false)),
        '\r' => None,
        'A'..='Z' | '~' | '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_' | '+'
        | '{' | '}' | '|' | ':' | '"' | '<' | '>' | '?' => Some((c as u32, true)),
        c if (c as u32) < 0x100 => Some((c as u32, false)),
        c => Some((0x0100_0000 + c as u32, false)),
    }
}

/// Type the local clipboard text into the console as key events.
#[tauri::command]
fn paste_clipboard(state: State<Session>, keys: Option<Vec<(u32, bool)>>) -> Result<usize, String> {
    // `keys` (keysym, shift) come from the page, worked out for the selected keyboard layout;
    // without them the clipboard is typed with the US layout
    let keys: Vec<(u32, bool)> = match keys {
        Some(k) => k,
        None => with_clip(|c| c.get_text())?.chars().filter_map(char_keysym).collect(),
    };
    let tx = state.0.lock().unwrap().clone().ok_or("not connected")?;
    let n = keys.len();
    if n > 10_000 {
        return Err(format!("clipboard too large ({n} chars, max 10000)"));
    }
    thread::spawn(move || {
        let key = |down, keysym| {
            let _ = tx.send(Cmd::Key { down, keysym });
            thread::sleep(Duration::from_millis(8));
        };
        for (sym, shift) in keys {
            if shift {
                key(true, 0xffe1);
            }
            key(true, sym);
            key(false, sym);
            if shift {
                key(false, 0xffe1);
            }
        }
    });
    Ok(n)
}

/// OCR a PNG with the `tesseract` CLI and put the text on the local clipboard.
pub(crate) fn ocr_png_to_clipboard(png: &[u8]) -> Result<String, String> {
    ocr_png_to_clipboard_args(png, &[])
}

/// Same, with extra tesseract arguments.
pub(crate) fn ocr_png_to_clipboard_args(png: &[u8], extra: &[&str]) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("tesseract")
        .args(["stdin", "stdout", "--psm", "6"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "tesseract not found (install it, e.g. `sudo apt install tesseract-ocr`)".to_string()
            }
            _ => e.to_string(),
        })?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let png = png.to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&png);
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !out.status.success() {
        return Err("tesseract failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("no text recognised in selection".into());
    }
    with_clip(|c| c.set_text(text.clone()))?;
    Ok(text)
}

/// OCR a PNG of a selected screen region and put the text on the local clipboard.
#[tauri::command]
fn copy_region_text(png_b64: String) -> Result<String, String> {
    let png = base64::engine::general_purpose::STANDARD
        .decode(png_b64)
        .map_err(|e| e.to_string())?;
    ocr_png_to_clipboard(&png)
}

/// Resize the window's content area to w x h (logical px), capped to the monitor.
#[tauri::command]
fn fit_window(window: tauri::WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    let (mut w, mut h) = (w, h);
    if let Ok(Some(m)) = window.current_monitor() {
        let sf = m.scale_factor();
        w = w.min(m.size().width as f64 / sf);
        h = h.min(m.size().height as f64 / sf - 80.0); // leave room for title bar/panels
    }
    window
        .set_size(tauri::LogicalSize::new(w, h))
        .map_err(|e| e.to_string())
}

fn send(state: &State<Session>, cmd: Cmd) {
    if let Some(tx) = state.0.lock().unwrap().as_ref() {
        let _ = tx.send(cmd);
    }
}

#[tauri::command]
fn send_key(state: State<Session>, down: bool, keysym: u32) {
    send(&state, Cmd::Key { down, keysym });
}

#[tauri::command]
fn send_pointer(state: State<Session>, buttons: u8, x: u16, y: u16) {
    send(&state, Cmd::Pointer { buttons, x, y });
}

fn main() {
    // the Java console is embedded by reparenting its X11 window, so the main window must be an X11 one
    // (XWayland under Wayland); set GDK_BACKEND yourself to override
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    tauri::Builder::default()
        .manage(Session::default())
        .setup(|app| {
            // Ctrl+C / SIGTERM (e.g. when started with `cargo run`) hang up cleanly too
            let handle = app.handle().clone();
            let _ = ctrlc::set_handler(move || {
                shutdown(&handle);
                std::process::exit(0);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                use tauri::Manager;
                shutdown(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![view_cert, trust_cert, default_password, saved_password, forget_password, default_protocol, default_user, default_ip, clipboard_text, set_clipboard, connect_text, term_input, term_resize, default_tls, autoconnect, connect, connect_amt, disconnect, html5::connect_html5, html5::disconnect_html5, java_console::connect_java, java_console::disconnect_java, java_console::java_paste, java_console::java_copy_text, java_console::download_java_runtime, html5::html5_layout, html5_tools::html5_paste, html5_tools::html5_copy_text, html5_tools::html5_zoom, send_key, send_pointer, paste_clipboard, copy_region_text, fit_window])
        .run(tauri::generate_context!())
        .expect("error running bobcat");
}
