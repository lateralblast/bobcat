//! Apple Remote Desktop (macOS Screen Sharing / Remote Management), the GUI protocol "ARD".
//!
//! A Mac's VNC server speaks RFB 3.889 and offers Apple's own login, security type 30, which the
//! `vnc` crate does not know. This module does that login itself and then bridges the stream: it
//! listens on a local port, gives the first connection a plain RFB 3.8 opening ("None" security,
//! the login is already done) and relays everything else between that connection and the Mac, so
//! the normal VNC canvas code can show it (same idea as the AMT KVM bridge in `amt_redir.rs`).
//!
//! Security type 30 (checked against LibVNC's `HandleARDAuth`):
//!   server: generator (u16), key length n (u16), prime (n bytes), server public key (n bytes)
//!   client: picks a random private key, public = g^priv mod p, shared = serverpub^priv mod p
//!           (zero-padded to n bytes), AES key = MD5(shared) (16 bytes),
//!           plaintext = user name (64 bytes) + password (64 bytes), each NUL-terminated with the
//!           rest random, encrypted with AES-128-ECB, no padding
//!   client sends the 128 ciphertext bytes, then its public key (n bytes); the server answers with
//!   a SecurityResult (u32, 0 = ok; in RFB 3.8 a failure is followed by a reason string).
//! Other Apple types (33, 35, 36) are not implemented; a server that offers only those, or a Mac
//! with a plain VNC password, can be reached with the VNC protocol instead.

use openssl::bn::{BigNum, BigNumContext};
use openssl::hash::{hash, MessageDigest};
use openssl::rand::rand_bytes;
use openssl::symm::{Cipher, Crypter, Mode};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

/// Apple Remote Desktop's security type.
const ARD: u8 = 30;

fn debug() -> bool {
    std::env::var("BOBCAT_DEBUG").is_ok()
}

fn read_exact(s: &mut TcpStream, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).map_err(|e| format!("reading from the Mac failed: {e}"))?;
    Ok(buf)
}

fn send(s: &mut TcpStream, data: &[u8]) -> Result<(), String> {
    s.write_all(data).map_err(|e| format!("writing to the Mac failed: {e}"))
}

/// A big number as exactly `len` big-endian bytes (left-padded with zeros).
fn padded(n: &BigNum, len: usize) -> Result<Vec<u8>, String> {
    n.to_vec_padded(len as i32).map_err(|e| format!("number too large for the key length: {e}"))
}

/// The 128-byte plaintext: user name (64) + password (64), NUL-terminated, the rest random.
fn credentials(user: &str, pass: &str) -> Result<[u8; 128], String> {
    let mut block = [0u8; 128];
    rand_bytes(&mut block).map_err(|e| e.to_string())?;
    for (offset, text) in [(0usize, user), (64, pass)] {
        let bytes = text.as_bytes();
        let n = bytes.len().min(63);
        block[offset..offset + n].copy_from_slice(&bytes[..n]);
        block[offset + n] = 0;
    }
    Ok(block)
}

fn aes128_ecb(mode: Mode, key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Cipher::aes_128_ecb();
    let mut c = Crypter::new(cipher, mode, key, None).map_err(|e| e.to_string())?;
    c.pad(false);
    let mut out = vec![0u8; data.len() + cipher.block_size()];
    let mut n = c.update(data, &mut out).map_err(|e| e.to_string())?;
    n += c.finalize(&mut out[n..]).map_err(|e| e.to_string())?;
    out.truncate(n);
    Ok(out)
}

