// HPE iLO power and IPMI-over-LAN controls for the desktop app, through the iLO's own CLI over SSH
// (the same commands as `bobcat-cli --power` and `--enable sol`; IloCli is a copy of the CLI's
// struct in src/main.rs). The commands return at once; results reach the page as "ilo-note" events.

use crate::{check_hostkey, is_ilo};
use serde::Serialize;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
struct Note {
    text: String,
    ok: bool,
}

fn note(app: &AppHandle, ok: bool, text: impl Into<String>) {
    let text = text.into();
    eprintln!("[bobcat] iLO: {text}");
    let _ = app.emit("ilo-note", Note { text, ok });
}

/// An iLO CLI (SMASH CLP) session over an SSH shell (an exec channel never ends on an iLO). Each
/// command's output is read up to the next prompt.
struct IloCli {
    _sess: ssh2::Session,
    ch: ssh2::Channel,
}

impl IloCli {
    fn open(ip: &str, user: &str, pass: &str) -> Result<IloCli, String> {
        let addr = (ip, 22).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad address")?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("connect failed: {e}"))?;
        let mut sess = ssh2::Session::new().map_err(|e| e.to_string())?;
        sess.set_tcp_stream(tcp);
        sess.handshake().map_err(|e| format!("ssh handshake: {e}"))?;
        check_hostkey(ip, &sess)?; // before the password is sent
        sess.userauth_password(user, pass).map_err(|e| format!("authentication failed: {e}"))?;
        let mut ch = sess.channel_session().map_err(|e| e.to_string())?;
        ch.request_pty("vt100", None, Some((200, 50, 0, 0))).map_err(|e| e.to_string())?;
        ch.shell().map_err(|e| e.to_string())?;
        sess.set_timeout(15000);
        let mut cli = IloCli { _sess: sess, ch };
        cli.read_until_prompt(false)?; // the login banner
        Ok(cli)
    }

    fn read_until_prompt(&mut self, need_status: bool) -> Result<String, String> {
        let mut seen = String::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = self.ch.read(&mut buf).map_err(|e| format!("no answer from the iLO CLI ({e}); got: {}", seen.trim()))?;
            if n == 0 {
                return Err(format!("the iLO CLI closed the session; got: {}", seen.trim()));
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            let after = if need_status { seen.find("status=") } else { Some(0) };
            if after.is_some_and(|i| seen[i..].contains("hpiLO->")) {
                return Ok(seen);
            }
        }
    }

    /// Run one command; it reports `status=0` when it worked.
    fn run(&mut self, cmd: &str) -> Result<String, String> {
        self.ch.write_all(format!("{cmd}\r").as_bytes()).map_err(|e| e.to_string())?;
        let out = self.read_until_prompt(true)?;
        if out.contains("COMMAND PROCESSING FAILED") || !out.contains("status=0") {
            return Err(format!("iLO command `{cmd}` failed: {}", out.trim()));
        }
        Ok(out)
    }

    fn close(mut self) {
        let _ = self.ch.write_all(b"exit\r");
        let _ = self.ch.close();
    }
}

/// What the iLO answered, without the echoed command, the prompt, the status lines and the date.
fn answer(out: &str, cmd: &str) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    out.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("status")
                && !l.starts_with("</>hpiLO->")
                && *l != cmd
                && !DAYS.iter().any(|d| l.starts_with(d) && l.ends_with(|c: char| c.is_ascii_digit()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const PROP: &str = "oemhp_ipmi_dcmi_overlan_enable";

fn ipmi_state(cfg: &str) -> Option<bool> {
    cfg.lines().find_map(|l| l.trim().strip_prefix(&format!("{PROP}=")).map(|v| v.trim() == "yes"))
}

fn spawn(app: AppHandle, f: impl FnOnce(&AppHandle) -> Result<(), String> + Send + 'static) {
    thread::spawn(move || {
        if let Err(e) = f(&app) {
            note(&app, false, e);
        }
    });
}

/// Is the host an HPE iLO? (SSH banner, or BOBCAT_BMC.) The page shows the iLO row when it is.
#[tauri::command]
pub async fn bmc_is_ilo(ip: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || is_ilo(&ip)).await.unwrap_or(false)
}

/// Power state and whether IPMI over LAN is on.
#[tauri::command]
pub fn ilo_status(app: AppHandle, ip: String, user: String, pass: String) {
    spawn(app, move |app| {
        let mut cli = IloCli::open(&ip, &user, &pass)?;
        let r = (|| {
            let power = answer(&cli.run("POWER")?, "POWER");
            let ipmi = ipmi_state(&cli.run("show /map1/config1")?);
            Ok::<_, String>((power, ipmi))
        })();
        cli.close();
        let (power, ipmi_on) = r?;
        let ipmi = match ipmi_on {
            Some(true) => "on",
            Some(false) => "off",
            None => "unknown",
        };
        let _ = app.emit("ilo-ipmi", ipmi_on);
        note(app, true, format!("{power}; IPMI over LAN {ipmi}"));
        Ok(())
    });
}

/// A power action: on, off (press), off-hard (press and hold), reset or nmi.
#[tauri::command]
pub fn ilo_power(app: AppHandle, ip: String, user: String, pass: String, action: String) {
    spawn(app, move |app| {
        let cmd = match action.as_str() {
            "on" => "POWER ON",
            "off" => "POWER OFF",
            "off-hard" => "POWER HOLD",
            "reset" => "POWER RESET",
            "nmi" => "NMI",
            other => return Err(format!("unknown power action {other}")),
        };
        let mut cli = IloCli::open(&ip, &user, &pass)?;
        let r = cli.run(cmd);
        cli.close();
        let text = answer(&r?, cmd);
        note(app, true, if text.is_empty() { format!("{cmd}: done") } else { format!("{cmd}: {text}") });
        Ok(())
    });
}

/// Switch IPMI/DCMI over LAN (what the SOL protocol needs) on or off.
#[tauri::command]
pub fn ilo_ipmi(app: AppHandle, ip: String, user: String, pass: String, on: bool) {
    spawn(app, move |app| {
        let want = if on { "yes" } else { "no" };
        let mut cli = IloCli::open(&ip, &user, &pass)?;
        let r = (|| {
            if ipmi_state(&cli.run("show /map1/config1")?) != Some(on) {
                cli.run(&format!("set /map1/config1 {PROP}={want}"))?;
                if ipmi_state(&cli.run("show /map1/config1")?) != Some(on) {
                    return Err(format!("the iLO did not take {PROP}={want}"));
                }
            }
            Ok(())
        })();
        cli.close();
        r?;
        let _ = app.emit("ilo-ipmi", Some(on));
        note(app, true, format!("IPMI over LAN is {}", if on { "on (SOL can connect; the BIOS must use the virtual serial port)" } else { "off" }));
        Ok(())
    });
}

/// The iLO's open sessions (web UI, consoles, SSH, IPMI), from Redfish.
#[tauri::command]
pub fn ilo_sessions(app: AppHandle, ip: String, user: String, pass: String) {
    spawn(app, move |app| {
        let lines = crate::ilo::session_list(&ip, &user, &pass)?;
        note(app, true, format!("{} session(s): {}", lines.len(), lines.join(" | ")));
        Ok(())
    });
}
