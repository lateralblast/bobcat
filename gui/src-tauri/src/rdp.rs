//! Remote Desktop Protocol (Windows Remote Desktop), the GUI protocol "RDP".
//!
//! IronRDP (pure Rust) does the protocol: TLS upgrade, CredSSP / NLA login (NTLM for a local
//! account), bitmap decoding. This module wires it to the page's canvas: the decoded picture goes
//! out as dirty rectangles on the frame channel (the same raw messages as VNC, see `put_rgba`),
//! and the page's key and pointer commands come back as RDP fast-path input. The TLS certificate
//! is pinned on first use like the other protocols (`check_pin`).
//!
//! Keys: a printable character is sent as a Unicode key event (independent of the server's
//! keyboard layout); anything else, and any key while Ctrl, Alt or the Windows key is down, is a
//! scancode (US layout positions).
//!
//! Desktop size: the page passes the size it wants (the window's canvas area when "Fit window" is
//! chosen), otherwise `BOBCAT_RDP_SIZE=WxH` or 1920x1080. While connected `Cmd::Resize` asks the
//! server for a new size over the Display Control channel; the server then runs the
//! deactivation-reactivation sequence, which `run` carries out and then replaces the picture.
//!
//! Clipboard (text only): the CLIPRDR channel. When the Windows side copies, its text is fetched
//! at once and put on the local clipboard; the local clipboard is polled (`CLIP_POLL`) and a new
//! text is announced to the server, which asks for it when something pastes.

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp::connector::sspi::network_client::NetworkClient;
use ironrdp::connector::sspi::generator::NetworkRequest;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use ironrdp::cliprdr::{Client as CliprdrClientRole, Cliprdr};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{self, ConnectionResult, Credentials};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::session::fast_path;
use ironrdp::session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp::connector::Sequence;
use ironrdp::core::WriteBuf;
use openssl::ssl::{SslConnector, SslMethod, SslSessionCacheMode, SslStream, SslVerifyMode};
use tauri::AppHandle;

use crate::{flush_frames, put_size, status, Cmd, FRAMES};

fn debug() -> bool {
    std::env::var("BOBCAT_DEBUG").is_ok()
}

/// CredSSP asks for a network client only for Kerberos (domain accounts through a KDC); a local
/// account uses NTLM, which needs none.
struct NoNetwork;

impl NetworkClient for NoNetwork {
    fn send(&self, _request: &NetworkRequest) -> ironrdp::connector::sspi::Result<Vec<u8>> {
        Err(ironrdp::connector::sspi::Error::new(
            ironrdp::connector::sspi::ErrorKind::NoAuthenticatingAuthority,
            "Kerberos (a domain controller) is not supported; use a local account or NTLM",
        ))
    }
}

