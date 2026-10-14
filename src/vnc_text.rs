// bobcat-cli: text view of the iDRAC VNC console (--console vnc)
// License: CC BY-NC-SA 4.0 (see LICENSE.md)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use std::sync::mpsc;

use crate::screen_text::{self, Glyphs, Votes};
use vnc::client::{AuthChoice, AuthMethod, Event};
use vnc::{Client, Encoding, PixelFormat, Rect};

/// Why the TLS forwarder gave up (so the VNC handshake error can say why).
static FWD_ERR: Mutex<Option<String>> = Mutex::new(None);

/// TLS leg of the iDRAC's encrypted VNC mode: TLS 1.2 (static-RSA AES256-GCM,
/// self-signed certificate, ~10s handshake) with plain RFB inside. The `vnc`
/// crate only does plain TCP, so a local forwarder handles the TLS. The
/// certificate is pinned on first use (shared with bobcat).
fn tls_forwarder(ip: &str, port: u16, trust_new: bool) -> Result<u16, String> {
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
            tcp.set_read_timeout(Some(Duration::from_secs(60))).ok(); // the iDRAC takes ~10s
            let tls = cfg.connect("idrac", tcp).map_err(|e| format!("tls handshake: {e}"))?;
            check_pin(&host, &tls, trust_new)?;
            let (local_conn, _) = listener.accept().map_err(|e| e.to_string())?;

            // openssl streams aren't splittable: share one behind a lock and
            // poll with short read timeouts so both directions make progress.
            let tcp = tls.get_ref().try_clone().map_err(|e| e.to_string())?;
            tcp.set_read_timeout(Some(Duration::from_millis(5))).ok();
            let tls = std::sync::Arc::new(Mutex::new(tls));
            let mut local_rd = local_conn.try_clone().map_err(|e| e.to_string())?;
            let mut local_wr = local_conn;
            local_rd.set_read_timeout(Some(Duration::from_millis(5))).ok();
            let would_block =
                |e: &std::io::Error| matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let mut idle = true;
                let n = { tls.lock().unwrap().read(&mut buf) };
                match n {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        local_wr.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                        idle = false;
                    }
                    Err(e) if would_block(&e) => {}
                    Err(e) => return Err(e.to_string()),
                }
                match local_rd.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        let mut t = tls.lock().unwrap();
                        t.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                        t.flush().ok();
                        idle = false;
                    }
                    Err(e) if would_block(&e) => {}
                    Err(e) => return Err(e.to_string()),
                }
                if idle {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        };
        if let Err(e) = run() {
            *FWD_ERR.lock().unwrap() = Some(e);
        }
    });
    Ok(local)
}

/// Trust on first use for the TLS certificate (key = the iDRAC's address, the same
/// entry bobcat uses). A changed certificate blocks the connection.
pub(crate) fn check_pin(host: &str, tls: &openssl::ssl::SslStream<TcpStream>, trust_new: bool) -> Result<(), String> {
    let cert = tls.ssl().peer_certificate().ok_or("the iDRAC presented no certificate")?;
    let fp = cert
        .digest(openssl::hash::MessageDigest::sha256())
        .map_err(|e| e.to_string())?
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    match crate::load_pins().get(host) {
        None => {
            crate::save_pin(host, &fp)?;
            eprintln!("Saved TLS certificate for {host} (SHA-256 {fp})");
            Ok(())
        }
        Some(p) if *p == fp => Ok(()),
        Some(_) if trust_new => {
            crate::save_pin(host, &fp)?;
            eprintln!("Replaced the saved TLS certificate for {host} (SHA-256 {fp})");
            Ok(())
        }
        Some(p) => Err(format!(
            "WARNING: the TLS certificate for {host} has changed since it was first saved; connection blocked.\n  saved: {p}\n  now:   {fp}\nIf you expected this, run again with --trust-new-hostkey."
        )),
    }
}

