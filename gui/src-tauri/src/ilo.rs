//! HP / HPE iLO for the "HTML5" protocol: telling an iLO from an iDRAC, and logging in to an iLO.
//!
//! The HTML5 console itself is a web page of the BMC (`html5.rs` embeds it), so each vendor needs
//! its own login and its own page: Dell's `virtualconsolehtml5.html` after `/data/login`, iLO's
//! `/irc.html` after `POST /json/login_session`. The page reads the session key from the
//! `sessionKey` cookie with JavaScript (`iLO.getSessionKey()`), so unlike Dell's session cookie it
//! must not be HttpOnly. Findings (tried on an iLO 4, firmware 2.80) are in RESEARCH.md.

use crate::html5::{debug_on, request, Console, WebSession, SESSIONS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Vendor {
    Dell,
    Hpe,
}

impl Vendor {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Vendor::Dell => "iDRAC",
            Vendor::Hpe => "iLO",
        }
    }
}

/// iDRAC or iLO? Both answer the Redfish service root without a login, and an iLO also serves
/// `/xmldata`. BOBCAT_BMC=idrac|ilo overrides it. An unreachable host or a changed certificate is an
/// error; a BMC that answers neither question is taken for an iDRAC (the older, original target).
pub(crate) fn detect(ip: &str) -> Result<Vendor, String> {
    match std::env::var("BOBCAT_BMC").ok().as_deref() {
        Some("ilo") | Some("hpe") => return Ok(Vendor::Hpe),
        Some("idrac") | Some("dell") => return Ok(Vendor::Dell),
        _ => {}
    }
    let root = request(ip, "GET", "/redfish/v1/", &[], None)?;
    if root.status == 200 {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&root.text()) {
            let oem = v.get("Oem").and_then(|o| o.as_object());
            if oem.is_some_and(|o| o.contains_key("Hpe") || o.contains_key("Hp")) {
                return Ok(Vendor::Hpe);
            }
            if oem.is_some_and(|o| o.contains_key("Dell")) || v.get("Vendor").and_then(|s| s.as_str()) == Some("Dell") {
                return Ok(Vendor::Dell);
            }
        }
    }
    if let Ok(x) = request(ip, "GET", "/xmldata?item=all", &[], None) {
        if x.status == 200 && x.text().contains("Integrated Lights-Out") {
            return Ok(Vendor::Hpe);
        }
    }
    Ok(Vendor::Dell)
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn auth(key: &str) -> [(&'static str, String); 2] {
    [("Cookie", format!("sessionKey={key}")), ("X-Auth-Token", key.to_string())]
}

/// Log in to an iLO and describe its console page.
pub(crate) fn login(ip: &str, user: &str, pass: &str) -> Result<Console, String> {
    let body = format!(r#"{{"method":"login","user_login":{},"password":{}}}"#, json_string(user), json_string(pass));
    let r = request(ip, "POST", "/json/login_session", &[], Some(&body))?;
    let v: serde_json::Value = serde_json::from_str(&r.text()).unwrap_or_default();
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
    if r.status != 200 {
        return Err(match (r.status, message) {
            (403, "JS_ERR_NO_PRIV") => "login failed: wrong user name or password".into(),
            (403, "JS_ERR_FAILURE") => "login failed: the iLO has no free sessions (close one and try again)".into(),
            (500, "JS_ERR_FAILURE") => "login failed: the iLO asks for a login delay after failed attempts; wait and try again".into(),
            _ => format!("login failed: HTTP {} {message}", r.status),
        });
    }
    let key = v.get("session_key").and_then(|k| k.as_str()).filter(|k| !k.is_empty()).ok_or("the iLO login gave no session key")?.to_string();
    let session = WebSession { ip: ip.to_string(), cookie: format!("sessionKey={key}"), st2: String::new(), vendor: Vendor::Hpe };
    // from here on the session exists on the iLO: always log out if we give up
    SESSIONS.lock().unwrap().push(session.clone());
    if v.get("remote_cons_priv").and_then(|p| p.as_i64()) != Some(1) {
        return Err("this user may not use the remote console".into());
    }
    // the console needs iLO Advanced (or a trial); the page says so itself, this only explains it early
    if let Ok(l) = request(ip, "GET", "/json/license", &auth(&key), None) {
        let l: serde_json::Value = serde_json::from_str(&l.text()).unwrap_or_default();
        if debug_on() {
            eprintln!("[bobcat] ilo: licence {} ({})", l["name"], l["type"]);
        }
        if l.get("type").and_then(|t| t.as_str()) == Some("Unlicensed") {
            return Err(format!("this iLO is not licensed for the remote console ({}); it needs iLO Advanced", l["name"].as_str().unwrap_or("iLO Standard")));
        }
    }
    let page = std::env::var("BOBCAT_ILO_PAGE").unwrap_or_else(|_| "/irc.html".into());
    // macOS WebKit refuses the iLO's certificate before the pinning delegate is asked (see ilo_relay.rs):
    // the view goes through a local relay there. BOBCAT_ILO_RELAY=1 / 0 forces it on / off.
    let relay = match std::env::var("BOBCAT_ILO_RELAY").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(target_os = "macos"),
    };
    let (url, web_host) = if relay {
        (format!("https://127.0.0.1:{}{page}", crate::ilo_relay::ensure(ip)?), "127.0.0.1".to_string())
    } else {
        (format!("https://{ip}{page}"), ip.to_string())
    };
    // irc.html takes the session from a `token` parameter when it has one (else from the sessionKey cookie)
    let url = if page.starts_with("/irc.html") { format!("{url}?token={key}") } else { url };
    Ok(Console { session, url, locale: "null".into(), st1: String::new(), vendor: Vendor::Hpe, web_host })
}

pub(crate) fn logout(s: &WebSession) {
    let key = s.cookie.split_once('=').map(|(_, v)| v).unwrap_or("");
    let body = format!(r#"{{"method":"logout","session_key":{}}}"#, json_string(key));
    let _ = request(&s.ip, "POST", "/json/login_session", &auth(key), Some(&body));
}

/// The iLO ends idle sessions after a few minutes.
pub(crate) fn ping(s: &WebSession) {
    let key = s.cookie.split_once('=').map(|(_, v)| v).unwrap_or("");
    let _ = request(&s.ip, "GET", "/json/session_info", &auth(key), None);
}

/// The iLO's open sessions (Redfish `SessionService/Sessions` with a short-lived login of our own,
/// which shows in the list as "this one"): one line per session with user, kind, address and login time.
pub(crate) fn session_list(ip: &str, user: &str, pass: &str) -> Result<Vec<String>, String> {
    let body = format!(r#"{{"method":"login","user_login":{},"password":{}}}"#, json_string(user), json_string(pass));
    let r = request(ip, "POST", "/json/login_session", &[], Some(&body))?;
    let v: serde_json::Value = serde_json::from_str(&r.text()).unwrap_or_default();
    if r.status != 200 {
        return Err(format!("login failed: HTTP {} {}", r.status, v.get("message").and_then(|m| m.as_str()).unwrap_or("")));
    }
    let key = v.get("session_key").and_then(|k| k.as_str()).filter(|k| !k.is_empty()).ok_or("the iLO login gave no session key")?.to_string();
    let result = (|| {
        let get = |path: &str| -> Result<serde_json::Value, String> {
            let r = request(ip, "GET", path, &auth(&key), None)?;
            if r.status != 200 {
                return Err(format!("{path}: HTTP {}", r.status));
            }
            serde_json::from_str(&r.text()).map_err(|e| format!("{path}: {e}"))
        };
        let list = get("/redfish/v1/SessionService/Sessions/")?;
        if debug_on() {
            eprintln!("[bobcat] ilo: sessions {list}");
        }
        // iLO 4 firmware differs: the members are links, or full objects (`Members` / `Items`)
        let members = list.get("Members").or_else(|| list.get("Items")).and_then(|m| m.as_array()).cloned().unwrap_or_default();
        let mut lines = Vec::new();
        for m in members {
            let s = if m.get("UserName").is_some() {
                m
            } else if let Some(link) = m.get("@odata.id").or_else(|| m.pointer("/links/self/href")).and_then(|l| l.as_str()) {
                get(link)?
            } else {
                continue;
            };
            let hp = s.pointer("/Oem/Hp").or_else(|| s.pointer("/Oem/Hpe")).cloned().unwrap_or_default();
            let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let mut parts = vec![text(&s, "UserName")];
            for (v, k) in [(&hp, "UserTag"), (&hp, "UserIP"), (&s, "LoginTime"), (&hp, "LoginTime")] {
                let t = text(v, k);
                if !t.is_empty() && !parts.contains(&t) {
                    parts.push(t);
                }
            }
            let mine = hp.get("MySession").and_then(|b| b.as_bool()).unwrap_or(false);
            lines.push(format!("{}{}", parts.join(" "), if mine { " (this query)" } else { "" }));
        }
        Ok(lines)
    })();
    let _ = request(ip, "POST", "/json/login_session", &auth(&key), Some(&format!(r#"{{"method":"logout","session_key":{}}}"#, json_string(&key))));
    result
}
