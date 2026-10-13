// bobcat-cli - iDRAC management command line tool
// Version: 1.4.0
// License: CC BY-NC-SA 4.0 (see LICENSE.md)

use std::io::{Read, Write};
use std::thread;
use std::net::TcpStream;
use std::process::ExitCode;
use std::time::Duration;

mod keychain;
mod screen_text;
mod vnc_text;

use clap::{Parser, ValueEnum};
use ssh2::Session;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Feature {
    /// The iDRAC VNC server (TCP 5901)
    Vnc,
    /// The iDRAC HTML5 virtual console (browser, HTTPS)
    Html5,
    /// The iDRAC Java virtual console (Java Web Start viewer)
    Java,
    /// IPMI over LAN, Serial over LAN and serial console redirection
    Sol,
}

/// iDRAC VNC SSL encryption setting (iDRAC.VNCServer.SSLEncryptionBitLength)
#[derive(Clone, Copy, ValueEnum)]
enum SslEbl {
    #[value(name = "Disabled")]
    Disabled,
    #[value(name = "Auto_Negotiate")]
    AutoNegotiate,
    #[value(name = "128-Bit_or_higher")]
    Bit128,
    #[value(name = "168-Bit_or_higher")]
    Bit168,
}

impl SslEbl {
    fn racadm(self) -> &'static str {
        match self {
            // racadm only accepts the numeric values (names are rejected)
            SslEbl::Disabled => "0",
            SslEbl::AutoNegotiate => "1",
            SslEbl::Bit128 => "2",
            SslEbl::Bit168 => "3",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Console {
    /// Serial over LAN via ipmitool (escape: ~.)
    Sol,
    /// The iDRAC's SSH `console com2` (escape: ~. or Ctrl+\)
    Ssh,
    /// The VNC console converted to text (escape: ~.)
    Vnc,
}

#[derive(Parser)]
#[command(name = "bobcat-cli", version, about = "iDRAC management command line tool")]
struct Args {
    /// Check that required external packages are installed, then exit
    #[arg(long)]
    check: bool,

    /// With --check, install any missing packages using apt-get (uses sudo)
    #[arg(long, requires = "check")]
    install: bool,

    /// IP address of the iDRAC
    #[arg(long, required_unless_present = "check", default_value = "")]
    ip: String,

    /// iDRAC username
    #[arg(long, required_unless_present = "check", default_value = "")]
    user: String,

    /// iDRAC password (prefer the BOBCAT_PASS environment variable or the
    /// interactive prompt: command-line arguments are visible in `ps`)
    #[arg(long = "pass", env = "BOBCAT_PASS", hide_env_values = true)]
    pass: Option<String>,

    /// After success, save the iDRAC login password (sol, ssh, --enable/--disable) and the VNC
    /// password (--console vnc) in the OS keychain; they are used automatically next time
    #[arg(long)]
    save: bool,

    /// Remove the saved keychain passwords (login for --user, and VNC) for --ip, then exit
    #[arg(long)]
    forget: bool,

    /// VNC password to set with --enable vnc (visible in `ps`; prefer the
    /// BOBCAT_VNC_PASS environment variable or the interactive prompt)
    #[arg(long, env = "BOBCAT_VNC_PASS", hide_env_values = true)]
    vncpass: Option<String>,

    /// VNC server port (set with --enable vnc, used by --console vnc)
    #[arg(long, default_value_t = 5901, value_parser = clap::value_parser!(u16).range(1024..))]
    port: u16,

    /// VNC SSL encryption bit length (set with --enable vnc)
    #[arg(long, value_enum, default_value = "Disabled")]
    sslebl: SslEbl,

    /// VNC idle timeout in seconds (set with --enable vnc)
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(60..=10800))]
    timeout: u32,

    /// Accept a changed SSH host key or TLS certificate and save it (use only if you expected the change)
    #[arg(long)]
    trust_new_hostkey: bool,