fn default_size() -> (u16, u16) {
    std::env::var("BOBCAT_RDP_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1920, 1080))
}

fn config(user: &str, pass: &str, domain: Option<String>, (w, h): (u16, u16)) -> connector::Config {
    connector::Config {
        credentials: Credentials::UsernamePassword { username: user.to_string(), password: pass.to_string() },
        domain,
        enable_tls: false,
        enable_credssp: true,
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_layout: 0,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: connector::DesktopSize { width: w, height: h },
        bitmap: None,
        client_build: 0,
        client_name: "bobcat".to_owned(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        #[cfg(windows)]
        platform: MajorPlatformType::WINDOWS,
        #[cfg(target_os = "macos")]
        platform: MajorPlatformType::MACINTOSH,
        #[cfg(not(any(windows, target_os = "macos")))]
        platform: MajorPlatformType::UNIX,
        enable_server_pointer: true,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        compression_type: None,
        pointer_software_rendering: true,
        multitransport_flags: None,
        performance_flags: PerformanceFlags::default(),
        desktop_scale_factor: 0,
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    }
}

/// The server's TLS public key (the CredSSP channel binding wants the bare key bytes).
fn public_key(tls: &SslStream<TcpStream>) -> Result<Vec<u8>, String> {
    use x509_cert::der::Decode;
    let cert = tls.ssl().peer_certificate().ok_or("the server presented no certificate")?;
    let der = cert.to_der().map_err(|e| e.to_string())?;
    let cert = x509_cert::Certificate::from_der(&der).map_err(|e| format!("certificate: {e}"))?;
    cert.tbs_certificate()
        .subject_public_key_info()
        .subject_public_key
        .as_bytes()
        .map(|b| b.to_vec())
        .ok_or_else(|| "certificate public key is not byte aligned".to_string())
}

type Framed = ironrdp_blocking::Framed<SslStream<TcpStream>>;

/// How often the local clipboard is looked at for a new text to offer the server.
const CLIP_POLL: Duration = Duration::from_millis(700);

/// The text clipboard: the CLIPRDR callbacks run inside `ActiveStage::process`, so they only queue
/// messages for `run` (which owns the stage) and put received text on the local clipboard.
#[derive(Debug)]
struct Clip {
    tx: Sender<ClipboardMessage>,
    /// The text last seen on the local clipboard or put there by the server (so it is not offered back).
    last: Arc<Mutex<Option<String>>>,
}

ironrdp::core::impl_as_any!(Clip);

impl Clip {
    fn local_text() -> Option<String> {
        crate::with_clip(|c| c.get_text()).ok().filter(|t| !t.is_empty())
    }
    fn formats() -> Vec<ClipboardFormat> {
        if Self::local_text().is_some() { vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)] } else { Vec::new() }
    }
}

impl CliprdrBackend for Clip {
    fn temporary_directory(&self) -> &str {
        ".bobcat-cliprdr"
    }
    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }
    fn on_ready(&mut self) {}
    fn on_request_format_list(&mut self) {
        let _ = self.tx.send(ClipboardMessage::SendInitiateCopy(Self::formats()));
    }
    fn on_process_negotiated_capabilities(&mut self, _capabilities: ClipboardGeneralCapabilityFlags) {}
    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        // fetch the text right away: it only costs a round trip, and the local clipboard is then ready
        if available_formats.iter().any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT) {
            let _ = self.tx.send(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
        }
    }
    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let reply = match (request.format, Self::local_text()) {
            (ClipboardFormatId::CF_UNICODETEXT, Some(t)) => OwnedFormatDataResponse::new_unicode_string(&t),
            _ => OwnedFormatDataResponse::new_error(),
        };
        let _ = self.tx.send(ClipboardMessage::SendFormatData(reply));
    }
    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        if let Ok(text) = response.to_unicode_string() {
            let text = text.trim_end_matches('\0').to_string();
            if !text.is_empty() {
                *self.last.lock().unwrap() = Some(text.clone());
                if debug() {
                    eprintln!("[bobcat] rdp: clipboard from Windows ({} chars)", text.chars().count());
                }
                let _ = crate::with_clip(|c| c.set_text(text));
            }
        }
    }
    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}
    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}
    fn on_lock(&mut self, _data_id: LockDataId) {}
    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

fn connect(app: &AppHandle, ip: &str, port: u16, user: &str, pass: &str, domain: Option<String>, size: (u16, u16), clip: Clip) -> Result<(ConnectionResult, Framed), String> {
    let addr = (ip, port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("connect failed: {e}"))?;
    tcp.set_nodelay(true).ok();
    let client_addr = tcp.local_addr().map_err(|e| e.to_string())?;
    let mut framed = ironrdp_blocking::Framed::new(tcp);
    let drdynvc = DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector = connector::ClientConnector::new(config(user, pass, domain, size), client_addr).with_static_channel(drdynvc);
    connector.attach_static_channel(Cliprdr::<CliprdrClientRole>::new(Box::new(clip)));

    status(app, "RDP: negotiating...");
    let should_upgrade = ironrdp_blocking::connect_begin(&mut framed, &mut connector).map_err(|e| format!("RDP negotiation: {e}"))?;
    let tcp = framed.into_inner_no_leftover();

    // the server's certificate is self-signed: pinned on first use, not verified
    let mut b = SslConnector::builder(SslMethod::tls()).map_err(|e| e.to_string())?;
    b.set_verify(SslVerifyMode::NONE);
    // CredSSP does not support TLS session resumption
    b.set_session_cache_mode(SslSessionCacheMode::OFF);
    let tls = b
        .build()
        .configure()
        .map_err(|e| e.to_string())?
        .use_server_name_indication(false)
        .verify_hostname(false)
        .connect(ip, tcp)
        .map_err(|e| format!("TLS: {e}"))?;
    crate::check_pin(ip, &tls)?;
    let key = public_key(&tls)?;

    let upgraded = ironrdp_blocking::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = ironrdp_blocking::Framed::new(tls);
    status(app, "RDP: logging in...");
    let result = ironrdp_blocking::connect_finalize(upgraded, connector, &mut framed, &mut NoNetwork, ip.to_string().into(), key, None)
        .map_err(|e| format!("RDP login: {e}"))?;
    Ok((result, framed))
}