/// The client side of security type 30 after the type byte was sent: everything up to (not
/// including) the SecurityResult.
fn ard_login(s: &mut TcpStream, user: &str, pass: &str) -> Result<(), String> {
    let head = read_exact(s, 4)?;
    let generator = BigNum::from_slice(&head[..2]).map_err(|e| e.to_string())?;
    let keylen = u16::from_be_bytes([head[2], head[3]]) as usize;
    if keylen == 0 || keylen > 1024 {
        return Err(format!("the Mac sent an odd Diffie-Hellman key length ({keylen})"));
    }
    let prime = BigNum::from_slice(&read_exact(s, keylen)?).map_err(|e| e.to_string())?;
    let server_pub = BigNum::from_slice(&read_exact(s, keylen)?).map_err(|e| e.to_string())?;
    let mut ctx = BigNumContext::new().map_err(|e| e.to_string())?;
    // private key: keylen random bytes with the top bit cleared, so it is below the prime
    let mut raw = vec![0u8; keylen];
    rand_bytes(&mut raw).map_err(|e| e.to_string())?;
    raw[0] &= 0x7f;
    let private = BigNum::from_slice(&raw).map_err(|e| e.to_string())?;
    let mut public = BigNum::new().map_err(|e| e.to_string())?;
    public.mod_exp(&generator, &private, &prime, &mut ctx).map_err(|e| e.to_string())?;
    let mut shared = BigNum::new().map_err(|e| e.to_string())?;
    shared.mod_exp(&server_pub, &private, &prime, &mut ctx).map_err(|e| e.to_string())?;
    let key = hash(MessageDigest::md5(), &padded(&shared, keylen)?).map_err(|e| e.to_string())?;
    let encrypted = aes128_ecb(Mode::Encrypt, &key, &credentials(user, pass)?)?;
    send(s, &encrypted)?;
    send(s, &padded(&public, keylen)?)
}

/// The SecurityResult after the login. Zero is success; Apple's servers have been seen sending
/// the failure value in either byte order, so anything non-zero is a refusal.
fn security_result(s: &mut TcpStream) -> Result<(), String> {
    let r = read_exact(s, 4)?;
    if r == [0, 0, 0, 0] {
        return Ok(());
    }
    // RFB 3.8: a reason string (u32 length, text) follows a failure
    let reason = read_exact(s, 4)
        .ok()
        .map(|l| u32::from_be_bytes([l[0], l[1], l[2], l[3]]) as usize)
        .filter(|n| *n > 0 && *n < 1000)
        .and_then(|n| read_exact(s, n).ok())
        .map(|t| String::from_utf8_lossy(&t).to_string());
    Err(match reason {
        Some(t) => format!("the Mac refused the login: {t} (check the user name and password; on newer macOS the user may have to be added by name under Sharing > Screen Sharing > Allow access for, even if All users is set)"),
        None => "the Mac refused the login (wrong user name or password, or that user may not use Screen Sharing; add the user by name under Allow access for)".into(),
    })
}

/// Connect to the Mac, log in with Apple's ARD authentication and return the Mac's stream, ready for
/// ClientInit.
fn open(ip: &str, port: u16, user: &str, pass: &str) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addr = (ip, port).to_socket_addrs().map_err(|e| format!("{ip}: {e}"))?.next().ok_or(format!("{ip}: no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("connecting to {ip}:{port} failed: {e} (is Screen Sharing or Remote Management on?)"))?;
    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
    s.set_write_timeout(Some(Duration::from_secs(20))).ok();
    s.set_nodelay(true).ok();
    let ver = read_exact(&mut s, 12)?;
    if !ver.starts_with(b"RFB ") {
        return Err(format!("{ip}:{port} is not a VNC server (it sent {:?})", String::from_utf8_lossy(&ver)));
    }
    if debug() {
        eprintln!("[ard] server version {:?}", String::from_utf8_lossy(&ver));
    }
    // Apple says 3.889; every client answers 3.8
    send(&mut s, b"RFB 003.008\n")?;
    let n = read_exact(&mut s, 1)?[0] as usize;
    if n == 0 {
        let len = read_exact(&mut s, 4)?;
        let text = read_exact(&mut s, u32::from_be_bytes([len[0], len[1], len[2], len[3]]).min(1000) as usize).unwrap_or_default();
        return Err(format!("the Mac refused the connection: {}", String::from_utf8_lossy(&text)));
    }
    let types = read_exact(&mut s, n)?;
    if debug() {
        eprintln!("[ard] security types {types:?}");
    }
    if !types.contains(&ARD) {
        return Err(format!(
            "the server offers security types {types:?}, not Apple's login (30); for a Mac with a plain VNC password use the VNC protocol (port 5900, TLS off)"
        ));
    }
    send(&mut s, &[ARD])?;
    ard_login(&mut s, user, pass)?;
    security_result(&mut s)?;
    s.set_read_timeout(None).ok();
    s.set_write_timeout(None).ok();
    Ok(s)
}

/// Log in to the Mac and bridge its screen to a local port (returned) for the VNC canvas. The
/// bridge serves one viewer.
pub fn bridge(ip: &str, port: u16, user: &str, pass: &str) -> Result<u16, String> {
    let mac = open(ip, port, user, pass)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        let Ok((client, _)) = listener.accept() else { return };
        if let Err(e) = serve_viewer(client, mac) {
            if debug() {
                eprintln!("[ard] {e}");
            }
        }
    });
    Ok(local)
}

