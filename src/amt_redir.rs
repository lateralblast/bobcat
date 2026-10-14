// bobcat-cli: Intel AMT redirection protocol (TCP 16994): the channel that carries Serial over LAN,
// IDE redirection and the KVM. A session starts with `StartRedirectionSession` naming the protocol
// ("SOL ", "IDER", "KVMR"), then the client authenticates with HTTP-style digest in
// `AuthenticateSession` messages. After that the channel carries the protocol itself; for the KVM
// that is an RFB (VNC) stream, bridged here to a local port so any VNC viewer can use it.
// License: CC BY-NC-SA 4.0 (LICENSE.md).

use crate::amt::{hex, md5_hex};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::thread;
use std::time::Duration;

fn debug() -> bool {
    std::env::var_os("BOBCAT_DEBUG").is_some()
}

fn dump(dir: &str, b: &[u8]) {
    if debug() {
        eprintln!("[redir] {dir} {} bytes: {}", b.len(), hex(&b[..b.len().min(96)]));
    }
}

/// Milliseconds since the bridge started, for the message trace.
fn ms() -> u128 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis()
}

fn send(s: &mut TcpStream, b: &[u8]) -> Result<(), String> {
    dump("->", b);
    s.write_all(b).map_err(|e| format!("sending to the AMT device: {e}"))
}

fn read_exact(s: &mut TcpStream, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).map_err(|e| format!("reading from the AMT device: {e}"))?;
    dump("<-", &buf);
    Ok(buf)
}

fn le32(n: usize) -> [u8; 4] {
    (n as u32).to_le_bytes()
}