/// Append a rectangle of the decoded image (RGBA) to `batch` (message kind 0, see `FRAMES`).
fn put_rgba(batch: &mut Vec<u8>, image: &DecodedImage, l: u16, t: u16, r: u16, b: u16) {
    let (w, h) = (image.width(), image.height());
    let (r, b) = (r.min(w.saturating_sub(1)), b.min(h.saturating_sub(1)));
    if l > r || t > b {
        return;
    }
    let (rw, rh) = (r - l + 1, b - t + 1);
    batch.push(0u8);
    for v in [l, t, rw, rh] {
        batch.extend_from_slice(&v.to_le_bytes());
    }
    let (data, stride) = (image.data(), image.stride());
    for y in t..=b {
        let start = y as usize * stride + l as usize * 4;
        let row = &data[start..start + rw as usize * 4];
        let at = batch.len();
        batch.extend_from_slice(row);
        for a in batch[at..].chunks_exact_mut(4) {
            a[3] = 255;
        }
    }
}

/// One step of a connection sequence (the reactivation one): read what it waits for, step it.
fn sequence_step(framed: &mut Framed, seq: &mut impl Sequence, buf: &mut WriteBuf) -> Result<connector::Written, String> {
    buf.clear();
    if let Some(hint) = seq.next_pdu_hint() {
        let pdu = framed.read_by_hint(hint).map_err(|e| format!("read: {e}"))?;
        seq.step(&pdu, buf).map_err(|e| e.to_string())
    } else {
        seq.step_no_input(buf).map_err(|e| e.to_string())
    }
}

/// US-layout scancode (extended flag, code) of an X11 keysym, ignoring Shift.
fn scancode(keysym: u32) -> Option<Scancode> {
    let (ext, code): (bool, u8) = match keysym {
        0xff08 => (false, 0x0e), // BackSpace
        0xff09 => (false, 0x0f), // Tab
        0xff0d => (false, 0x1c), // Return
        0xff1b => (false, 0x01), // Escape
        0xff14 => (false, 0x46), // Scroll_Lock
        0xff7f => (false, 0x45), // Num_Lock
        0xffe1 => (false, 0x2a), // Shift_L
        0xffe2 => (false, 0x36), // Shift_R
        0xffe3 => (false, 0x1d), // Control_L
        0xffe4 => (true, 0x1d),  // Control_R
        0xffe5 => (false, 0x3a), // Caps_Lock
        0xffe9 | 0xffe7 => (false, 0x38), // Alt_L, Meta_L
        0xffea | 0xffe8 | 0xfe03 => (true, 0x38), // Alt_R, Meta_R, ISO_Level3_Shift (AltGr)
        0xffeb => (true, 0x5b),  // Super_L
        0xffec => (true, 0x5c),  // Super_R
        0xff67 => (true, 0x5d),  // Menu
        0xff8d => (true, 0x1c),  // KP_Enter
        0xffff => (true, 0x53),  // Delete
        0xff63 => (true, 0x52),  // Insert
        0xff50 => (true, 0x47),  // Home
        0xff51 => (true, 0x4b),  // Left
        0xff52 => (true, 0x48),  // Up
        0xff53 => (true, 0x4d),  // Right
        0xff54 => (true, 0x50),  // Down
        0xff55 => (true, 0x49),  // Page_Up
        0xff56 => (true, 0x51),  // Page_Down
        0xff57 => (true, 0x4f),  // End
        0xffbe..=0xffc7 => (false, 0x3b + (keysym - 0xffbe) as u8), // F1-F10
        0xffc8 => (false, 0x57), // F11
        0xffc9 => (false, 0x58), // F12
        0x20 => (false, 0x39),   // space
        _ => {
            // printable ASCII: the key that types it on a US keyboard
            let c = char::from_u32(keysym).filter(char::is_ascii)?;
            let rows = [
                ("1234567890-=", 0x02u8),
                ("qwertyuiop[]", 0x10),
                ("asdfghjkl;'`", 0x1e),
                ("\\zxcvbnm,./", 0x2b),
            ];
            let lower = c.to_ascii_lowercase();
            let shifted = "!@#$%^&*()_+{}|:\"~<>?";
            let unshifted = "1234567890-=[]\\;'`,./";
            let base = match shifted.find(c) {
                Some(i) => unshifted.chars().nth(i)?,
                None => lower,
            };
            rows.iter().find_map(|(keys, first)| keys.find(base).map(|i| (false, first + i as u8)))?
        }
    };
    Some(Scancode::from_u8(ext, code))
}