/// Give the local viewer a plain RFB 3.8 opening (version, "None", ok), then relay both ways.
fn serve_viewer(mut client: TcpStream, mac: TcpStream) -> Result<(), String> {
    client.set_nodelay(true).ok();
    send(&mut client, b"RFB 003.008\n")?;
    read_exact(&mut client, 12)?; // the viewer's version
    send(&mut client, &[1, 1])?; // one security type: None
    read_exact(&mut client, 1)?; // its choice
    send(&mut client, &[0, 0, 0, 0])?; // SecurityResult: ok
    relay(client, mac);
    Ok(())
}

/// One client-to-server RFB message, read whole. The `vnc` crate writes each field with a separate
/// write, so a pointer event arrives as four tiny reads; forwarding them one by one would send four
/// packets to the Mac for every mouse move.
#[derive(Debug, PartialEq)]
enum ClientMsg {
    Whole(Vec<u8>),
    /// A message type this does not know (only its first byte was read): forward the rest unparsed.
    Unknown(u8),
}

fn take<R: Read>(r: &mut R, msg: &mut Vec<u8>, n: usize) -> std::io::Result<()> {
    let at = msg.len();
    msg.resize(at + n, 0);
    r.read_exact(&mut msg[at..])
}

fn read_client_message<R: Read>(r: &mut R) -> std::io::Result<ClientMsg> {
    let mut t = [0u8; 1];
    r.read_exact(&mut t)?;
    let mut m = vec![t[0]];
    match t[0] {
        0 => take(r, &mut m, 19)?, // SetPixelFormat
        2 => {
            // SetEncodings: padding, count, then count 32-bit encodings
            take(r, &mut m, 3)?;
            let n = u16::from_be_bytes([m[2], m[3]]) as usize;
            take(r, &mut m, 4 * n)?;
        }
        3 => take(r, &mut m, 9)?, // FramebufferUpdateRequest
        4 => take(r, &mut m, 7)?, // KeyEvent
        5 => take(r, &mut m, 5)?, // PointerEvent
        6 => {
            // ClientCutText: padding, length, text
            take(r, &mut m, 7)?;
            let n = u32::from_be_bytes([m[4], m[5], m[6], m[7]]) as usize;
            if n > 1 << 20 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "client cut text too long"));
            }
            take(r, &mut m, n)?;
        }
        other => return Ok(ClientMsg::Unknown(other)),
    }
    Ok(ClientMsg::Whole(m))
}

