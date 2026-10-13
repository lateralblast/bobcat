// bobcat-gui: the iDRAC HTML5 virtual console in its own window
// Version: 1.1.9
// License: CC BY-NC-SA 4.0 (see LICENSE.md)
//
// The iDRAC's HTML5 console is a web page (virtualconsolehtml5.html) that needs a logged-in web
// session. We log in ourselves over a TLS connection whose certificate is checked against the
// pinned one *before* the password is sent, read the few session values the page needs, give the
// session cookie to a new web view window and open the page there. The same pin is enforced for
// the window's own connections, and closing the window (or the app) logs the session out.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use tauri::webview::cookie::Cookie;
use tauri::webview::{NewWindowFeatures, NewWindowResponse, WebviewBuilder};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl, WebviewWindowBuilder};

pub(crate) const LABEL: &str = "html5console";

/// A logged-in iDRAC web session we must log out of.
#[derive(Clone)]
struct WebSession {
    ip: String,
    cookie: String, // "-http-session-=..."
    st2: String,
}

static SESSIONS: Mutex<Vec<WebSession>> = Mutex::new(Vec::new());

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn decode_chunked(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = b.windows(2).position(|w| w == b"\r\n") else { break };
        let size = usize::from_str_radix(std::str::from_utf8(&b[..nl]).unwrap_or("0").split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        b = &b[nl + 2..];
        if size == 0 || b.len() < size {
            out.extend_from_slice(&b[..b.len().min(size)]);
            break;
        }
        out.extend_from_slice(&b[..size]);
        b = b.get(size + 2..).unwrap_or(&[]);
    }
    out
}

/// One HTTPS request on a fresh TLS connection. The certificate is checked against the
/// pin (saved on first use) before anything is written, so credentials never reach an
/// impostor.
fn request(ip: &str, method: &str, path: &str, extra: &[(&str, String)], body: Option<&str>) -> Result<Resp, String> {
    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};
    let addr = (ip, 443).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("connect failed: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(60))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(20))).ok();
    let mut b = SslConnector::builder(SslMethod::tls()).map_err(|e| e.to_string())?;
    b.set_verify(SslVerifyMode::NONE);
    b.set_cipher_list("ALL:@SECLEVEL=0").map_err(|e| e.to_string())?;
    let mut cfg = b.build().configure().map_err(|e| e.to_string())?;
    cfg.set_verify_hostname(false);
    cfg.set_use_server_name_indication(false);
    let mut tls = cfg.connect(ip, tcp).map_err(|e| format!("tls handshake: {e}"))?;
    crate::check_pin(ip, &tls)?;

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ip}\r\nConnection: close\r\nAccept-Encoding: gzip\r\nUser-Agent: bobcat-gui\r\n"
    );
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(body) = body {
        req.push_str(&format!("Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    if let Some(body) = body {
        req.push_str(body);
    }
    tls.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(_) if !raw.is_empty() => break, // the server may close without a TLS goodbye
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or("bad status line")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")) {
        body = decode_chunked(&body);
    }
    // the iDRAC serves its static files only to clients that accept gzip
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-encoding") && v.contains("gzip")) {
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut plain).map_err(|e| format!("bad gzip body: {e}"))?;
        body = plain;
    }
    Ok(Resp { status, headers, body })
}

fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim().to_string())
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// What the console page needs, taken from a fresh login.
struct Console {
    session: WebSession,
    url: String,
    /// the iDRAC's language strings: the viewer page reads them from window.opener.top.localeObj
    locale: String,
}