/// Keys and pointer state as RDP input.
struct Input {
    db: Database,
    /// How each pressed keysym was sent, so its release matches.
    down: HashMap<u32, Operation>,
    buttons: u8,
    modifiers: u8,
}

impl Input {
    fn new() -> Self {
        Input { db: Database::new(), down: HashMap::new(), buttons: 0, modifiers: 0 }
    }

    fn key(&mut self, down: bool, keysym: u32) -> Vec<ironrdp::pdu::input::fast_path::FastPathInputEvent> {
        let modifier = match keysym {
            0xffe3 | 0xffe4 => Some(1u8),
            0xffe9 | 0xffea | 0xffe7 | 0xffe8 | 0xfe03 => Some(2),
            0xffeb | 0xffec => Some(4),
            _ => None,
        };
        if let Some(m) = modifier {
            if down { self.modifiers |= m } else { self.modifiers &= !m }
        }
        let op = if down {
            let printable = char::from_u32(keysym).filter(|c| !c.is_control() && keysym >= 0x20 && !(0xff00..0x1000000).contains(&keysym));
            let op = match (printable, self.modifiers & 7 == 0) {
                (Some(c), true) if c != ' ' => Operation::UnicodeKeyPressed(c),
                _ => match scancode(keysym) {
                    Some(sc) => Operation::KeyPressed(sc),
                    None => match printable {
                        Some(c) => Operation::UnicodeKeyPressed(c),
                        None => return Vec::new(),
                    },
                },
            };
            self.down.insert(keysym, op.clone());
            op
        } else {
            match self.down.remove(&keysym) {
                Some(Operation::UnicodeKeyPressed(c)) => Operation::UnicodeKeyReleased(c),
                Some(Operation::KeyPressed(sc)) => Operation::KeyReleased(sc),
                _ => return Vec::new(),
            }
        };
        self.db.apply([op]).to_vec()
    }

    /// RFB-style button mask: 1 left, 2 middle, 4 right, 8 wheel up, 16 wheel down, 32/64 wheel left/right.
    fn pointer(&mut self, buttons: u8, x: u16, y: u16) -> Vec<ironrdp::pdu::input::fast_path::FastPathInputEvent> {
        let mut ops = vec![Operation::MouseMove(MousePosition { x, y })];
        let changed = buttons ^ self.buttons;
        for (bit, b) in [(1u8, MouseButton::Left), (2, MouseButton::Middle), (4, MouseButton::Right)] {
            if changed & bit != 0 {
                ops.push(if buttons & bit != 0 { Operation::MouseButtonPressed(b) } else { Operation::MouseButtonReleased(b) });
            }
        }
        for (bit, vertical, units) in [(8u8, true, 120i16), (16, true, -120), (32, false, 120), (64, false, -120)] {
            if buttons & bit != 0 && self.buttons & bit == 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: vertical, rotation_units: units }));
            }
        }
        self.buttons = buttons;
        self.db.apply(ops).to_vec()
    }
}