/// A length-prefixed (1 byte) field of a digest message.
fn pstr(out: &mut Vec<u8>, s: &str) {
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

fn take_pstr(data: &[u8], at: &mut usize) -> Result<String, String> {
    let n = *data.get(*at).ok_or("short digest challenge")? as usize;
    let s = data.get(*at + 1..*at + 1 + n).ok_or("short digest challenge")?;
    *at += 1 + n;
    Ok(String::from_utf8_lossy(s).to_string())
}

/// One AuthenticateSessionReply: (status, auth type, data).
fn read_auth_reply(s: &mut TcpStream) -> Result<(u8, u8, Vec<u8>), String> {
    let h = read_exact(s, 9)?;
    if h[0] != 0x14 {
        return Err(format!("unexpected answer from the AMT redirection port (message 0x{:02x})", h[0]));
    }
    let len = u32::from_le_bytes([h[5], h[6], h[7], h[8]]) as usize;
    let data = if len > 0 { read_exact(s, len)? } else { Vec::new() };
    Ok((h[1], h[4], data))
}

/// Open a redirection session for `proto` ("SOL ", "IDER" or "KVMR") and authenticate.
pub fn open(ip: &str, port: u16, proto: &[u8; 4], user: &str, pass: &str) -> Result<TcpStream, String> {
    let addr = (ip, port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad address")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(8)).map_err(|e| format!("cannot connect to {ip}:{port} ({e})"))?;
    s.set_read_timeout(Some(Duration::from_secs(15))).ok();
    s.set_nodelay(true).ok();

    // StartRedirectionSession
    let mut start = vec![0x10, 0, 0, 0];
    start.extend_from_slice(proto);
    send(&mut s, &start)?;
    let reply = read_exact(&mut s, 13)?;
    if reply[0] != 0x11 {
        return Err(format!("unexpected answer to StartRedirectionSession (message 0x{:02x})", reply[0]));
    }
    let oem = reply[12] as usize;
    if oem > 0 {
        read_exact(&mut s, oem)?;
    }
    if reply[1] != 0 {
        return Err(format!(
            "the AMT device refused the {} redirection session (status {}){}",
            String::from_utf8_lossy(proto).trim(),
            reply[1],
            if reply[1] == 2 { ": the redirection listener may be off or another session of this kind is open" } else { "" }
        ));
    }

    // which authentication methods are offered
    send(&mut s, &[0x13, 0, 0, 0, 0, 0, 0, 0, 0])?;
    let (_, _, methods) = read_auth_reply(&mut s)?;
    if !methods.contains(&4) {
        return Err(format!("the device offers no digest authentication (methods {methods:?})"));
    }

    // digest, first request without a response: the device answers with realm, nonce and qop
    let uri = "/RedirectionService";
    let mut first = vec![0x13, 0, 0, 0, 4];
    first.extend_from_slice(&le32(user.len() + uri.len() + 8));
    pstr(&mut first, user);
    first.extend_from_slice(&[0, 0]);
    pstr(&mut first, uri);
    first.extend_from_slice(&[0, 0, 0, 0]);
    send(&mut s, &first)?;
    let (status, _, data) = read_auth_reply(&mut s)?;
    if status == 0 {
        return Ok(s); // no challenge needed
    }
    let (mut at, realm, nonce, qop);
    at = 0;
    realm = take_pstr(&data, &mut at)?;
    nonce = take_pstr(&data, &mut at)?;
    qop = take_pstr(&data, &mut at).unwrap_or_default();

    let mut rnd = [0u8; 8];
    openssl::rand::rand_bytes(&mut rnd).map_err(|e| e.to_string())?;
    let cnonce = hex(&rnd);
    let nc = "00000002";
    let ha1 = md5_hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5_hex(&format!("POST:{uri}"));
    let digest = if qop.is_empty() {
        md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
    } else {
        md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:{qop}:{ha2}"))
    };
    let total = user.len() + realm.len() + nonce.len() + uri.len() + cnonce.len() + nc.len() + digest.len() + qop.len() + 8;
    let mut second = vec![0x13, 0, 0, 0, 4];
    second.extend_from_slice(&le32(total));
    for f in [user, realm.as_str(), nonce.as_str(), uri, cnonce.as_str(), nc, digest.as_str(), qop.as_str()] {
        pstr(&mut second, f);
    }
    send(&mut s, &second)?;
    let (status, _, _) = read_auth_reply(&mut s)?;
    if status != 0 {
        return Err("login to the redirection port failed: wrong user name or password".into());
    }
    Ok(s)
}

/// Read what the device sends until it has been quiet for `idle` seconds.
fn collect(s: &mut TcpStream, idle: u64) -> Vec<u8> {
    s.set_read_timeout(Some(Duration::from_secs(idle))).ok();
    let mut all = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
    }
    all
}

fn show(what: &str, b: &[u8]) {
    if b.is_empty() {
        println!("{what}: nothing");
    } else {
        println!("{what}: {} bytes: {}  ({:?})", b.len(), hex(&b[..b.len().min(96)]), String::from_utf8_lossy(&b[..b.len().min(96)]));
    }
}

/// The KVM start message: 0x40 and zeros (the device answers 0x41 ...).
const KVM_START: [u8; 8] = [0x40, 0, 0, 0, 0, 0, 0, 0];

/// Diagnostic: open a session and walk the first steps of the protocol, showing every answer: wait
/// for the device, send the KVM start message (or the hex in BOBCAT_REDIR_SEND), and if an RFB
/// version line arrives, answer it with 3.8 and show what follows.
pub fn test(ip: &str, port: u16, proto: &[u8; 4], user: &str, pass: &str) -> Result<(), String> {
    let mut s = open(ip, port, proto, user, pass)?;
    println!("{} redirection session opened and authenticated", String::from_utf8_lossy(proto).trim());
    let first = collect(&mut s, 3);
    show("after login", &first);
    if proto != b"KVMR" && std::env::var_os("BOBCAT_REDIR_SEND").is_none() {
        return Ok(());
    }
    let msg: Vec<u8> = match std::env::var("BOBCAT_REDIR_SEND") {
        Ok(h) => (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect(),
        Err(_) => KVM_START.to_vec(),
    };
    println!("sending {} bytes: {}", msg.len(), hex(&msg));
    send(&mut s, &msg)?;
    let mut got = collect(&mut s, 4);
    show("answer", &got);
    if let Some(at) = got.windows(4).position(|w| w == b"RFB ") {
        println!("RFB version from the device: {:?}", String::from_utf8_lossy(&got[at..(at + 12).min(got.len())]));
        send(&mut s, b"RFB 003.008\n")?;
        got = collect(&mut s, 4);
        show("after our version", &got);
    } else {
        send(&mut s, b"RFB 003.008\n")?;
        got = collect(&mut s, 4);
        show("after sending RFB 003.008", &got);
    }
    Ok(())
}

/// Bridge the AMT KVM to a local TCP port so a VNC viewer can use it. The session is opened and
/// taken through the device's RFB opening now (so errors show before any viewer starts):
///   client `40 00 00 00 00 00 00 00` -> device `41 00 00 00 00 00 00 00` + `RFB 004.000\n`;
///   client `RFB 003.008\n` -> device security types (`02 01 80`: None and an Intel type);
///   client picks 1 (None; the redirection login already authenticated) -> SecurityResult.
/// The first local connection is then given a plain RFB 3.8 opening (version, "None", result) and
/// joined to the device, which waits for ClientInit.
pub fn kvm_bridge(ip: &str, port: u16, user: &str, pass: &str) -> Result<u16, String> {
    let mut amt = open(ip, port, b"KVMR", user, pass)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    send(&mut amt, &KVM_START)?;
    let reply = read_exact(&mut amt, 8)?;
    if reply[0] != 0x41 || reply[1] != 0 {
        return Err(format!("the device did not accept the KVM start (answer {})", hex(&reply)));
    }
    let ver = read_exact(&mut amt, 12)?;
    if !ver.starts_with(b"RFB ") {
        return Err(format!("the KVM channel did not offer an RFB version (got {:?})", String::from_utf8_lossy(&ver)));
    }
    send(&mut amt, b"RFB 003.008\n")?;
    let n = read_exact(&mut amt, 1)?[0] as usize;
    let types = read_exact(&mut amt, n)?;
    if !types.contains(&1) {
        return Err(format!("the KVM offers no 'None' security type (types {types:?})"));
    }
    send(&mut amt, &[1])?;
    let result = read_exact(&mut amt, 4)?;
    if result != [0, 0, 0, 0] {
        return Err("the KVM refused the connection (it may be in use by another viewer or need user consent)".into());
    }
    thread::spawn(move || {
        let Ok((client, _)) = listener.accept() else { return };
        if let Err(e) = serve_viewer(client, amt) {
            if debug() {
                eprintln!("[bridge] {e}");
            }
        }
    });
    Ok(local)
}

/// An RFB pixel format (the 16 bytes of ServerInit / SetPixelFormat).
#[derive(Clone, Copy, Debug)]
struct Pf {
    bpp: u8,
    big: bool,
    true_colour: bool,
    max: [u32; 3],
    shift: [u32; 3],
}

impl Pf {
    fn parse(b: &[u8]) -> Pf {
        Pf {
            bpp: b[0],
            big: b[2] != 0,
            true_colour: b[3] != 0,
            max: [u16::from_be_bytes([b[4], b[5]]) as u32, u16::from_be_bytes([b[6], b[7]]) as u32, u16::from_be_bytes([b[8], b[9]]) as u32],
            shift: [b[10] as u32, b[11] as u32, b[12] as u32],
        }
    }
    fn bytes(&self) -> usize {
        self.bpp as usize / 8
    }
    fn read(&self, p: &[u8]) -> u32 {
        let mut v = 0u32;
        if self.big {
            for &b in &p[..self.bytes()] {
                v = (v << 8) | b as u32;
            }
        } else {
            for &b in p[..self.bytes()].iter().rev() {
                v = (v << 8) | b as u32;
            }
        }
        v
    }
    fn write(&self, v: u32, out: &mut Vec<u8>) {
        let n = self.bytes();
        for i in 0..n {
            let k = if self.big { n - 1 - i } else { i };
            out.push((v >> (8 * k)) as u8);
        }
    }
}

/// Convert Raw pixels from `src` to `dst` format (both true colour).
fn convert(raw: &[u8], src: &Pf, dst: &Pf) -> Vec<u8> {
    let (sn, dn) = (src.bytes(), dst.bytes());
    let mut out = Vec::with_capacity(raw.len() / sn * dn);
    for px in raw.chunks_exact(sn) {
        let v = src.read(px);
        let mut o = 0u32;
        for c in 0..3 {
            let ch = (v >> src.shift[c]) & src.max[c];
            let scaled = if src.max[c] == 0 { 0 } else { ch * dst.max[c] / src.max[c] };
            o |= scaled << dst.shift[c];
        }
        dst.write(o, &mut out);
    }
    out
}

/// Inflate one chunk of the device's continuing raw-deflate stream (sync-flushed per rectangle).
fn inflate_chunk(d: &mut flate2::Decompress, input: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::{FlushDecompress, Status};
    let mut out = Vec::with_capacity(16384);
    let start = d.total_in();
    loop {
        let used = (d.total_in() - start) as usize;
        if used >= input.len() {
            break;
        }
        let (before_in, before_out) = (d.total_in(), d.total_out());
        let status = d.decompress_vec(&input[used..], &mut out, FlushDecompress::Sync).map_err(|e| format!("bad deflate data from the device: {e}"))?;
        if status == Status::StreamEnd {
            break;
        }
        if d.total_in() == before_in && d.total_out() == before_out {
            if out.len() == out.capacity() {
                out.reserve(16384);
            } else {
                break;
            }
        } else if out.len() == out.capacity() {
            out.reserve(16384);
        }
    }
    Ok(out)
}

/// Decode one Intel RLE tile (the data inside encoding 16, after the deflate step) into raw pixels
/// in the device's pixel format (`bpp` bytes per pixel, little endian, `w` x `h`). Sub-encodings, as
/// MeshCommander decodes them: 0 raw, 1 solid, 2..16 packed palette, 128 run-length, >129 palette
/// run-length.
fn rle_tile(data: &[u8], w: usize, h: usize, bpp: usize) -> Result<Vec<u8>, String> {
    let s = w * h;
    let mut out = vec![0u8; s * bpp];
    let mut p = 0usize;
    let mut count = 0usize;
    let sub = *data.first().ok_or("empty RLE tile")?;
    p += 1;
    let color = |p: &mut usize| -> Result<&[u8], String> {
        let c = data.get(*p..*p + bpp).ok_or("short RLE tile")?;
        *p += bpp;
        Ok(c)
    };
    let run_len = |p: &mut usize| -> Result<usize, String> {
        let mut run = 1usize;
        loop {
            let b = *data.get(*p).ok_or("short RLE run")?;
            *p += 1;
            run += b as usize;
            if b != 255 {
                return Ok(run);
            }
        }
    };
    let put = |out: &mut Vec<u8>, at: usize, c: &[u8], n: usize| {
        for i in 0..n {
            let k = at + i;
            if k < s {
                out[k * bpp..(k + 1) * bpp].copy_from_slice(c);
            }
        }
    };
    match sub {
        0 => {
            let raw = data.get(p..p + s * bpp).ok_or("short raw RLE tile")?;
            out.copy_from_slice(raw);
        }
        1 => {
            let c = color(&mut p)?.to_vec();
            put(&mut out, 0, &c, s);
        }
        2..=16 => {
            let n = sub as usize;
            let mut palette = Vec::new();
            for _ in 0..n {
                palette.push(color(&mut p)?.to_vec());
            }
            let (br, bm) = if sub == 2 { (1u32, 1u8) } else if sub <= 4 { (2, 3) } else { (4, 15) };
            while count < s && p < data.len() {
                let v = data[p];
                p += 1;
                let mut i = 8 - br as i32;
                while i >= 0 {
                    if count < s {
                        let c = palette.get(((v >> i) & bm) as usize).ok_or("bad palette index")?;
                        put(&mut out, count, c, 1);
                    }
                    count += 1;
                    i -= br as i32;
                }
            }
        }
        128 => {
            while count < s && p < data.len() {
                let c = color(&mut p)?.to_vec();
                let run = run_len(&mut p)?;
                put(&mut out, count, &c, run);
                count += run;
            }
        }
        n if n > 129 => {
            let size = n as usize - 128;
            let mut palette = Vec::new();
            for _ in 0..size {
                palette.push(color(&mut p)?.to_vec());
            }
            while count < s && p < data.len() {
                let index = data[p];
                p += 1;
                let c = palette.get(index as usize % 128).ok_or("bad palette index")?.clone();
                let run = if index > 127 { run_len(&mut p)? } else { 1 };
                put(&mut out, count, &c, run);
                count += run;
            }
        }
        n => return Err(format!("unknown RLE tile type {n}")),
    }
    Ok(out)
}

/// Serve one viewer: the AMT KVM speaks RFB with 16-bit colour and a few encodings only, so the
/// viewer's SetPixelFormat / SetEncodings are not passed on. The device is asked for Raw, and
/// every rectangle it sends is converted to the pixel format the viewer asked for.
fn serve_viewer(mut client: TcpStream, mut amt: TcpStream) -> Result<(), String> {
    use std::sync::{Arc, Mutex};
    client.set_nodelay(true).ok();
    // an ordinary RFB 3.8 opening for the viewer: version, one security type (None), result OK
    let mut cv = [0u8; 12];
    client.write_all(b"RFB 003.008\n").map_err(|e| e.to_string())?;
    client.read_exact(&mut cv).map_err(|e| e.to_string())?;
    let mut choice = [0u8; 1];
    client.write_all(&[1, 1]).map_err(|e| e.to_string())?;
    client.read_exact(&mut choice).map_err(|e| e.to_string())?;
    if choice[0] != 1 {
        return Err("the viewer did not choose 'None'".into());
    }
    client.write_all(&[0, 0, 0, 0]).map_err(|e| e.to_string())?;
    // ClientInit -> device, ServerInit <- device
    let mut ci = [0u8; 1];
    client.read_exact(&mut ci).map_err(|e| e.to_string())?;
    amt.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let _ = ci; // MeshCommander always sends the shared flag as 1 (the viewer's own choice is not passed on)
    send(&mut amt, &[1])?;
    let si = read_exact(&mut amt, 24)?;
    let name_len = u32::from_be_bytes([si[20], si[21], si[22], si[23]]) as usize;
    let name = read_exact(&mut amt, name_len)?;
    let native = Pf::parse(&si[4..20]);
    let screen = (u16::from_be_bytes([si[0], si[1]]) as usize, u16::from_be_bytes([si[2], si[3]]) as usize);
    if debug() {
        eprintln!("[bridge] device screen {}x{}, pixel format {native:?}", u16::from_be_bytes([si[0], si[1]]), u16::from_be_bytes([si[2], si[3]]));
    }
    if !native.true_colour || ![8usize, 16, 32].contains(&(native.bpp as usize)) {
        return Err("the device uses a pixel format the bridge cannot convert".into());
    }
    // AMT cannot handle a display buffer over 9 216 000 bytes (bytes per pixel x width x height; this
    // limit is from MeshCommander's code): bigger screens are switched to 8-bit RGB332
    let force8 = std::env::var("BOBCAT_AMT_BPP").is_ok_and(|v| v == "1");
    let src = if force8 || screen.0 * screen.1 * native.bytes() > 9_216_000 {
        let p8 = Pf { bpp: 8, big: false, true_colour: true, max: [7, 7, 3], shift: [5, 2, 0] };
        let mut m = vec![0u8, 0, 0, 0, 8, 8, 0, 1];
        m.extend_from_slice(&[0, 7, 0, 7, 0, 3, 5, 2, 0, 0, 0, 0]);
        send(&mut amt, &m)?;
        if debug() {
            eprintln!("[bridge] {}x{} is too big for 16-bit colour on the device: using 8-bit RGB332", screen.0, screen.1);
        }
        p8
    } else {
        native
    };
    let mut hello = si.clone();
    hello.extend_from_slice(&name);
    client.write_all(&hello).map_err(|e| e.to_string())?;
    // ask the device for Raw only (BOBCAT_AMT_ENC=16,0 asks for other encodings, to find what it sends)
    let encodings: Vec<i32> = std::env::var("BOBCAT_AMT_ENC")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .filter(|v: &Vec<i32>| !v.is_empty())
        .unwrap_or_else(|| vec![16, 0, 1092, -223]); // MeshCommander's list: RLE, Raw, 1092, desktop size
    let mut se = vec![2u8, 0];
    se.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in &encodings {
        se.extend_from_slice(&e.to_be_bytes());
    }
    send(&mut amt, &se)?;
    amt.set_read_timeout(None).ok();
    // BOBCAT_AMT_WAKE=1: press and release Shift on the device a few times (to wake a blanked display)
    if std::env::var_os("BOBCAT_AMT_WAKE").is_some() {
        if let Ok(mut w) = amt.try_clone() {
            thread::spawn(move || {
                for round in 1..=3 {
                    thread::sleep(Duration::from_secs(2));
                    for down in [1u8, 0] {
                        let msg = [4u8, down, 0, 0, 0, 0, 0xff, 0xe1]; // KeyEvent, Shift_L
                        if debug() {
                            eprintln!("[bridge] {:>6} ms wake {round}: Shift {}", ms(), if down == 1 { "down" } else { "up" });
                        }
                        if w.write_all(&msg).is_err() {
                            return;
                        }
                        thread::sleep(Duration::from_millis(80));
                    }
                }
            });
        }
    }
    // the AMT KVM keep-alive MeshCommander sends every 5 s: a ClientCutText holding `\0KvmDataChannel\0`
    if let Ok(mut w) = amt.try_clone() {
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            let mut m = vec![6u8, 0, 0, 0, 0, 0, 0, 16];
            m.extend_from_slice(b"\0KvmDataChannel\0");
            if w.write_all(&m).is_err() {
                return;
            }
        });
    }
    // BOBCAT_AMT_POLL=<seconds>: ask the device for the whole screen again every so often
    if let Some(secs) = std::env::var("BOBCAT_AMT_POLL").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0) {
        if let Ok(mut w) = amt.try_clone() {
            let (sw, sh) = (screen.0 as u16, screen.1 as u16);
            thread::spawn(move || loop {
                thread::sleep(Duration::from_secs(secs));
                let mut m = vec![3u8, 0, 0, 0, 0, 0];
                m.extend_from_slice(&sw.to_be_bytes());
                m.extend_from_slice(&sh.to_be_bytes());
                if debug() {
                    eprintln!("[bridge] {:>6} ms poll: full-screen update request", ms());
                }
                if w.write_all(&m).is_err() {
                    return;
                }
            });
        }
    }
    let target = Arc::new(Mutex::new(src));

    let viewer_ds = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let viewer_ds2 = viewer_ds.clone();
    let mut refresh = amt.try_clone().map_err(|e| e.to_string())?;
    let (mut a_r, mut c_w, t2) = (amt.try_clone().map_err(|e| e.to_string())?, client.try_clone().map_err(|e| e.to_string())?, target.clone());
    // BOBCAT_AMT_DUMP=<file.ppm>: keep a copy of the screen and write it after every update
    let dump_path = std::env::var("BOBCAT_AMT_DUMP").ok();
    let down = thread::spawn(move || {
        let r = (move || -> Result<(), String> {
        let mut screen = screen;
        let mut inflater = flate2::Decompress::new(false);
        let mut zlib_header_seen = false;
        let mut fb = vec![0u8; screen.0 * screen.1 * 3];
        let mut rect_log = 0;
        let mut rle_log = 0;
        let mut msgs = 0;
        loop {
            let mut ty = [0u8; 1];
            // wait for the next message in 5 s steps so a silent device shows up in the trace
            a_r.set_read_timeout(Some(Duration::from_secs(5))).ok();
            loop {
                match a_r.read_exact(&mut ty) {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                        if debug() {
                            eprintln!("[bridge] {:>6} ms: nothing from the device yet", ms());
                        }
                    }
                    Err(e) => return Err(format!("the device closed the KVM channel ({e})")),
                }
            }
            a_r.set_read_timeout(None).ok();
            if debug() && msgs < 40 {
                msgs += 1;
                eprintln!("[bridge] {:>6} ms device -> viewer: message type {}", ms(), ty[0]);
            }
            match ty[0] {
                0 => {
                    let mut h = [0u8; 3];
                    a_r.read_exact(&mut h).map_err(|e| e.to_string())?;
                    let n = u16::from_be_bytes([h[1], h[2]]) as usize;
                    let mut out = vec![0u8, 0, h[1], h[2]];
                    let dst = *t2.lock().unwrap();
                    let mut shown = 0;
                    let mut emitted = 0u16;
                    for _ in 0..n {
                        let mut r = [0u8; 12];
                        a_r.read_exact(&mut r).map_err(|e| e.to_string())?;
                        let (rx, ry) = (u16::from_be_bytes([r[0], r[1]]) as usize, u16::from_be_bytes([r[2], r[3]]) as usize);
                        let (w, hh) = (u16::from_be_bytes([r[4], r[5]]) as usize, u16::from_be_bytes([r[6], r[7]]) as usize);
                        let enc = i32::from_be_bytes([r[8], r[9], r[10], r[11]]);
                        if debug() && rect_log < 3 && shown < 8 {
                            shown += 1;
                            eprintln!("[bridge]   rect at {rx},{ry} size {w}x{hh} encoding {enc}");
                        }
                        if enc == -223 {
                            // the device's screen changed size: tell the viewer (if it can take it), refresh
                            if debug() {
                                eprintln!("[bridge] the device's screen is now {w}x{hh}");
                            }
                            if !viewer_ds2.load(std::sync::atomic::Ordering::SeqCst) {
                                return Err("the device's screen changed size and the viewer cannot resize (reconnect it)".into());
                            }
                            screen = (w, hh);
                            fb = vec![0u8; w * hh * 3];
                            out.extend_from_slice(&r);
                            emitted += 1;
                            let mut m = vec![3u8, 0, 0, 0, 0, 0];
                            m.extend_from_slice(&(w as u16).to_be_bytes());
                            m.extend_from_slice(&(hh as u16).to_be_bytes());
                            refresh.write_all(&m).map_err(|e| e.to_string())?;
                            continue;
                        }
                        if rx + w > screen.0 || ry + hh > screen.1 {
                            eprintln!("[bridge] warning: rectangle {rx},{ry} {w}x{hh} lies outside the {}x{} screen", screen.0, screen.1);
                        }
                        let raw = match enc {
                            0 => {
                                let mut raw = vec![0u8; w * hh * src.bytes()];
                                a_r.read_exact(&mut raw).map_err(|e| e.to_string())?;
                                raw
                            }
                            16 => {
                                // Intel RLE: 32-bit length, then an uncompressed block (5-byte header) or deflate data
                                let mut l = [0u8; 4];
                                a_r.read_exact(&mut l).map_err(|e| e.to_string())?;
                                let len = u32::from_be_bytes(l) as usize;
                                if len > 1 << 20 || w == 0 || hh == 0 || w > 64 || hh > 64 {
                                    return Err(format!("the device sent an RLE tile of {w}x{hh} with {len} bytes, which is not valid"));
                                }
                                let mut data = vec![0u8; len];
                                a_r.set_read_timeout(Some(Duration::from_secs(10))).ok();
                                a_r.read_exact(&mut data).map_err(|e| format!("reading an RLE tile of {len} bytes: {e}"))?;
                                let stored = len > 5 && data[0] == 0 && u16::from_le_bytes([data[1], data[2]]) as usize == len - 5;
                                if debug() && rle_log < 6 {
                                    eprintln!("[bridge]   RLE tile {w}x{hh} at {rx},{ry}: {len} bytes ({}), first bytes {}", if stored { "stored" } else { "deflated" }, hex(&data[..data.len().min(40)]));
                                }
                                // the first compressed chunk of the stream starts with a zlib header (78 9c ...)
                                let mut body: &[u8] = &data;
                                if !stored && !zlib_header_seen && body.len() > 2 && body[0] == 0x78 && ((body[0] as u16) << 8 | body[1] as u16) % 31 == 0 {
                                    body = &body[2..];
                                    zlib_header_seen = true;
                                }
                                let payload = if stored {
                                    data[5..].to_vec()
                                } else {
                                    inflate_chunk(&mut inflater, body).map_err(|e| format!("{e} (tile {w}x{hh} at {rx},{ry}, {len} bytes: {})", hex(&data[..data.len().min(48)])))?
                                };
                                if debug() && rle_log < 6 {
                                    rle_log += 1;
                                    eprintln!("[bridge]   RLE tile {w}x{hh}: {len} bytes ({}), data {} -> {} bytes, tile type {}", if stored { "stored" } else { "deflated" }, hex(&data[..data.len().min(12)]), payload.len(), payload.first().map(|b| b.to_string()).unwrap_or_default());
                                }
                                rle_tile(&payload, w, hh, src.bytes()).map_err(|e| format!("{e} (tile {w}x{hh} at {rx},{ry}, {len} bytes, first bytes {})", hex(&payload[..payload.len().min(16)])))?
                            }
                            e => {
                                // show what the device sent, to work out the encoding
                                a_r.set_read_timeout(Some(Duration::from_secs(2))).ok();
                                let mut peek = [0u8; 48];
                                let got = a_r.read(&mut peek).unwrap_or(0);
                                eprintln!("[bridge] rect {w}x{hh} at {rx},{ry} with encoding {e}; next {got} bytes: {}", hex(&peek[..got]));
                                return Err(format!("the device sent encoding {e}, which the bridge cannot convert yet"));
                            }
                        };
                        let mut hdr = r;
                        hdr[8..12].copy_from_slice(&0i32.to_be_bytes()); // the viewer gets Raw
                        out.extend_from_slice(&hdr);
                        out.extend_from_slice(&convert(&raw, &src, &dst));
                        emitted += 1;
                        if dump_path.is_some() {
                            for (i, px) in raw.chunks_exact(src.bytes()).enumerate() {
                                let v = src.read(px);
                                let (cx, cy) = (rx + i % w, ry + i / w);
                                if cx < screen.0 && cy < screen.1 {
                                    for c in 0..3 {
                                        fb[(cy * screen.0 + cx) * 3 + c] = (((v >> src.shift[c]) & src.max[c]) * 255 / src.max[c].max(1)) as u8;
                                    }
                                }
                            }
                        }
                    }
                    out[2..4].copy_from_slice(&emitted.to_be_bytes());
                    if debug() && rect_log < 3 {
                        rect_log += 1;
                        eprintln!("[bridge] update with {n} rectangle(s), {} bytes to the viewer", out.len());
                    }
                    c_w.write_all(&out).map_err(|e| e.to_string())?;
                    if let Some(p) = &dump_path {
                        let mut ppm = format!("P6\n{} {}\n255\n", screen.0, screen.1).into_bytes();
                        ppm.extend_from_slice(&fb);
                        let _ = std::fs::write(p, ppm);
                        let lit = fb.iter().filter(|b| **b != 0).count();
                        eprintln!("[bridge] screen saved to {p} ({lit} of {} colour bytes are not black)", fb.len());
                    }
                }
                2 => c_w.write_all(&[2]).map_err(|e| e.to_string())?, // Bell
                3 => {
                    let mut h = [0u8; 7];
                    a_r.read_exact(&mut h).map_err(|e| e.to_string())?;
                    let len = u32::from_be_bytes([h[3], h[4], h[5], h[6]]) as usize;
                    let mut text = vec![0u8; len];
                    a_r.read_exact(&mut text).map_err(|e| e.to_string())?;
                    if text.starts_with(b"\0Kvm") {
                        continue; // the AMT KVM's own data channel / extension messages, not clipboard text
                    }
                    let mut out = vec![3u8];
                    out.extend_from_slice(&h);
                    out.extend_from_slice(&text);
                    c_w.write_all(&out).map_err(|e| e.to_string())?;
                }
                t => return Err(format!("the device sent message type {t}, which the bridge does not know")),
            }
        }
        })();
        if let Err(e) = &r {
            eprintln!("[bridge] device side stopped: {e}");
        }
        r
    });

    // viewer -> device: pixel format and encodings are dealt with here, the rest passes through
    let mut a_w = amt;
    let mut c_r = client;
    let mut traced = 0;
    let up: Result<(), String> = (|| loop {
        let mut ty = [0u8; 1];
        c_r.read_exact(&mut ty).map_err(|_| "the viewer closed the connection".to_string())?;
        let mut msg = vec![ty[0]];
        let more = |c: &mut TcpStream, msg: &mut Vec<u8>, n: usize| -> Result<(), String> {
            let mut b = vec![0u8; n];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            msg.extend_from_slice(&b);
            Ok(())
        };
        match ty[0] {
            0 => {
                more(&mut c_r, &mut msg, 19)?;
                let want = Pf::parse(&msg[4..20]);
                if debug() {
                    eprintln!("[bridge] the viewer wants pixel format {want:?}");
                }
                if !want.true_colour {
                    return Err("the viewer asked for a colour-mapped pixel format".into());
                }
                *target.lock().unwrap() = want;
            }
            2 => {
                more(&mut c_r, &mut msg, 3)?;
                let n = u16::from_be_bytes([msg[2], msg[3]]) as usize;
                more(&mut c_r, &mut msg, n * 4)?; // not passed on: the device gets its own list
                let has_ds = msg[4..].chunks_exact(4).any(|e| i32::from_be_bytes([e[0], e[1], e[2], e[3]]) == -223);
                viewer_ds.store(has_ds, std::sync::atomic::Ordering::SeqCst);
            }
            3 => {
                more(&mut c_r, &mut msg, 9)?;
                a_w.write_all(&msg).map_err(|e| e.to_string())?;
            }
            4 => {
                more(&mut c_r, &mut msg, 7)?;
                a_w.write_all(&msg).map_err(|e| e.to_string())?;
            }
            5 => {
                more(&mut c_r, &mut msg, 5)?;
                a_w.write_all(&msg).map_err(|e| e.to_string())?;
            }
            6 => {
                more(&mut c_r, &mut msg, 7)?;
                let len = u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]) as usize;
                more(&mut c_r, &mut msg, len)?;
                a_w.write_all(&msg).map_err(|e| e.to_string())?;
            }
            t => return Err(format!("the viewer sent message type {t}, which the bridge does not know")),
        }
        if debug() && traced < 40 {
            traced += 1;
            eprintln!("[bridge] {:>6} ms viewer -> device: type {} ({} bytes) {}", ms(), msg[0], msg.len(), hex(&msg[..msg.len().min(14)]));
        }
    })();
    let _ = a_w.shutdown(std::net::Shutdown::Both);
    let down = down.join().map_err(|_| "bridge thread failed".to_string())?;
    up.and(down)
}