/// The remote screen, as 0x00RRGGBB pixels.
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Frame {
    fn new(w: usize, h: usize) -> Frame {
        Frame { w, h, px: vec![0; w * h] }
    }

    /// Copy a rectangle of server pixels (32-bit little-endian B,G,R,X) in.
    fn put(&mut self, r: Rect, data: &[u8]) {
        let (x0, y0, rw, rh) = (r.left as usize, r.top as usize, r.width as usize, r.height as usize);
        for row in 0..rh {
            let y = y0 + row;
            if y >= self.h {
                break;
            }
            for col in 0..rw {
                let x = x0 + col;
                let i = (row * rw + col) * 4;
                if x >= self.w || i + 3 >= data.len() {
                    continue;
                }
                self.px[y * self.w + x] = (data[i + 2] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i] as u32;
            }
        }
    }
}

/// Write the frame as a binary PPM (for offline analysis).
fn dump_ppm(f: &Frame, path: &str) -> std::io::Result<()> {
    let mut out = std::fs::File::create(path)?;
    write!(out, "P6\n{} {}\n255\n", f.w, f.h)?;
    let mut bytes = Vec::with_capacity(f.w * f.h * 3);
    for p in &f.px {
        bytes.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    out.write_all(&bytes)
}

/// One key press to send: an X11 keysym plus modifiers.
struct Key {
    sym: u32,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// A typed character as a key press (US layout: capitals and shifted symbols need Shift).
fn char_key(c: char) -> Key {
    let sym = if (c as u32) < 0x100 { c as u32 } else { 0x0100_0000 + c as u32 };
    Key { sym, shift: c.is_ascii_uppercase() || SHIFTED.contains(c), ctrl: false, alt: false }
}

fn named(sym: u32) -> Key {
    Key { sym, shift: false, ctrl: false, alt: false }
}

/// Turn bytes typed on the terminal into key presses (letters, Ctrl combinations,
/// Enter/Tab/Backspace, arrows, Home/End, PgUp/PgDn, Ins/Del, F1-F12, Alt+key).
fn parse_keys(b: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            0x1b => {
                if i + 1 >= b.len() {
                    keys.push(named(0xff1b)); // a lone Esc
                    i += 1;
                } else if b[i + 1] == b'[' || b[i + 1] == b'O' {
                    let ss3 = b[i + 1] == b'O';
                    let mut j = i + 2;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    if j >= b.len() {
                        i = b.len();
                        continue;
                    }
                    let params: Vec<u32> = std::str::from_utf8(&b[i + 2..j])
                        .unwrap_or("")
                        .split(';')
                        .map(|p| p.parse().unwrap_or(0))
                        .collect();
                    let first = params.first().copied().unwrap_or(0);
                    let modifier = params.get(1).copied().unwrap_or(1).saturating_sub(1);
                    let sym = match (b[j], first, ss3) {
                        (b'A', _, _) => Some(0xff52),
                        (b'B', _, _) => Some(0xff54),
                        (b'C', _, _) => Some(0xff53),
                        (b'D', _, _) => Some(0xff51),
                        (b'H', _, _) => Some(0xff50),
                        (b'F', _, _) => Some(0xff57),
                        (b'P', _, true) => Some(0xffbe),
                        (b'Q', _, true) => Some(0xffbf),
                        (b'R', _, true) => Some(0xffc0),
                        (b'S', _, true) => Some(0xffc1),
                        (b'~', 1, _) | (b'~', 7, _) => Some(0xff50),
                        (b'~', 2, _) => Some(0xff63),
                        (b'~', 3, _) => Some(0xffff),
                        (b'~', 4, _) | (b'~', 8, _) => Some(0xff57),
                        (b'~', 5, _) => Some(0xff55),
                        (b'~', 6, _) => Some(0xff56),
                        (b'~', 11..=14, _) => Some(0xffbe + (first - 11)),
                        (b'~', 15, _) => Some(0xffc2),
                        (b'~', 17..=21, _) => Some(0xffc3 + (first - 17)),
                        (b'~', 23, _) => Some(0xffc8),
                        (b'~', 24, _) => Some(0xffc9),
                        _ => None,
                    };
                    if let Some(sym) = sym {
                        keys.push(Key { sym, shift: modifier & 1 != 0, alt: modifier & 2 != 0, ctrl: modifier & 4 != 0 });
                    }
                    i = j + 1;
                } else {
                    // Esc followed by a key: Alt+key
                    let mut k = parse_keys(&b[i + 1..i + 2]);
                    if let Some(first) = k.first_mut() {
                        first.alt = true;
                    }
                    keys.extend(k);
                    i += 2;
                }
            }
            b'\r' | b'\n' => {
                keys.push(named(0xff0d));
                i += 1;
            }
            b'\t' => {
                keys.push(named(0xff09));
                i += 1;
            }
            0x7f | 0x08 => {
                keys.push(named(0xff08));
                i += 1;
            }
            1..=26 => {
                keys.push(Key { sym: (b'a' + c - 1) as u32, shift: false, ctrl: true, alt: false });
                i += 1;
            }
            0x20..=0x7e => {
                keys.push(char_key(c as char));
                i += 1;
            }
            _ => {
                // UTF-8 character
                let len = match c {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                if let Some(ch) = b.get(i..i + len).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) {
                    keys.push(char_key(ch));
                }
                i += len;
            }
        }
    }
    keys
}