    /// Use TLS for --console vnc (the default; needs VNC SSL encryption enabled on the iDRAC)
    #[arg(long, conflicts_with = "notls")]
    tls: bool,

    /// Do not use TLS for --console vnc (plain VNC)
    #[arg(long)]
    notls: bool,

    /// With --console vnc, forget the saved character table for this iDRAC and learn it again
    #[arg(long)]
    relearn: bool,

    /// Open a text console to the server (sol needs ipmitool)
    #[arg(long, value_enum)]
    console: Option<Console>,

    /// Enable iDRAC features via racadm over SSH
    #[arg(long, value_enum, value_delimiter = ',')]
    enable: Vec<Feature>,

    /// Disable iDRAC features via racadm over SSH
    #[arg(long, value_enum, value_delimiter = ',')]
    disable: Vec<Feature>,
}

/// Run one command on the iDRAC over SSH. Credentials are handed to the SSH
/// library in-process, so they never appear in any process argument list.
fn run(sess: &Session, cmd: &str) -> Result<String, String> {
    let mut ch = sess.channel_session().map_err(|e| e.to_string())?;
    ch.exec(cmd).map_err(|e| e.to_string())?;
    let mut out = String::new();
    ch.read_to_string(&mut out).map_err(|e| e.to_string())?;
    ch.wait_close().map_err(|e| e.to_string())?;
    // racadm over SSH exits 0 even when it fails, so look for its error text
    if out.contains("ERROR") {
        return Err(format!("`{cmd}` failed: {}", out.trim()));
    }
    match ch.exit_status().map_err(|e| e.to_string())? {
        0 => Ok(out),
        n => Err(format!("`{cmd}` exited with {n}: {}", out.trim())),
    }
}

/// Saved SSH host key fingerprints and TLS certificate pins. Shared with bobcat-gui: same file,
/// SSH keys are "ssh:<ip>".
fn pins_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("bobcat").join("known_certs.json")
}

/// Where dracon-gui (the old name of the GUI) kept the pins; still read if the new file is missing.
fn legacy_pins_path() -> std::path::PathBuf {
    let mut p = pins_path();
    p.pop();
    p.pop();
    p.join("dracon-gui").join("known_certs.json")
}

fn load_pins() -> std::collections::HashMap<String, String> {
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

fn save_pin(key: &str, fp: &str) -> Result<(), String> {
    let mut pins = load_pins();
    pins.insert(key.to_string(), fp.to_string());
    let path = pins_path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_string_pretty(&pins).unwrap()).map_err(|e| e.to_string())
}

/// Trust on first use for the SSH host key. Runs before any password is sent.
fn check_hostkey(ip: &str, sess: &Session, trust_new: bool) -> Result<(), String> {
    let fp = sess
        .host_key_hash(ssh2::HashType::Sha256)
        .ok_or("the iDRAC presented no SSH host key")?
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let key = format!("ssh:{ip}");
    match load_pins().get(&key) {
        None => {
            save_pin(&key, &fp)?;
            eprintln!("Saved SSH host key for {ip} (SHA-256 {fp})");
            Ok(())
        }
        Some(p) if *p == fp => Ok(()),
        Some(_) if trust_new => {
            save_pin(&key, &fp)?;
            eprintln!("Replaced the saved SSH host key for {ip} (SHA-256 {fp})");
            Ok(())
        }
        Some(p) => Err(format!(
            "WARNING: the SSH host key for {ip} has changed since it was first saved; connection blocked.\n  saved: {p}\n  now:   {fp}\nIf you expected this, run again with --trust-new-hostkey."
        )),
    }
}