/// Connect and show the Windows desktop until `Cmd::Stop`, a disconnect or an error.
pub fn run(app: &AppHandle, ip: &str, port: u16, user: &str, pass: &str, domain: Option<String>, want: Option<(u16, u16)>, remember: bool, cmds: &Receiver<Cmd>) -> Result<(), String> {
    let size = want.map(|(w, h)| MonitorLayoutEntry::adjust_display_size(w as u32, h as u32)).map(|(w, h)| (w as u16, h as u16)).unwrap_or_else(default_size);
    let (ctx, crx) = channel();
    let last = Arc::new(Mutex::new(Clip::local_text()));
    let (result, mut framed) = connect(app, ip, port, user, pass, domain, size, Clip { tx: ctx, last: last.clone() })?;
    if remember {
        crate::keychain::remember(user, ip, pass);
    }
    let (mut w, mut h) = (result.desktop_size.width, result.desktop_size.height);
    let activation = result.activation_factory;
    status(app, format!("connected: Windows desktop {w}x{h}"));
    let mut image = DecodedImage::new(PixelFormat::RgbA32, w, h);
    let mut stage: ActiveStage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    let chan = FRAMES.lock().unwrap().clone();
    let mut batch: Vec<u8> = Vec::new();
    put_size(app, &chan, &mut batch, w, h);
    flush_frames(&chan, &mut batch);
    let mut input = Input::new();

    // short read timeouts let the loop pick up key and pointer commands between server updates
    framed.get_inner().0.get_ref().set_read_timeout(Some(Duration::from_millis(5))).ok();

    let mut updates = 0u64;
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut last_resize_note = std::time::Instant::now();
    let mut last_clip_poll = std::time::Instant::now();
    loop {
        let mut events = Vec::new();
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Stop => {
                    if let Ok(outs) = stage.graceful_shutdown() {
                        for o in outs {
                            if let ActiveStageOutput::ResponseFrame(f) = o {
                                let _ = framed.write_all(&f);
                            }
                        }
                    }
                    return Ok(());
                }
                Cmd::Key { down, keysym } => events.extend(input.key(down, keysym)),
                Cmd::Pointer { buttons, x, y } => events.extend(input.pointer(buttons, x, y)),
                Cmd::Resize(rw, rh) => {
                    if debug() {
                        eprintln!("[bobcat] rdp: resize request {rw}x{rh} (desktop is {w}x{h})");
                    }
                    pending_resize = Some((rw, rh))
                }
                Cmd::Input(_) => {}
            }
        }
        let mut outputs = Vec::new();
        // a new size goes over the Display Control channel as soon as that is connected
        if let Some((rw, rh)) = pending_resize {
            let (nw, nh) = MonitorLayoutEntry::adjust_display_size(rw as u32, rh as u32);
            if (nw, nh) == (w as u32, h as u32) {
                pending_resize = None;
            } else if let Some(frame) = stage.encode_resize(nw, nh, None, None) {
                pending_resize = None;
                if debug() {
                    eprintln!("[bobcat] rdp: asking for {nw}x{nh} (now {w}x{h})");
                }
                outputs.push(ActiveStageOutput::ResponseFrame(frame.map_err(|e| format!("resize: {e}"))?));
            } else if debug() && last_resize_note.elapsed() >= Duration::from_secs(2) {
                last_resize_note = std::time::Instant::now();
                eprintln!("[bobcat] rdp: resize to {nw}x{nh} waiting: the Display Control channel is not connected (does the server support it?)");
            }
        }
        // clipboard: queued messages from the callbacks, and a new local text to offer
        if last_clip_poll.elapsed() >= CLIP_POLL {
            last_clip_poll = std::time::Instant::now();
            let now = Clip::local_text();
            let changed = {
                let mut l = last.lock().unwrap();
                if now.is_some() && *l != now {
                    *l = now;
                    true
                } else {
                    false
                }
            };
            if changed {
                if let Some(c) = stage.get_svc_processor_mut::<Cliprdr<CliprdrClientRole>>() {
                    if let Ok(m) = c.initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]) {
                        outputs.push(ActiveStageOutput::ResponseFrame(stage.process_svc_processor_messages(m).map_err(|e| format!("clipboard: {e}"))?));
                    }
                }
            }
            if let Some(c) = stage.get_svc_processor_mut::<Cliprdr<CliprdrClientRole>>() {
                if let Ok(m) = c.drive_timeouts() {
                    let f = stage.process_svc_processor_messages(m).map_err(|e| format!("clipboard: {e}"))?;
                    if !f.is_empty() {
                        outputs.push(ActiveStageOutput::ResponseFrame(f));
                    }
                }
            }
        }
        while let Ok(msg) = crx.try_recv() {
            let Some(c) = stage.get_svc_processor_mut::<Cliprdr<CliprdrClientRole>>() else { break };
            let m = match msg {
                ClipboardMessage::SendInitiateCopy(f) => c.initiate_copy(&f),
                ClipboardMessage::SendFormatData(r) => c.submit_format_data(r),
                ClipboardMessage::SendInitiatePaste(f) => c.initiate_paste(f),
                _ => continue,
            };
            if let Ok(m) = m {
                outputs.push(ActiveStageOutput::ResponseFrame(stage.process_svc_processor_messages(m).map_err(|e| format!("clipboard: {e}"))?));
            }
        }
        if !events.is_empty() {
            outputs.extend(stage.process_fastpath_input(&mut image, &events).map_err(|e| format!("input: {e}"))?);
        }
        match framed.read_pdu() {
            Ok((action, payload)) => {
                outputs.extend(stage.process(&mut image, action, &payload).map_err(|e| format!("session: {e}"))?);
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(format!("disconnected: {e}")),
        }
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(f) => framed.write_all(&f).map_err(|e| format!("write: {e}"))?,
                ActiveStageOutput::GraphicsUpdate(r) => {
                    updates += 1;
                    put_rgba(&mut batch, &image, r.left, r.top, r.right, r.bottom);
                    if batch.len() >= crate::BATCH_LIMIT {
                        flush_frames(&chan, &mut batch);
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    flush_frames(&chan, &mut batch);
                    return Err(format!("the server ended the session: {}", reason.description()));
                }
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation-Reactivation Sequence: the capability exchange again, with the new desktop size
                    framed.get_inner().0.get_ref().set_read_timeout(None).ok();
                    let mut seq = activation.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = sequence_step(&mut framed, &mut seq, &mut buf).map_err(|e| format!("resize: {e}"))?;
                        if written.size().is_some() {
                            framed.write_all(buf.filled()).map_err(|e| format!("write: {e}"))?;
                        }
                        if let ConnectionActivationState::Finalized { desktop_size, share_id, enable_server_pointer, pointer_software_rendering } = seq.connection_activation_state() {
                            w = desktop_size.width;
                            h = desktop_size.height;
                            image = DecodedImage::new(PixelFormat::RgbA32, w, h);
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: seq.io_channel_id(),
                                    user_channel_id: seq.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            break;
                        }
                    }
                    framed.get_inner().0.get_ref().set_read_timeout(Some(Duration::from_millis(5))).ok();
                    if debug() {
                        eprintln!("[bobcat] rdp: desktop is now {w}x{h}");
                    }
                    batch.clear();
                    put_size(app, &chan, &mut batch, w, h);
                    flush_frames(&chan, &mut batch);
                }
                _ => {}
            }
        }
        flush_frames(&chan, &mut batch);
        if debug() && updates > 0 && updates % 200 == 0 {
            eprintln!("[bobcat] rdp: {updates} graphics updates");
        }
    }
}