fn login(ip: &str, user: &str, pass: &str) -> Result<Console, String> {
    let body = format!("user={}&password={}", pct(user), pct(pass));
    let r = request(ip, "POST", "/data/login", &[], Some(&body))?;
    if r.status != 200 {
        return Err(format!("login failed: HTTP {}", r.status));
    }
    let xml = r.text();
    let auth: i32 = tag(&xml, "authResult").and_then(|v| v.parse().ok()).unwrap_or(99);
    if auth != 0 {
        return Err(match auth {
            1 => "login failed: wrong user name or password".into(),
            2 => "login failed: no user name given".into(),
            3 => "login failed: no password given".into(),
            4 => "login failed: this user is not allowed to log in".into(),
            5 => "login failed: the iDRAC has no free web sessions (close one and try again)".into(),
            _ => format!("login failed (code {auth})"),
        });
    }
    let forward = tag(&xml, "forwardUrl").ok_or("login response had no forwardUrl")?;
    let st1 = forward.split("ST1=").nth(1).and_then(|s| s.split(',').next()).ok_or("no ST1 token")?.to_string();
    let st2 = forward.split("ST2=").nth(1).ok_or("no ST2 token")?.trim().to_string();
    let cookie = r
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .find_map(|(_, v)| v.split(';').next().filter(|c| c.starts_with("-http-session-=")).map(|c| c.to_string()))
        .ok_or("login gave no session cookie")?;
    let session = WebSession { ip: ip.to_string(), cookie, st2: st2.clone() };
    // from here on the session exists on the iDRAC: always log out if we give up
    SESSIONS.lock().unwrap().push(session.clone());
    let get = |path: &str| -> Result<serde_json::Value, String> {
        let r = request(ip, "GET", path, &[("Cookie", session.cookie.clone()), ("ST2", st2.clone())], None)?;
        serde_json::from_str(&r.text()).map_err(|_| format!("unexpected answer from {path} (HTTP {})", r.status))
    };
    let ssn = get("/session?getSsnVar=aimSession,accessConsoleRedirection")?;
    let props = get("/session?aimGetIntProp=rp_int_apcp_port,gui_kvm_plugin_type,vkvm_bool_enabled")?;
    let pick = |v: &serde_json::Value, group: &str, key: &str| -> String {
        match &v[group][key] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    };
    if pick(&ssn, "getSsnVar", "accessConsoleRedirection") != "1" {
        return Err("this user may not use the virtual console".into());
    }
    if pick(&props, "aimGetIntProp", "vkvm_bool_enabled") != "1" {
        return Err("the virtual console is disabled on the iDRAC (bobcat-cli --enable html5)".into());
    }
    if pick(&props, "aimGetIntProp", "gui_kvm_plugin_type") != "2" {
        return Err("the iDRAC console plugin is not HTML5 (bobcat-cli --enable html5 selects it)".into());
    }
    let locale = request(ip, "GET", "/locale/locale_en.json", &[("Cookie", session.cookie.clone()), ("ST2", st2.clone())], None)?;
    // the file is {"localeString": {...}}; the web interface uses the inner object as localeObj
    let locale_obj = serde_json::from_slice::<serde_json::Value>(&locale.body)
        .ok()
        .and_then(|v| v.get("localeString").cloned())
        .filter(|v| v.is_object())
        .ok_or_else(|| format!("could not read the iDRAC language file (HTTP {})", locale.status))?;
    let aim = pick(&ssn, "getSsnVar", "aimSession");
    let port = pick(&props, "aimGetIntProp", "rp_int_apcp_port");
    let url = format!(
        "https://{ip}/virtualconsolehtml5.html?ipAddr={ip}&kvmPort={port}&vmPriv=false&lang=en&aimSession={aim}&ST2={st2}&TokenName=ST1&TokenKey={st1}"
    );
    Ok(Console { session, url, locale: locale_obj.to_string() })
}

fn logout(s: &WebSession) {
    let _ = request(&s.ip, "GET", "/data/logout", &[("Cookie", s.cookie.clone()), ("ST2", s.st2.clone())], None);
    SESSIONS.lock().unwrap().retain(|x| x.cookie != s.cookie);
}

/// Log every open web session out (window close, Ctrl+C, SIGTERM).
pub fn logout_all() {
    let all: Vec<WebSession> = SESSIONS.lock().unwrap().clone();
    let handles: Vec<_> = all.into_iter().map(|s| std::thread::spawn(move || logout(&s))).collect();
    for h in handles {
        let _ = h.join();
    }
}