fn connect(ip: &str, user: &str, pass: &str, trust_new: bool) -> Result<Session, String> {
    let tcp = TcpStream::connect_timeout(
        &format!("{ip}:22").parse().map_err(|e| format!("bad ip: {e}"))?,
        Duration::from_secs(10),
    )
    .map_err(|e| format!("connect failed: {e}"))?;
    let mut sess = Session::new().map_err(|e| e.to_string())?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|e| format!("ssh handshake: {e}"))?;
    check_hostkey(ip, &sess, trust_new)?;
    sess.userauth_password(user, pass)
        .map_err(|e| format!("authentication failed: {e}"))?;
    Ok(sess)
}

fn enable_vnc(sess: &Session, vncpass: Option<&str>, port: u16, sslebl: SslEbl, timeout: u32) -> Result<(), String> {
    let pw = match vncpass {
        Some(p) => p.to_string(),
        None => rpassword::prompt_password("New VNC password (max 8 chars): ")
            .map_err(|e| e.to_string())?,
    };
    if pw.is_empty() || pw.len() > 8 || !pw.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("VNC password must be 1-8 alphanumeric characters".into());
    }
    run(sess, &format!("racadm set iDRAC.VNCServer.Password {pw}"))?;
    run(sess, &format!("racadm set iDRAC.VNCServer.Port {port}"))?;
    run(
        sess,
        &format!("racadm set iDRAC.VNCServer.SSLEncryptionBitLength {}", sslebl.racadm()),
    )?;
    run(sess, &format!("racadm set iDRAC.VNCServer.Timeout {timeout}"))?;
    run(sess, "racadm set iDRAC.VNCServer.Enable Enabled")?;
    let state = run(sess, "racadm get iDRAC.VNCServer")?;
    println!("{}", state.trim());
    Ok(())
}

/// Enable the virtual console and select a viewer plugin
/// (PluginType: 1 = Java, 2 = HTML5; 0 is ActiveX).
fn enable_console(sess: &Session, plugin_type: u8) -> Result<(), String> {
    run(sess, "racadm set iDRAC.VirtualConsole.Enable Enabled")?;
    run(sess, &format!("racadm set iDRAC.VirtualConsole.PluginType {plugin_type}"))?;
    let state = run(sess, "racadm get iDRAC.VirtualConsole")?;
    println!("{}", state.trim());
    Ok(())
}

/// Current value of a racadm attribute, e.g. `iDRAC.IPMISOL.BaudRate` -> "115200".
fn get_attr(sess: &Session, attr: &str) -> Result<String, String> {
    let key = attr.rsplit('.').next().unwrap();
    let out = run(sess, &format!("racadm get {attr}"))?;
    out.lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{key}=")).map(|v| v.trim().to_string()))
        .ok_or_else(|| format!("could not read {attr}"))
}

/// Set `attr` to `value` unless it already shows `value`. Returns true if it changed.
fn ensure(sess: &Session, attr: &str, value: &str) -> Result<bool, String> {
    if get_attr(sess, attr)? == value {
        println!("[ok]      {attr} = {value}");
        return Ok(false);
    }
    run(sess, &format!("racadm set {attr} {value}"))?;
    println!("[changed] {attr} = {value}");
    Ok(true)
}

/// Enable IPMI over LAN, Serial over LAN and serial console redirection.
/// iDRAC settings apply immediately. BIOS serial redirection is only staged
/// (a job that runs at the next reboot): the server is never rebooted here.
fn enable_sol(sess: &Session) -> Result<(), String> {
    ensure(sess, "iDRAC.IPMILan.Enable", "Enabled")?;
    ensure(sess, "iDRAC.IPMISOL.Enable", "Enabled")?;
    ensure(sess, "iDRAC.IPMISOL.BaudRate", "115200")?;
    ensure(sess, "iDRAC.IPMISOL.MinPrivilege", "4")?; // 4 = Administrator
    ensure(sess, "iDRAC.Serial.Enable", "Enabled")?;
    ensure(sess, "iDRAC.SerialRedirection.Enable", "Enabled")?;

    // BIOS serial console redirection to COM2 (what SOL listens on) at 115200
    let mut bios = false;
    bios |= ensure(sess, "BIOS.SerialCommSettings.SerialComm", "OnConRedirCom2")?;
    bios |= ensure(sess, "BIOS.SerialCommSettings.SerialPortAddress", "Serial1Com2Serial2Com1")?;
    bios |= ensure(sess, "BIOS.SerialCommSettings.RedirAfterBoot", "Enabled")?;
    bios |= ensure(sess, "BIOS.SerialCommSettings.FailSafeBaud", "115200")?;
    if bios {
        let job = run(sess, "racadm jobqueue create BIOS.Setup.1-1")?;
        println!("{}", job.trim());
        println!("\nBIOS serial redirection is staged and applies at the next server reboot (not rebooted).");
    }
    println!("\nSOL: ipmitool -I lanplus -H <ip> -U <user> -E sol activate");
    println!("  or over SSH to the iDRAC: console com2");
    Ok(())
}