/// Copy both directions until either side closes, then close both. Towards the Mac each RFB message
/// is sent in one write (see `read_client_message`); the first byte after the security handshake is
/// ClientInit, which is not a numbered message.
fn relay(a: TcpStream, b: TcpStream) {
    let (Ok(mut a_in), Ok(mut b_in)) = (a.try_clone(), b.try_clone()) else { return };
    let (mut a_out, mut b_out) = (a, b);
    let up = thread::spawn(move || {
        let mut init = [0u8; 1];
        let mut raw = a_in.read_exact(&mut init).is_err() || b_out.write_all(&init).is_err();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            if raw {
                match a_in.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if b_out.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
                continue;
            }
            match read_client_message(&mut a_in) {
                Ok(ClientMsg::Whole(m)) => {
                    if b_out.write_all(&m).is_err() {
                        break;
                    }
                }
                Ok(ClientMsg::Unknown(t)) => {
                    if b_out.write_all(&[t]).is_err() {
                        break;
                    }
                    raw = true;
                }
                Err(_) => break,
            }
        }
        let _ = b_out.shutdown(Shutdown::Both);
        let _ = a_in.shutdown(Shutdown::Both);
    });
    // Mac -> viewer: big updates, so a big buffer
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match b_in.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if a_out.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = a_out.shutdown(Shutdown::Both);
    let _ = b_in.shutdown(Shutdown::Both);
    let _ = up.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in Mac: offers type 30, does the server side of the Diffie-Hellman exchange with the
    /// same primitives, decrypts the credentials and answers the SecurityResult. This checks that
    /// the client's steps agree with themselves (key agreement, MD5 key, AES-ECB layout); the real
    /// proof is a Mac.
    fn fake_mac(listener: TcpListener, want_user: &'static str, want_pass: &'static str) -> thread::JoinHandle<Result<Vec<u8>, String>> {
        thread::spawn(move || {
            let (mut s, _) = listener.accept().map_err(|e| e.to_string())?;
            send(&mut s, b"RFB 003.889\n")?;
            if read_exact(&mut s, 12)? != b"RFB 003.008\n" {
                return Err("client version".into());
            }
            send(&mut s, &[3, 33, ARD, 36])?;
            if read_exact(&mut s, 1)?[0] != ARD {
                return Err("client did not pick type 30".into());
            }
            let keylen = 128usize;
            // a 1024-bit safe-prime-ish modulus is not needed for the test: any odd 1024-bit number
            // with the top bit set works for modular exponentiation to agree
            let mut p = vec![0xffu8; keylen];
            p[keylen - 1] = 0xc5;
            let prime = BigNum::from_slice(&p).unwrap();
            let generator = BigNum::from_u32(2).unwrap();
            let mut ctx = BigNumContext::new().unwrap();
            let mut raw = vec![0u8; keylen];
            rand_bytes(&mut raw).unwrap();
            raw[0] &= 0x7f;
            let private = BigNum::from_slice(&raw).unwrap();
            let mut public = BigNum::new().unwrap();
            public.mod_exp(&generator, &private, &prime, &mut ctx).unwrap();
            let mut head = vec![0, 2, 0, keylen as u8];
            head.extend_from_slice(&padded(&prime, keylen)?);
            head.extend_from_slice(&padded(&public, keylen)?);
            send(&mut s, &head)?;
            let encrypted = read_exact(&mut s, 128)?;
            let client_pub = BigNum::from_slice(&read_exact(&mut s, keylen)?).unwrap();
            let mut shared = BigNum::new().unwrap();
            shared.mod_exp(&client_pub, &private, &prime, &mut ctx).unwrap();
            let key = hash(MessageDigest::md5(), &padded(&shared, keylen)?).unwrap();
            let plain = aes128_ecb(Mode::Decrypt, &key, &encrypted)?;
            let text = |b: &[u8]| String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).to_string();
            let ok = text(&plain[..64]) == want_user && text(&plain[64..]) == want_pass;
            if ok {
                send(&mut s, &[0, 0, 0, 0])?;
            } else {
                send(&mut s, &[0, 0, 0, 1])?;
                let msg = b"Authentication failed";
                send(&mut s, &(msg.len() as u32).to_be_bytes())?;
                send(&mut s, msg)?;
                return Ok(plain);
            }
            // after the login: wait for ClientInit and answer with a marker the test can see
            read_exact(&mut s, 1)?;
            send(&mut s, b"SERVERINIT")?;
            Ok(plain)
        })
    }

    #[test]
    fn login_and_relay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mac = fake_mac(listener, "richard", "s3cret pass");
        let local = bridge("127.0.0.1", port, "richard", "s3cret pass").unwrap();
        // act as the viewer: plain RFB 3.8 opening, then ClientInit and the relayed ServerInit
        let mut v = TcpStream::connect(("127.0.0.1", local)).unwrap();
        assert_eq!(read_exact(&mut v, 12).unwrap(), b"RFB 003.008\n");
        send(&mut v, b"RFB 003.008\n").unwrap();
        assert_eq!(read_exact(&mut v, 2).unwrap(), [1, 1]);
        send(&mut v, &[1]).unwrap();
        assert_eq!(read_exact(&mut v, 4).unwrap(), [0, 0, 0, 0]);
        send(&mut v, &[1]).unwrap(); // ClientInit (shared)
        assert_eq!(read_exact(&mut v, 10).unwrap(), b"SERVERINIT");
        let plain = mac.join().unwrap().unwrap();
        assert_eq!(plain.len(), 128);
    }

    #[test]
    fn wrong_password_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mac = fake_mac(listener, "richard", "right");
        let err = bridge("127.0.0.1", port, "richard", "wrong").unwrap_err();
        assert!(err.contains("refused the login") && err.contains("Authentication failed"), "{err}");
        mac.join().unwrap().unwrap();
    }

    #[test]
    fn client_messages_are_read_whole() {
        use std::io::Cursor;
        let mut bytes: Vec<u8> = Vec::new();
        let pointer = [5u8, 1, 0, 10, 0, 20];
        let update = [3u8, 1, 0, 0, 0, 0, 0x0a, 0x00, 0x06, 0x40];
        let encodings = [2u8, 0, 0, 2, 0, 0, 0, 16, 0xff, 0xff, 0xff, 0x21];
        let key = [4u8, 1, 0, 0, 0, 0, 0xff, 0x0d];
        let mut cut = vec![6u8, 0, 0, 0, 0, 0, 0, 3];
        cut.extend_from_slice(b"abc");
        let pixel_format = [0u8; 20];
        for m in [&pointer[..], &update[..], &encodings[..], &key[..], &cut[..], &pixel_format[..]] {
            bytes.extend_from_slice(m);
        }
        bytes.push(99); // not a message type this knows
        let mut r = Cursor::new(bytes);
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(pointer.to_vec()));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(update.to_vec()));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(encodings.to_vec()));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(key.to_vec()));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(cut));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Whole(pixel_format.to_vec()));
        assert_eq!(read_client_message(&mut r).unwrap(), ClientMsg::Unknown(99));
        assert!(read_client_message(&mut r).is_err()); // end of input
    }

    #[test]
    fn long_names_are_cut_and_terminated() {
        let block = credentials(&"u".repeat(100), &"p".repeat(100)).unwrap();
        assert_eq!(block[63], 0);
        assert_eq!(block[127], 0);
        assert!(block[..63].iter().all(|b| *b == b'u'));
        assert!(block[64..127].iter().all(|b| *b == b'p'));
    }

    #[test]
    fn server_without_type_30_is_explained() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            send(&mut s, b"RFB 003.008\n").unwrap();
            read_exact(&mut s, 12).unwrap();
            send(&mut s, &[1, 2]).unwrap(); // VNC password only
        });
        let err = bridge("127.0.0.1", port, "u", "p").unwrap_err();
        assert!(err.contains("not Apple's login") && err.contains("VNC protocol"), "{err}");
    }
}