/// Make the web view accept the iDRAC's self-signed certificate, but only if it is exactly
/// the pinned one (the connection was already checked once in `request`).
#[cfg(target_os = "linux")]
fn pin_webview_tls(webview: &tauri::Webview) {
    use webkit2gtk::gio::prelude::*;
    use webkit2gtk::{SettingsExt, WebContextExt, WebViewExt};
    let _ = webview.with_webview(|pw| {
        let wv = pw.inner();
        // WebKit blocks window.open() that is not the result of a click, before the app is
        // even asked; the viewer opens its certificate window automatically when it loads
        if let Some(settings) = WebViewExt::settings(&wv) {
            settings.set_javascript_can_open_windows_automatically(true);
        }
        wv.connect_load_failed_with_tls_errors(|wv, uri, cert, _errors| {
            let host = uri.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("").to_string();
            let Some(der) = cert.certificate() else { return false };
            let Ok(hash) = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), &der) else { return false };
            let fp = hash.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
            if crate::load_pins().get(&host) == Some(&fp) {
                if let Some(ctx) = wv.context() {
                    ctx.allow_tls_certificate_for_host(cert, &host);
                }
                wv.load_uri(uri);
                true
            } else {
                eprintln!("[bobcat-gui] blocked a certificate for {host} that does not match the pinned one ({fp})");
                false
            }
        });
    });
}

#[cfg(not(target_os = "linux"))]
fn pin_webview_tls(_webview: &tauri::Webview) {}

/// The iDRAC ends idle web sessions; the viewer page does not keep this one alive without
/// its original opener window, so ping the session every couple of minutes while it exists.
fn keep_alive(s: WebSession) {
    std::thread::spawn(move || loop {
        for _ in 0..120 {
            std::thread::sleep(Duration::from_secs(1));
            if !SESSIONS.lock().unwrap().iter().any(|x| x.cookie == s.cookie) {
                return;
            }
        }
        let _ = request(&s.ip, "GET", "/session?getSsnVar=aimSession", &[("Cookie", s.cookie.clone()), ("ST2", s.st2.clone())], None);
    });
}

static CLOSING: AtomicBool = AtomicBool::new(false);
/// Height (CSS px) the main page needs for its toolbar and status bar; reported by the front end.
static TOP_HEIGHT: AtomicI32 = AtomicI32::new(0);

fn debug_on() -> bool {
    std::env::var("BOBCAT_DEBUG").is_ok()
}

/// Close the console the way Dell's own Disconnect Viewer button does: call the viewer's
/// disconnectKVM(), which sends the "close connection" messages so the iDRAC ends the Virtual
/// Console session, give it a moment, and only then close the view (just killing it leaves the
/// Virtual Console session on the iDRAC for minutes). Then log the web session out. Works for
/// the embedded view and the separate window. Blocks for about 2.5 seconds.
pub fn close_window(app: &AppHandle) {
    let Some(wv) = app.get_webview(LABEL) else { return };
    let _ = wv.eval("try { disconnectKVM(); } catch (e) {}");
    std::thread::sleep(Duration::from_millis(2500));
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    } else {
        let _ = wv.close();
        restore_main_layout(app);
    }
    destroy_popups(app);
    logout_all();
}

/// Close any certificate pop-up windows still open.
fn destroy_popups(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with("html5popup") {
            let _ = win.destroy();
        }
    }
}

/// The script that gives Dell's viewer page the window.opener it expects. The page is written as
/// a pop-up of the iDRAC web interface and reads its language strings from
/// window.opener.top.localeObj, so provide that before its scripts run.
fn opener_script(locale: &str) -> String {
    format!(
        "try {{ Object.defineProperty(window, 'opener', {{ configurable: true, value: {{ top: {{ localeObj: {locale}, htmlViewerWindow: null }} }} }}); }} catch (e) {{}}"
    )
}

/// The viewer opens a small window to the console server's own address (https://<ip>:5900) so the
/// browser can trust its certificate, pings it with postMessage and waits for the page to answer.
/// Allow exactly that: a window to this iDRAC, linked to the console view (so window.opener and
/// postMessage work), with the same pinned-certificate check. Anything else is denied.
fn new_window_handler(app: AppHandle, ip: String) -> impl Fn(Url, NewWindowFeatures) -> NewWindowResponse<tauri::Wry> + Send + 'static {
    move |url, features| {
        if debug_on() {
            eprintln!("[bobcat-gui] the console page asked for a new window: {url}");
        }
        if url.host_str() != Some(ip.as_str()) {
            if debug_on() {
                eprintln!("[bobcat-gui] denied (not the iDRAC's address)");
            }
            return NewWindowResponse::Deny;
        }
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNT.fetch_add(1, Ordering::SeqCst);
        match WebviewWindowBuilder::new(&app, format!("html5popup{n}"), WebviewUrl::External("about:blank".parse().unwrap()))
            .window_features(features)
            .title(format!("iDRAC console - {ip} (certificate)"))
            .on_page_load(|w, payload| {
                if debug_on() {
                    eprintln!("[bobcat-gui] popup {:?}: {}", payload.event(), payload.url());
                }
                // The page answers the viewer's ping and then calls window.close(), which does not
                // close a window the app created. Once it has had ample time to answer (the viewer
                // pings every 250ms), close the window ourselves.
                if let tauri::webview::PageLoadEvent::Finished = payload.event() {
                    let w = w.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(3));
                        let _ = w.destroy();
                    });
                }
            })
            .build()
        {
            Ok(window) => {
                pin_webview_tls(window.as_ref());
                NewWindowResponse::Create { window }
            }
            Err(e) => {
                if debug_on() {
                    eprintln!("[bobcat-gui] could not create the window: {e}");
                }
                NewWindowResponse::Deny
            }
        }
    }
}