fn disable_vnc(sess: &Session) -> Result<(), String> {
    run(sess, "racadm set iDRAC.VNCServer.Enable Disabled")?;
    let state = run(sess, "racadm get iDRAC.VNCServer")?;
    println!("{}", state.trim());
    Ok(())
}

enum Probe {
    /// Executable found in PATH
    Bin(&'static str),
    /// Shared library soname known to the dynamic linker (Linux only)
    #[cfg(target_os = "linux")]
    Lib(&'static str),
}

struct Dep {
    name: &'static str,
    probe: Probe,
    /// Debian/Ubuntu package that provides it
    apt: &'static str,
    /// Homebrew formula (macOS)
    brew: &'static str,
    purpose: &'static str,
}

const DEPS: &[Dep] = &[
    Dep {
        name: "ipmitool",
        probe: Probe::Bin("ipmitool"),
        apt: "ipmitool",
        brew: "ipmitool",
        purpose: "--console sol",
    },
    Dep {
        name: "tesseract",
        probe: Probe::Bin("tesseract"),
        apt: "tesseract-ocr",
        brew: "tesseract",
        purpose: "--console vnc, GUI: Copy (OCR)",
    },
    #[cfg(target_os = "linux")]
    Dep {
        name: "WebKitGTK 4.1",
        probe: Probe::Lib("libwebkit2gtk-4.1.so.0"),
        apt: "libwebkit2gtk-4.1-0",
        brew: "",
        purpose: "GUI: window rendering",
    },
    #[cfg(target_os = "linux")]
    Dep {
        name: "GTK 3",
        probe: Probe::Lib("libgtk-3.so.0"),
        apt: "libgtk-3-0",
        brew: "",
        purpose: "GUI: window toolkit",
    },
];

fn present(probe: &Probe) -> bool {
    match probe {
        Probe::Bin(b) => std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(b).is_file()))
            .unwrap_or(false),
        #[cfg(target_os = "linux")]
        Probe::Lib(l) => std::process::Command::new("ldconfig")
            .arg("-p")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(l))
            .unwrap_or(false),
    }
}

fn check(install: bool) -> bool {
    let missing = report();
    if missing.is_empty() {
        return true;
    }
    if !install {
        println!("\nInstall with: {} {}", installer().0, missing.join(" "));
        return false;
    }
    println!("\nInstalling: {}", missing.join(" "));
    if let Err(e) = apt_install(&missing) {
        eprintln!("error: {e}");
        return false;
    }
    println!();
    report().is_empty()
}

/// Print the status of every dependency; returns the apt packages that are missing.
fn report() -> Vec<&'static str> {
    let mut missing = Vec::new();
    for d in DEPS {
        let ok = present(&d.probe);
        println!("[{}] {} ({})", if ok { "ok" } else { "MISSING" }, d.name, d.purpose);
        if !ok {
            missing.push(if cfg!(target_os = "macos") { d.brew } else { d.apt });
        }
    }
    missing
}