fn send_key(client: &mut Client, k: &Key) -> Result<(), String> {
    let mut mods = Vec::new();
    if k.ctrl {
        mods.push(0xffe3);
    }
    if k.alt {
        mods.push(0xffe9);
    }
    if k.shift {
        mods.push(0xffe1);
    }
    let e = |e: vnc::Error| e.to_string();
    for m in &mods {
        client.send_key_event(true, *m).map_err(e)?;
    }
    client.send_key_event(true, k.sym).map_err(e)?;
    client.send_key_event(false, k.sym).map_err(e)?;
    for m in mods.iter().rev() {
        client.send_key_event(false, *m).map_err(e)?;
    }
    Ok(())
}

pub fn run(ip: &str, port: u16, tls: bool, vncpass: &str, trust_new: bool, relearn: bool) -> Result<(), String> {
    crate::install_signal_handlers();
    let (host, port) = if tls {
        eprintln!("Negotiating TLS (the iDRAC takes ~10-15s)...");
        ("127.0.0.1".to_string(), tls_forwarder(ip, port, trust_new)?)
    } else {
        (ip.to_string(), port)
    };
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("bad address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("connect failed: {e}"))?;
    // a TLS-only server never sends the RFB greeting to a plain client (and vice versa)
    tcp.set_read_timeout(Some(Duration::from_secs(if tls { 90 } else { 15 }))).ok();
    let mut key = [0u8; 8];
    for (i, b) in vncpass.bytes().take(8).enumerate() {
        key[i] = b;
    }
    let tcp_for_timeout = tcp.try_clone().map_err(|e| e.to_string())?;
    let mut client = Client::from_tcp_stream(tcp, true, |methods| {
        methods.iter().find_map(|m| match m {
            AuthMethod::Password => Some(AuthChoice::Password(key)),
            _ => None,
        })
    })
    .map_err(|e| {
        thread::sleep(Duration::from_millis(300)); // let the forwarder record why it quit
        let why = FWD_ERR.lock().unwrap().take();
        match why {
            Some(w) if w.contains("wrong version number") || w.contains("tls handshake") && !w.contains("timed out") => {
                format!("{w} (VNC encryption may be off on the iDRAC: try --notls)")
            }
            Some(w) => w,
            None if !tls => {
                format!("vnc handshake: {e} (no greeting from the server; VNC encryption may be on: try --tls)")
            }
            None => format!("vnc handshake: {e}"),
        }
    })?;
    tcp_for_timeout.set_read_timeout(None).ok();

    // Force 32-bit little-endian B,G,R,X (the server's native 16-bit format gave
    // wrong colours). set_format waits for one full-screen rectangle, which only
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
    client.set_encodings(&[Encoding::Zrle, Encoding::Raw]).map_err(|e| e.to_string())?;
    let (w, h) = client.size();
    let mut frame = Frame::new(w as usize, h as usize);
    eprintln!("Connected: {} ({w}x{h})", client.name());
    let full = |w: u16, h: u16| Rect { left: 0, top: 0, width: w, height: h };
    client.request_update(full(w, h), false).map_err(|e| e.to_string())?;

    let dump = std::env::var("BOBCAT_DUMP").ok();
    // BOBCAT_ONCE=1: print the screen as text once and exit (no keyboard; for scripts)
    let once = std::env::var("BOBCAT_ONCE").is_ok();
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let interactive = tty && !once;
    let _raw = if interactive { Some(crate::RawMode::enter()?) } else { None };

    let mut glyphs = Glyphs::default();
    // the table learned on earlier runs, so the first screen can skip the OCR step
    let saved = if relearn { None } else { screen_text::load_saved(ip) };
    let mut grid: Option<(usize, usize)> = None;
    let mut calib: Option<mpsc::Receiver<Result<Votes, String>>> = None;
    let mut last_calib = Instant::now() - Duration::from_secs(60);
    let mut calibrations = 0;
    let mut calib_snapshot: Option<screen_text::Cells> = None;
    let mut calib_err: Option<String> = None;
    let mut last_shown: Vec<String> = Vec::new();
    let mut last_note = String::new();
    let mut last_change = Instant::now();
    let mut dirty = false;
    let mut need_render = false;
    // after the picture settles we ask for one clean full frame and render that, so a
    // half-finished scroll is never read
    let mut verifying: Option<Instant> = None;
    let started = Instant::now();
    let debug = std::env::var("BOBCAT_DEBUG").is_ok();
    let (mut line_start, mut tilde) = (true, false);
    let mut buf = [0u8; 4096];

    let result: Result<(), String> = 'main: loop {
        if crate::signaled() {
            break Ok(());
        }
        // --- server -> frame
        let mut got = false;
        while let Some(ev) = client.poll_event() {
            got = true;
            match ev {
                Event::PutPixels(r, px) => {
                    frame.put(r, &px);
                    last_change = Instant::now();
                    dirty = true;
                }
                Event::EndOfFrame => {
                    if let Err(e) = client.request_update(full(w, h), true) {
                        break 'main Err(e.to_string());
                    }
                }
                Event::CopyPixels { .. } => {
                    if let Err(e) = client.request_update(full(w, h), false) {
                        break 'main Err(e.to_string());
                    }
                }
                Event::Disconnected(e) => {
                    break 'main Err(match e {
                        Some(e) => format!("disconnected: {e}"),
                        None => "disconnected".into(),
                    })
                }
                _ => {}
            }
        }
        // --- finished calibration
        if let Some(rx) = &calib {
            if let Ok(res) = rx.try_recv() {
                calib = None;
                if debug {
                    eprintln!("[{:.1}s] calibration finished: {}", started.elapsed().as_secs_f32(), if res.is_ok() { "ok" } else { "error" });
                }
                match res {
                    Ok(v) => {
                        glyphs.merge(v);
                        calib_err = None;
                        // shapes it could not name (a glyph clipped at the screen edge) are
                        // given up on, so they don't trigger endless OCR runs
                        if let Some(snap) = calib_snapshot.take() {
                            let mut unnamed: Vec<screen_text::Shape> = Vec::new();
                            for k in snap.keys.iter().flatten() {
                                if glyphs.lookup(k).is_none() && !unnamed.contains(k) {
                                    unnamed.push(k.clone());
                                }
                            }
                            glyphs.give_up_on(unnamed);
                        }
                        if let Some((cw, ch)) = grid {
                            if let Err(e) = screen_text::save(ip, cw, ch, glyphs.votes(), glyphs.unreadable()) {
                                if debug {
                                    eprintln!("could not save the learned characters: {e}");
                                }
                            }
                        }
                    }
                    Err(e) => calib_err = Some(e),
                }
                need_render = true;
            }
        }
        // --- keyboard -> server
        if interactive {
            let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut fd, 1, if got { 0 } else { 5 }) };
            if ready > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let n = std::io::stdin().read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break Ok(());
                }
                // ~. at the start of a line quits
                let mut typed: Vec<u8> = Vec::new();
                for &b in &buf[..n] {
                    if tilde {
                        tilde = false;
                        if b == b'.' {
                            break 'main Ok(());
                        }
                        typed.push(b'~');
                    }
                    if line_start && b == b'~' {
                        tilde = true;
                        line_start = false;
                        continue;
                    }
                    line_start = b == b'\r' || b == b'\n';
                    typed.push(b);
                }
                for k in parse_keys(&typed) {
                    if let Err(e) = send_key(&mut client, &k) {
                        break 'main Err(e);
                    }
                    thread::sleep(Duration::from_millis(4));
                }
            }
        } else if !got {
            thread::sleep(Duration::from_millis(5));
        }

        // --- developer aid: save the raw frame
        if let (Some(path), true) = (&dump, started.elapsed() > Duration::from_secs(2) && last_change.elapsed() > Duration::from_millis(800) && frame.px.iter().any(|&p| p != 0)) {
            if let Err(e) = dump_ppm(&frame, path) {
                break Err(e.to_string());
            }
            eprintln!("dumped frame to {path}");
            break Ok(());
        }

        // --- screen -> text, once the picture has settled
        if dirty && last_change.elapsed() >= Duration::from_millis(250) && verifying.is_none() && !need_render {
            if let Err(e) = client.request_update(full(w, h), false) {
                break Err(e.to_string());
            }
            verifying = Some(Instant::now());
            dirty = false;
        }
        let verified = match verifying {
            Some(t) => (dirty && last_change.elapsed() >= Duration::from_millis(250)) || t.elapsed() > Duration::from_millis(1500),
            None => false,
        };
        if verified || need_render {
            verifying = None;
            dirty = false;
            need_render = false;
            if grid.is_none() {
                grid = screen_text::detect_grid(&frame.px, frame.w, frame.h);
            }
            let (lines, note) = match grid {
                None => (Vec::new(), "no text console detected on this screen (graphics?)".to_string()),
                Some((cw, ch)) => {
                    let cells = screen_text::analyse(&frame.px, frame.w, frame.h, cw, ch);
                    glyphs.set_grid(cells.grid);
                    if glyphs.is_empty() {
                        if let Some(((scw, sch), v, unreadable)) = &saved {
                            if (*scw, *sch) == (cw, ch) {
                                glyphs.merge(v.clone());
                                glyphs.give_up_on(unreadable.clone());
                                if debug {
                                    eprintln!("[{:.1}s] loaded {} saved shapes", started.elapsed().as_secs_f32(), glyphs.votes().len());
                                }
                            }
                        }
                    }
                    let nonempty = cells.keys.iter().flatten().count();
                    let (lines, unknown, fresh) = screen_text::render(&cells, &mut glyphs);
                    if !fresh.is_empty() && calib.is_none() && last_calib.elapsed() > Duration::from_secs(2) && calibrations < 12 {
                        let (tx, rx) = mpsc::channel();
                        let c = cells.clone();
                        calib_snapshot = Some(cells.clone());
                        thread::spawn(move || {
                            let _ = tx.send(screen_text::ocr_votes(&c, &[3, 4, 5]));
                        });
                        calib = Some(rx);
                        last_calib = Instant::now();
                        calibrations += 1;
                        if debug {
                            eprintln!("[{:.1}s] calibration #{calibrations} started ({unknown} unknown cells)", started.elapsed().as_secs_f32());
                        }
                    }
                    if unknown * 3 > nonempty * 2 && calibrations >= 2 {
                        grid = None; // mostly unreadable: the grid guess may be wrong
                    }
                    let note = if let Some(e) = &calib_err {
                        format!("text recognition failed: {e}")
                    } else if calib.is_some() {
                        "recognising text...".to_string()
                    } else if unknown > 0 {
                        format!("{unknown} unreadable cells shown as ?")
                    } else {
                        String::new()
                    };
                    (lines, note)
                }
            };
            if lines != last_shown || note != last_note {
                if once {
                    if calib.is_none() && (!note.contains("recogn")) || started.elapsed() > Duration::from_secs(60) {
                        println!("{}", lines.join("\n"));
                        if !note.is_empty() {
                            eprintln!("({note})");
                        }
                        break Ok(());
                    }
                } else {
                    let mut out = String::from("\x1b[H\x1b[2J");
                    out.push_str(&lines.join("\r\n"));
                    out.push_str("\r\n\x1b[7m bobcat-cli VNC text view | ~. quits ");
                    if !note.is_empty() {
                        out.push_str(&format!("| {note} "));
                    }
                    out.push_str("\x1b[0m");
                    print!("{out}");
                    let _ = std::io::stdout().flush();
                }
                last_shown = lines;
                last_note = note;
            }
        }
        if started.elapsed() > Duration::from_secs(40) && !dirty && frame.px.iter().all(|&p| p == 0) && !interactive {
            break Err("timed out waiting for a frame".into());
        }
    };
    let _ = client.disconnect();
    drop(_raw);
    if interactive {
        eprintln!("\n[disconnected]");
    }
    result
}