fn page_log(event: &str, payload: &tauri::webview::PageLoadPayload<'_>) {
    if debug_on() {
        eprintln!("[bobcat-gui] {event} {:?}: {}", payload.event(), payload.url());
    }
}

/// On Linux a child web view is packed into the window's vertical box (it is not positioned).
/// WebKit's own natural height is "the whole window", so a size request on the main page is only a
/// minimum; instead put the main page (toolbar and status bar) in a plain container of fixed height
/// at the top, and let the console view take all the space below it.
#[cfg(target_os = "linux")]
const STRIP: &str = "bobcat-strip";

#[cfg(target_os = "linux")]
fn apply_embedded_layout(app: &AppHandle) {
    use gtk::prelude::*;
    let top = TOP_HEIGHT.load(Ordering::SeqCst);
    let Some(console) = app.get_webview(LABEL) else { return };
    let _ = console.with_webview(move |pw| {
        let console_w: gtk::Widget = pw.inner().upcast();
        let Some(vbox) = console_w.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) else { return };
        for child in vbox.children() {
            if child == console_w {
                continue;
            }
            let strip: gtk::Widget = if child.widget_name() == STRIP {
                child
            } else {
                let eb = gtk::EventBox::new();
                eb.set_widget_name(STRIP);
                vbox.remove(&child);
                eb.add(&child);
                vbox.pack_start(&eb, false, true, 0);
                eb.show();
                eb.upcast()
            };
            if top > 0 {
                strip.set_size_request(-1, top);
            }
            vbox.set_child_packing(&strip, false, true, 0, gtk::PackType::Start);
            vbox.reorder_child(&strip, 0);
        }
        vbox.set_child_packing(&console_w, true, true, 0, gtk::PackType::Start);
        console_w.set_size_request(-1, -1);
        if debug_on() {
            // log the real sizes GTK gives the pieces (after it has laid them out)
            let vbox = vbox.clone();
            let console_w = console_w.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(800), move || {
                eprintln!("[bobcat-gui] layout: requested top strip height {top}");
                for child in vbox.children() {
                    let a = child.allocation();
                    let which = if child == console_w { "console view" } else { "toolbar strip" };
                    eprintln!("[bobcat-gui] layout: {which} at y={} size {}x{} (in a {}x{} box)", a.y(), a.width(), a.height(), vbox.allocated_width(), vbox.allocated_height());
                }
            });
        }
    });
}

/// Give the main page the whole window back (after the console view is gone).
#[cfg(target_os = "linux")]
fn restore_main_layout(app: &AppHandle) {
    use gtk::prelude::*;
    let Some(main) = app.get_webview("main") else { return };
    let _ = main.with_webview(|pw| {
        let w: gtk::Widget = pw.inner().upcast();
        let Some(parent) = w.parent() else { return };
        if parent.widget_name() == STRIP {
            let Some(vbox) = parent.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) else { return };
            if let Ok(strip) = parent.clone().downcast::<gtk::Container>() {
                strip.remove(&w);
            }
            vbox.remove(&parent);
            vbox.pack_start(&w, true, true, 0);
            vbox.reorder_child(&w, 0);
            if debug_on() {
                let (w2, vbox2) = (w.clone(), vbox.clone());
                gtk::glib::timeout_add_local_once(Duration::from_millis(800), move || {
                    let a = w2.allocation();
                    eprintln!("[bobcat-gui] layout restored: main page {}x{} in a {}x{} box, {} child(ren)", a.width(), a.height(), vbox2.allocated_width(), vbox2.allocated_height(), vbox2.children().len());
                });
            }
        } else if let Ok(vbox) = parent.downcast::<gtk::Box>() {
            vbox.set_child_packing(&w, true, true, 0, gtk::PackType::Start);
        }
        w.set_size_request(-1, -1);
    });
}