/// Package manager install command for this platform, as (display, program, args).
fn installer() -> (&'static str, &'static str, &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        ("brew install", "brew", &["install"])
    } else {
        ("sudo apt install", "apt-get", &["install", "-y"])
    }
}

/// Install packages with apt-get. sudo (if needed) prompts on the terminal
/// itself, so bobcat-cli never sees the sudo password.
fn apt_install(pkgs: &[&str]) -> Result<(), String> {
    use std::process::Command;
    let (shown, prog, args) = installer();
    if !present(&Probe::Bin(prog)) {
        return Err(format!("{prog} not found; install manually: {shown} {}", pkgs.join(" ")));
    }
    let root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    let mut cmd = if root || cfg!(target_os = "macos") {
        Command::new(prog)
    } else {
        let mut c = Command::new("sudo");
        c.arg(prog);
        c
    };
    let status = cmd
        .args(args)
        .args(pkgs)
        .status()
        .map_err(|e| format!("failed to run installer: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("installer exited with {status}"))
    }
}

/// Attach to the server's serial console over IPMI SOL by running
/// `ipmitool sol activate` on the terminal. The password reaches ipmitool
/// through its environment (-E), never on a command line.
fn console_sol(ip: &str, user: &str, pass: &str) -> Result<(), String> {
    use std::process::Command;
    if !present(&Probe::Bin("ipmitool")) {
        return Err(format!("ipmitool not found (install it with `{} ipmitool`, or bobcat-cli --check --install)", installer().0));
    }
    install_signal_handlers();
    eprintln!("Connecting to the serial console of {ip}. Press ~. to disconnect, ~? for help.");
    let mut child = Command::new("ipmitool")
        .args(["-I", "lanplus", "-H", ip, "-U", user, "-E", "sol", "activate"])
        .env("IPMI_PASSWORD", pass)
        .spawn()
        .map_err(|e| format!("failed to run ipmitool: {e}"))?;
    // ipmitool deactivates the SOL session itself when it exits normally (~.). If we
    // are told to stop (or the terminal closes and kills it), deactivate it for it.
    let deactivate = || {
        let _ = Command::new("ipmitool")
            .args(["-I", "lanplus", "-H", ip, "-U", user, "-E", "sol", "deactivate"])
            .env("IPMI_PASSWORD", pass)
            .output();
    };
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                if signaled() {
                    deactivate();
                    return Ok(());
                }
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "ipmitool exited with {status} (if a SOL session is already active, run: \
                         ipmitool -I lanplus -H {ip} -U {user} -E sol deactivate)"
                    ))
                };
            }
            None if signaled() => {
                deactivate();
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("\n[disconnected]");
                return Ok(());
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Set by SIGTERM/SIGHUP/SIGINT so a console can hang up cleanly instead of dying.
static SIGNALED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALED.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
}

fn signaled() -> bool {
    SIGNALED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Puts the terminal into raw mode and restores it when dropped.
struct RawMode(libc::termios);

impl RawMode {
    fn enter() -> Result<RawMode, String> {
        // SAFETY: plain libc termios calls on stdin with a zeroed, then filled, struct
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return Err("--console ssh needs an interactive terminal".into());
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                return Err("could not set raw terminal mode".into());
            }
            Ok(RawMode(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the attributes saved in enter()
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.0) };
    }
}

fn term_size() -> (u32, u32) {
    // SAFETY: TIOCGWINSZ fills a winsize struct
    unsafe {
        let mut w: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut w) == 0 && w.ws_col > 0 && w.ws_row > 0 {
            (w.ws_col as u32, w.ws_row as u32)
        } else {
            (80, 24)
        }
    }
}

/// End the console cleanly: send the iDRAC's quit key (Ctrl+\\) so it releases the
/// serial device, wait briefly, then close the channel and say goodbye.
fn ssh_goodbye(sess: &Session, ch: &mut ssh2::Channel, send_quit: bool) {
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
    let _ = sess.disconnect(Some(ssh2::DisconnectCode::ByApplication), "bobcat-cli closing", None);
}

/// Serial console through the iDRAC's SSH shell (`console com2`), with the
/// credentials handled in-process by ssh2 (nothing on a command line).
fn console_ssh(ip: &str, user: &str, pass: &str, trust_new: bool) -> Result<(), String> {
    /// Why the console loop ended
    enum End {
        /// user typed ~.
        Local,
        /// the server closed the session
        Remote,
        /// our terminal went away or we were told to stop (SIGTERM/SIGHUP/SIGINT)
        Signal,
    }
    install_signal_handlers();
    let sess = connect(ip, user, pass, trust_new)?;
    let mut ch = sess.channel_session().map_err(|e| e.to_string())?;
    let (cols, rows) = term_size();
    ch.request_pty("xterm", None, Some((cols, rows, 0, 0))).map_err(|e| e.to_string())?;
    ch.exec("console com2").map_err(|e| e.to_string())?;
    eprintln!("Connected to the serial console of {ip}. Press ~. (at the start of a line) or Ctrl+\\ to disconnect.");

    let raw = RawMode::enter()?;
    sess.set_blocking(false);
    let mut out = std::io::stdout();
    let mut buf = vec![0u8; 8192];
    let mut line_start = true; // for the ~. escape
    let mut tilde = false;
    let end = 'run: loop {
        if signaled() {
            break End::Signal;
        }
        // server -> terminal
        match ch.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                out.write_all(&buf[..n]).ok();
                out.flush().ok();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break End::Remote,
        }
        if ch.eof() {
            break End::Remote;
        }
        // terminal -> server (wait up to 10ms for a key)
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the duration of the call
        let ready = unsafe { libc::poll(&mut fd, 1, 10) };
        if ready > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let n = std::io::stdin().read(&mut buf).unwrap_or(0);
            if n == 0 {
                break End::Signal; // stdin closed: the terminal is gone
            }
            let mut send: Vec<u8> = Vec::new();
            for &b in &buf[..n] {
                if tilde {
                    tilde = false;
                    if b == b'.' {
                        break 'run End::Local;
                    }
                    send.push(b'~');
                }
                if line_start && b == b'~' {
                    tilde = true;
                    line_start = false;
                    continue;
                }
                line_start = b == b'\r' || b == b'\n';
                send.push(b);
            }
            let mut off = 0;
            while off < send.len() {
                match ch.write(&send[off..]) {
                    Ok(w) => off += w,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break 'run End::Remote,
                }
            }
        }
    };
    // Every way out hangs up properly so the iDRAC releases the serial device.
    ssh_goodbye(&sess, &mut ch, !matches!(end, End::Remote));
    drop(raw); // restore the terminal before printing
    match end {
        End::Local | End::Signal => eprintln!("\n[disconnected]"),
        End::Remote => eprintln!("\n[connection closed]"),
    }
    Ok(())
}