#[cfg(not(target_os = "linux"))]
fn apply_embedded_layout(_app: &AppHandle) {}
#[cfg(not(target_os = "linux"))]
fn restore_main_layout(_app: &AppHandle) {}

fn set_session_cookie(webview: &tauri::Webview, ip: &str, session: &WebSession) -> Result<(), String> {
    let cookie = session.cookie.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or("bad cookie")?;
    webview
        .set_cookie(Cookie::build(cookie).domain(ip.to_string()).path("/").secure(true).http_only(true).build())
        .map_err(|e| e.to_string())
}

/// The console as a child view inside the main window, under the toolbar.
fn open_embedded(app: &AppHandle, ip: &str, console: &Console) -> Result<(), String> {
    let main = app.get_window("main").ok_or("no main window")?;
    let scale = main.scale_factor().map_err(|e| e.to_string())?;
    let inner = main.inner_size().map_err(|e| e.to_string())?;
    let (w, h) = (inner.width as f64 / scale, inner.height as f64 / scale);
    let top = TOP_HEIGHT.load(Ordering::SeqCst).max(0) as f64;
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External("about:blank".parse().unwrap()))
        .initialization_script(opener_script(&console.locale))
        .on_new_window(new_window_handler(app.clone(), ip.to_string()))
        .on_page_load(|_w, payload| page_log("page", &payload));
    let webview = main
        .add_child(builder, LogicalPosition::new(0.0, top), LogicalSize::new(w, (h - top).max(100.0)))
        .map_err(|e| e.to_string())?;
    pin_webview_tls(&webview);
    apply_embedded_layout(app);
    set_session_cookie(&webview, ip, &console.session)?;
    webview.navigate(console.url.parse().map_err(|e| format!("{e}"))?).map_err(|e| e.to_string())?;
    Ok(())
}

/// The console in a window of its own (the fallback when embedding is not possible).
fn open_window(app: &AppHandle, ip: &str, console: &Console) -> Result<(), String> {
    let win = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External("about:blank".parse().unwrap()))
        .title(format!("iDRAC console - {ip}"))
        .inner_size(1100.0, 840.0)
        .initialization_script(opener_script(&console.locale))
        .on_new_window(new_window_handler(app.clone(), ip.to_string()))
        .on_page_load(|_w, payload| page_log("page", &payload))
        .build()
        .map_err(|e| e.to_string())?;
    pin_webview_tls(win.as_ref());
    let app_for_close = app.clone();
    win.on_window_event(move |event| {
        // the user closed the window: disconnect the viewer properly first
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            disconnect_in_background(&app_for_close);
        }
    });
    set_session_cookie(win.as_ref(), ip, &console.session)?;
    win.navigate(console.url.parse().map_err(|e| format!("{e}"))?).map_err(|e| e.to_string())?;
    Ok(())
}