fn go(args: Args) -> Result<(), String> {
    if args.check {
        return if check(args.install) { Ok(()) } else { Err("required packages are missing".into()) };
    }
    // The iDRAC login is only needed for racadm over SSH, SOL and the SSH console;
    // --console vnc uses the VNC password instead.
    let needs_login = !args.enable.is_empty()
        || !args.disable.is_empty()
        || matches!(args.console, Some(Console::Sol) | Some(Console::Ssh));
    if args.forget {
        let login = keychain::forget(&args.user, &args.ip)?;
        let vnc = keychain::forget("", &args.ip)?;
        println!(
            "{}",
            match (login, vnc) {
                (true, true) => "Removed the saved login and VNC passwords.",
                (true, false) => "Removed the saved login password.",
                (false, true) => "Removed the saved VNC password.",
                (false, false) => "No saved passwords.",
            }
        );
        return Ok(());
    }
    let mut from_keychain = false;
    let pass = match args.pass.clone() {
        Some(p) => p,
        None if needs_login => match keychain::load(&args.user, &args.ip) {
            Some(p) => {
                from_keychain = true;
                p
            }
            None => rpassword::prompt_password(format!("Password for {}@{}: ", args.user, args.ip))
                .map_err(|e| e.to_string())?,
        },
        None => String::new(),
    };
    if args.enable.is_empty() && args.disable.is_empty() && args.console.is_none() {
        println!("Target: {}@{}", args.user, args.ip);
        return Ok(());
    }
    // VNC console: --vncpass / BOBCAT_VNC_PASS, then the keychain, then the prompt
    // (but not before --enable vnc, which asks for the new VNC password itself)
    let mut vnc_from_keychain = false;
    let vncpass = match (args.console, args.vncpass.clone()) {
        (Some(Console::Vnc), Some(p)) => p,
        (Some(Console::Vnc), None) if args.enable.contains(&Feature::Vnc) => String::new(),
        (Some(Console::Vnc), None) => match keychain::load("", &args.ip) {
            Some(p) => {
                vnc_from_keychain = true;
                p
            }
            None => rpassword::prompt_password("VNC password: ").map_err(|e| e.to_string())?,
        },
        _ => String::new(),
    };
    act(&args, &pass, &vncpass).map_err(|e| {
        if from_keychain || vnc_from_keychain {
            format!("{e}\n(a password came from the keychain; --forget removes it, --save stores a new one)")
        } else {
            e
        }
    })?;
    // Only a password that just worked is worth saving.
    if args.save {
        if needs_login && !from_keychain && !pass.is_empty() {
            keychain::save(&args.user, &args.ip, &pass)?;
            eprintln!("Saved the login password for {}@{} in the keychain.", args.user, args.ip);
        }
        if args.console == Some(Console::Vnc) && !vnc_from_keychain && !vncpass.is_empty() {
            keychain::save("", &args.ip, &vncpass)?;
            eprintln!("Saved the VNC password for {} in the keychain.", args.ip);
        }
    }
    Ok(())
}

fn act(args: &Args, pass: &str, vncpass: &str) -> Result<(), String> {
    if args.enable.contains(&Feature::Html5) && args.enable.contains(&Feature::Java) {
        return Err("--enable html5 and --enable java select different consoles; pick one".into());
    }
    if !args.enable.is_empty() || !args.disable.is_empty() {
        let sess = connect(&args.ip, &args.user, pass, args.trust_new_hostkey)?;
        configure(&sess, args)?;
    }
    match args.console {
        Some(Console::Sol) => console_sol(&args.ip, &args.user, pass)?,
        Some(Console::Ssh) => console_ssh(&args.ip, &args.user, pass, args.trust_new_hostkey)?,
        Some(Console::Vnc) => {
            // --enable vnc together with --console vnc and no --vncpass: ask now, after enabling
            let prompted;
            let vncpass = if vncpass.is_empty() {
                prompted = rpassword::prompt_password("VNC password: ").map_err(|e| e.to_string())?;
                prompted.as_str()
            } else {
                vncpass
            };
            vnc_text::run(&args.ip, args.port, !args.notls, vncpass, args.trust_new_hostkey, args.relearn)?
        }
        None => {}
    }
    Ok(())
}

fn configure(sess: &Session, args: &Args) -> Result<(), String> {
    for f in &args.enable {
        match f {
            Feature::Vnc => enable_vnc(sess, args.vncpass.as_deref(), args.port, args.sslebl, args.timeout)?,
            Feature::Html5 => enable_console(sess, 2)?,
            Feature::Java => enable_console(sess, 1)?,
            Feature::Sol => enable_sol(sess)?,
        }
    }
    for f in &args.disable {
        match f {
            Feature::Vnc => disable_vnc(sess)?,
            Feature::Html5 | Feature::Java | Feature::Sol => {
                return Err("--disable is only supported for vnc".into())
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match go(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