fn open(app: &AppHandle, ip: &str, user: &str, pass: &str) -> Result<(), String> {
    close_window(app);
    crate::status(app, format!("logging in to {ip} (the iDRAC takes ~5s)..."));
    let console = login(ip, user, pass).map_err(|e| {
        logout_all(); // a half-finished login must not leave a session behind
        e
    })?;
    keep_alive(console.session.clone());
    let embedded = match open_embedded(app, ip, &console) {
        Ok(()) => true,
        Err(e) => {
            if debug_on() {
                eprintln!("[bobcat-gui] could not embed the console ({e}); using a separate window");
            }
            // clean up a half-made child before falling back
            if let Some(wv) = app.get_webview(LABEL) {
                let _ = wv.close();
                restore_main_layout(app);
            }
            open_window(app, ip, &console).map_err(|e| {
                logout_all();
                e
            })?;
            false
        }
    };
    if let Ok(spec) = std::env::var("BOBCAT_TEST_SNAPSHOT") {
        // "<seconds>:<directory>": save PNGs of the console and main page after that long (testing)
        if let Some((secs, dir)) = spec.split_once(':') {
            let (secs, dir, app) = (secs.parse::<u64>().unwrap_or(30), dir.to_string(), app.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                for label in [LABEL, "main"] {
                    match crate::html5_tools::snapshot_png(&app, label, None) {
                        Ok(png) => {
                            let _ = std::fs::write(format!("{dir}/{label}.png"), png);
                            eprintln!("[bobcat-gui] test: saved {dir}/{label}.png");
                        }
                        Err(e) => eprintln!("[bobcat-gui] test: snapshot of {label} failed: {e}"),
                    }
                }
            });
        }
    }
    // Test hooks: "<seconds>:<value>" runs paste (value = text, taken from the clipboard), copy
    // (value ignored) or zoom (value = factor) in the console after that long.
    for (var, action) in [("BOBCAT_TEST_PASTE", "paste"), ("BOBCAT_TEST_COPY", "copy"), ("BOBCAT_TEST_ZOOM", "zoom")] {
        if let Ok(spec) = std::env::var(var) {
            let (secs, value) = spec.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((spec.clone(), String::new()));
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs.parse().unwrap_or(30)));
                let r = match action {
                    "paste" => crate::with_clip(|c| c.set_text(value.clone()))
                        .and_then(|_| crate::html5_tools::paste_from_clipboard(&app).map(|n| format!("pasted {n} characters"))),
                    "copy" => crate::html5_tools::copy_text(&app),
                    _ => crate::html5_tools::zoom(&app, value.parse().unwrap_or(1.0)).map(|_| format!("zoom {value}")),
                };
                eprintln!("[bobcat-gui] test {action}: {r:?}");
            });
        }
    }
    if let Ok(spec) = std::env::var("BOBCAT_TEST_JS") {
        // "<seconds>:<javascript expression>": evaluate it in the console page and log the result
        if let Some((secs, js)) = spec.split_once(':') {
            let (secs, js, app) = (secs.parse::<u64>().unwrap_or(30), js.to_string(), app.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                eprintln!("[bobcat-gui] test js: {:?}", crate::html5_tools::run_js_pub(&app, js));
            });
        }
    }
    if let Ok(spec) = std::env::var("BOBCAT_TEST_MAINJS") {
        // like BOBCAT_TEST_JS but in the main (toolbar) page
        if let Some((secs, js)) = spec.split_once(':') {
            let (secs, js, app) = (secs.parse::<u64>().unwrap_or(30), js.to_string(), app.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                eprintln!("[bobcat-gui] test main js: {:?}", crate::html5_tools::run_js_in(&app, "main", js));
            });
        }
    }
    let (ip2, user2, pass2) = (ip.to_string(), user.to_string(), pass.to_string());
    if let Some(secs) = std::env::var("BOBCAT_TEST_DISCONNECT").ok().and_then(|v| v.parse::<u64>().ok()) {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            eprintln!("[bobcat-gui] test: disconnecting now");
            close_window(&app);
            std::thread::sleep(Duration::from_millis(1500));
            if std::env::var("BOBCAT_TEST_RECONNECT").is_ok() {
                eprintln!("[bobcat-gui] test: connecting again");
                if let Err(e) = open(&app, &ip2, &user2, &pass2) {
                    eprintln!("[bobcat-gui] test: reconnect failed: {e}");
                }
            }
        });
    }
    crate::status(app, if embedded { format!("HTML5 console of {ip}") } else { format!("HTML5 console of {ip} opened in its own window") });
    Ok(())
}

fn disconnect_in_background(app: &AppHandle) {
    if !CLOSING.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        std::thread::spawn(move || {
            close_window(&app);
            CLOSING.store(false, Ordering::SeqCst);
        });
    }
}

#[tauri::command]
pub fn connect_html5(app: AppHandle, ip: String, user: String, pass: String) {
    std::thread::spawn(move || {
        if let Err(e) = open(&app, &ip, &user, &pass) {
            crate::status(&app, format!("error: {e}"));
        }
    });
}

#[tauri::command]
pub fn disconnect_html5(app: AppHandle) {
    disconnect_in_background(&app);
}

/// The main page reports how tall its toolbar and status bar are, so the console view can take
/// the rest of the window.
#[tauri::command]
pub fn html5_layout(app: AppHandle, height: i32) {
    TOP_HEIGHT.store(height, Ordering::SeqCst);
    apply_embedded_layout(&app);
}
