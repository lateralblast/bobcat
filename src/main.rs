// bobcat-cli - iDRAC management command line tool
// Version: 1.8.6
// License: CC BY-NC-SA 4.0 (see LICENSE.md)

use std::io::{Read, Write};
use std::thread;
use std::net::TcpStream;
use std::process::ExitCode;
use std::time::Duration;

mod amt;
mod amt_redir;
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

/// What `--disconnect` ends on the iDRAC
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Disconnect {
    /// Show the iDRAC's sessions (racadm getssninfo) and end nothing
    List,
    /// Virtual media: the mapped ISO/image of a console session (HTML5/Java), a remote image, and
    /// any Virtual Media session
    #[value(alias = "iso", alias = "vm", alias = "virtualmedia")]
    Media,
    /// Web interface (GUI) sessions
    Gui,
    /// Virtual Console sessions (HTML5, Java)
    #[value(alias = "console", alias = "virtualconsole")]
    Vc,
    /// SSH sessions (the one this runs in cannot be closed and is left)
    Ssh,
    /// Serial console sessions (listed as SOL: ipmitool Serial over LAN and the SSH `console com2` console)
    Sol,
    /// Every other iDRAC session of any type with `racadm closessn -a`
    Sessions,
    /// Virtual media first, then all sessions
    All,
}

impl Disconnect {
    /// The session type as `racadm getssninfo` names it, for the types that close by session ID.
    fn session_type(self) -> Option<&'static str> {
        match self {
            Disconnect::Gui => Some("GUI"),
            Disconnect::Vc => Some("Virtual Console"),
            Disconnect::Ssh => Some("SSH"),
            Disconnect::Sol => Some("SOL"),
            _ => None,
        }
    }
}

/// Power actions for an Intel AMT device (`--amt --power`)
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Power {
    /// Show the power state
    Status,
    /// Power on
    On,
    /// Soft power off
    Off,
    /// Hard power off
    OffHard,
    /// Power cycle
    Cycle,
    /// Reset
    Reset,
    /// Sleep (deep)
    Sleep,
    /// Hibernate
    Hibernate,
    /// Send a non-maskable interrupt (diagnostic)
    Nmi,
}

impl Power {
    /// AMT's CIM_PowerManagementService number for the action.
    fn amt_state(self) -> Option<u32> {
        match self {
            Power::Status => None,
            Power::On => Some(2),
            Power::Sleep => Some(4),
            Power::Cycle => Some(5),
            Power::OffHard => Some(6),
            Power::Hibernate => Some(7),
            Power::Off => Some(8),
            Power::Reset => Some(10),
            Power::Nmi => Some(11),
        }
    }
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

    /// With --check, install any missing packages (apt-get with sudo on Linux, Homebrew on macOS; also Rosetta 2 on Apple Silicon)
    #[arg(long, requires = "check")]
    install: bool,

    /// IP address of the iDRAC
    #[arg(long, required_unless_present = "check", default_value = "")]
    ip: String,

    /// iDRAC username
    #[arg(long, required_unless_present = "check", default_value = "")]
    user: String,

    /// iDRAC password, or with --amt the AMT admin password (prefer the BOBCAT_PASS environment
    /// variable or the interactive prompt: command-line arguments are visible in `ps`)
    #[arg(long = "pass", env = "BOBCAT_PASS", hide_env_values = true)]
    pass: Option<String>,

    /// After success, save the iDRAC login password (sol, ssh, --enable/--disable) and the VNC
    /// password (--console vnc) in the OS keychain; they are used automatically next time
    #[arg(long)]
    save: bool,

    /// Remove the saved keychain passwords (login for --user, and VNC) for --ip, then exit
    #[arg(long)]
    forget: bool,

    /// VNC password to set with --enable vnc, or with --amt the AMT KVM's RFB password (exactly 8
    /// characters: upper, lower, digit, special); visible in `ps`, so prefer the BOBCAT_VNC_PASS
    /// environment variable or the interactive prompt
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

    /// Manage an Intel AMT (vPro) device instead of an iDRAC: --ip is the AMT address and --user
    /// its admin user (usually admin). With --amt: --info, --power, --enable vnc / --disable vnc
    /// (the AMT KVM, an RFB server on 5900) and --console vnc (port 5900, no TLS) work
    #[arg(long)]
    amt: bool,

    /// With --amt, the AMT web-services port (16992 is plain HTTP; TLS on 16993 is not supported yet)
    #[arg(long, default_value_t = 16992)]
    amt_port: u16,

    /// With --amt, show what the device reports (firmware, power, provisioning, KVM, redirection)
    #[arg(long, requires = "amt")]
    info: bool,

    /// With --amt, show or change the power state
    #[arg(long, value_enum, requires = "amt")]
    power: Option<Power>,

    /// With --amt, try several shapes of the KVM settings Put and print the device's answer to each
    /// (changes the RFB password; for finding what a firmware accepts)
    #[arg(long, requires = "amt", hide = true)]
    amt_diagnose_kvm: bool,

    /// With --amt, open an AMT redirection session (SOL, IDER or KVMR) on port 16994, authenticate and
    /// show what the device sends next (for checking the redirection protocol)
    #[arg(long, requires = "amt", hide = true, value_name = "SOL|IDER|KVMR|BRIDGE")]
    amt_redir_test: Option<String>,

    /// Do not ask before a power change
    #[arg(long, requires = "power")]
    yes: bool,

    /// Open a text console to the server (sol needs ipmitool)
    #[arg(long, value_enum)]
    console: Option<Console>,

    /// Enable iDRAC features via racadm over SSH
    #[arg(long, value_enum, value_delimiter = ',')]
    enable: Vec<Feature>,

    /// Disable iDRAC features via racadm over SSH
    #[arg(long, value_enum, value_delimiter = ',')]
    disable: Vec<Feature>,

    /// End sessions on the iDRAC via racadm over SSH: list shows them, media (alias iso, vm)
    /// disconnects virtual media (e.g. after a console was closed with an image still mapped), gui,
    /// vc (virtual console), ssh and sol close the sessions of that type, sessions closes all other
    /// sessions, all does media and sessions
    #[arg(long, value_enum, value_delimiter = ',')]
    disconnect: Vec<Disconnect>,
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

/// Saved SSH host key fingerprints and TLS certificate pins. Shared with bobcat: same file,
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
    /// Executable found in PATH (on macOS also in the Homebrew folders, which a GUI-launched
    /// or minimal PATH may leave out)
    Bin(&'static str),
    /// Shared library soname known to the dynamic linker (Linux only)
    #[cfg(target_os = "linux")]
    Lib(&'static str),
    /// Rosetta 2, which runs the Intel-only Java viewer on Apple Silicon
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Rosetta,
    /// A Java runtime for the GUI's Java console: BOBCAT_JAVA, the Java 8 the GUI downloaded into
    /// ~/.config/bobcat/java (macOS), or `java` from the PATH (elsewhere)
    Java,
}

struct Dep {
    name: &'static str,
    probe: Probe,
    /// Debian/Ubuntu package that provides it
    apt: &'static str,
    /// Fedora/RHEL (dnf) package
    dnf: &'static str,
    /// openSUSE (zypper) package
    zypper: &'static str,
    /// Homebrew formula (macOS)
    brew: &'static str,
    purpose: &'static str,
    /// Optional: shown, but a miss does not fail --check; `hint` says what to do about it
    optional: bool,
    hint: &'static str,
}

const DEPS: &[Dep] = &[
    Dep {
        name: "ipmitool",
        probe: Probe::Bin("ipmitool"),
        apt: "ipmitool",
        dnf: "ipmitool",
        zypper: "ipmitool",
        brew: "ipmitool",
        purpose: "--console sol",
        optional: false,
        hint: "",
    },
    Dep {
        name: "tesseract",
        probe: Probe::Bin("tesseract"),
        apt: "tesseract-ocr",
        dnf: "tesseract",
        zypper: "tesseract-ocr",
        brew: "tesseract",
        purpose: "--console vnc, GUI: Copy (OCR)",
        optional: false,
        hint: "",
    },
    #[cfg(target_os = "linux")]
    Dep {
        name: "WebKitGTK 4.1",
        probe: Probe::Lib("libwebkit2gtk-4.1.so.0"),
        apt: "libwebkit2gtk-4.1-0",
        dnf: "webkit2gtk4.1",
        zypper: "libwebkit2gtk-4_1-0",
        brew: "",
        purpose: "GUI: window rendering",
        optional: false,
        hint: "",
    },
    #[cfg(target_os = "linux")]
    Dep {
        name: "GTK 3",
        probe: Probe::Lib("libgtk-3.so.0"),
        apt: "libgtk-3-0",
        dnf: "gtk3",
        zypper: "libgtk-3-0",
        brew: "",
        purpose: "GUI: window toolkit",
        optional: false,
        hint: "",
    },
    #[cfg(target_os = "macos")]
    Dep {
        name: "Homebrew",
        probe: Probe::Bin("brew"),
        apt: "",
        dnf: "",
        zypper: "",
        brew: "",
        purpose: "needed only for --install",
        optional: true,
        hint: "install it from https://brew.sh",
    },
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Dep {
        name: "Rosetta 2",
        probe: Probe::Rosetta,
        apt: "",
        dnf: "",
        zypper: "",
        brew: "",
        purpose: "GUI: Java console (Intel-only viewer)",
        optional: true,
        hint: "softwareupdate --install-rosetta --agree-to-license (or --check --install)",
    },
    #[cfg(target_os = "linux")]
    Dep {
        name: "rpmbuild",
        probe: Probe::Bin("rpmbuild"),
        apt: "rpm",
        dnf: "rpm-build",
        zypper: "rpm-build",
        brew: "",
        purpose: "build.sh --package rpm",
        optional: true,
        hint: "sudo apt install rpm (Fedora/openSUSE: rpm-build), or --check --install",
    },
    Dep {
        name: "Java runtime",
        probe: Probe::Java,
        apt: "",
        dnf: "",
        zypper: "",
        brew: "",
        purpose: "GUI: Java console",
        optional: true,
        hint: if cfg!(target_os = "macos") {
            "the GUI downloads Java 8 on first use (or set BOBCAT_JAVA)"
        } else {
            "install Java 8 or later (apt: default-jre, dnf: java-latest-openjdk, zypper: java-17-openjdk) or set BOBCAT_JAVA"
        },
    },
];

fn config_dir() -> std::path::PathBuf {
    pins_path().parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn present(probe: &Probe) -> bool {
    match probe {
        Probe::Bin(b) => {
            let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
            if cfg!(target_os = "macos") {
                dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(std::path::PathBuf::from));
            }
            dirs.iter().any(|d| d.join(b).is_file())
        }
        #[cfg(target_os = "linux")]
        // ldconfig is in /sbin, which a normal user's PATH lacks on Fedora and openSUSE
        Probe::Lib(l) => ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"].iter().any(|ld| {
            std::process::Command::new(ld)
                .arg("-p")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(l))
                .unwrap_or(false)
        }),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Probe::Rosetta => std::path::Path::new("/Library/Apple/usr/share/rosetta/rosetta").exists(),
        Probe::Java => {
            if let Some(p) = std::env::var_os("BOBCAT_JAVA") {
                return std::path::Path::new(&p).exists();
            }
            if cfg!(target_os = "macos") {
                std::fs::read_dir(config_dir().join("java"))
                    .map(|d| d.flatten().any(|e| e.path().join("Contents/Home/bin/java").exists()))
                    .unwrap_or(false)
            } else {
                present(&Probe::Bin("java"))
            }
        }
    }
}

fn check(install: bool) -> bool {
    let (mut missing, optional, extra) = report();
    // optional items that a package provides are installed too, when asked to
    if install {
        missing.extend(extra);
    }
    // Rosetta 2 is the one optional item bobcat-cli can install (it is not a package manager package)
    let rosetta = optional.contains(&"Rosetta 2");
    if missing.is_empty() && !(install && rosetta) {
        return true;
    }
    if !install {
        if !missing.is_empty() {
            println!("\nInstall with: {} {}", installer().0, missing.join(" "));
        }
        return missing.is_empty();
    }
    if !missing.is_empty() {
        println!("\nInstalling: {}", missing.join(" "));
        if let Err(e) = apt_install(&missing) {
            eprintln!("error: {e}");
            return false;
        }
    }
    if rosetta {
        println!("\nInstalling Rosetta 2 (softwareupdate may ask for your password)");
        let st = std::process::Command::new("softwareupdate").args(["--install-rosetta", "--agree-to-license"]).status();
        if !st.is_ok_and(|s| s.success()) {
            eprintln!("error: could not install Rosetta 2");
        }
    }
    println!();
    report().0.is_empty()
}

/// Print the status of every dependency; returns the packages that are missing (required), the
/// names of the missing optional ones and the packages that would provide those.
fn report() -> (Vec<&'static str>, Vec<&'static str>, Vec<&'static str>) {
    let mut missing = Vec::new();
    let mut optional = Vec::new();
    let mut extra = Vec::new();
    for d in DEPS {
        let ok = present(&d.probe);
        let tag = if ok { "ok" } else if d.optional { "--" } else { "MISSING" };
        println!("[{tag}] {} ({}){}", d.name, d.purpose, if !ok && d.optional { format!(": {}", d.hint) } else { String::new() });
        if !ok {
            if d.optional {
                optional.push(d.name);
                let pkg = pkg_name(d);
                if !pkg.is_empty() {
                    extra.push(pkg);
                }
            } else {
                missing.push(pkg_name(d));
            }
        }
    }
    (missing, optional, extra)
}

#[derive(Clone, Copy, PartialEq)]
enum PkgMgr {
    Apt,
    Dnf,
    Zypper,
    Brew,
}

/// The platform's package manager: Homebrew on macOS; on Linux from `ID`/`ID_LIKE` in /etc/os-release
/// (Debian-like: apt, Fedora/RHEL-like: dnf, openSUSE-like: zypper), else whichever is installed.
fn pkg_mgr() -> PkgMgr {
    if cfg!(target_os = "macos") {
        return PkgMgr::Brew;
    }
    let ids = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("ID=") || l.starts_with("ID_LIKE="))
        .flat_map(|l| l.split_once('=').map(|(_, v)| v.trim_matches('"').to_string()))
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    pkg_mgr_from(&ids, |b| present(&Probe::Bin(b)))
}

fn pkg_mgr_from(ids: &[String], have: impl Fn(&'static str) -> bool) -> PkgMgr {
    for id in ids {
        match id.as_str() {
            "debian" | "ubuntu" => return PkgMgr::Apt,
            "fedora" | "rhel" | "centos" => return PkgMgr::Dnf,
            "suse" | "opensuse" | "sles" => return PkgMgr::Zypper,
            _ => {}
        }
    }
    if have("apt-get") {
        PkgMgr::Apt
    } else if have("dnf") {
        PkgMgr::Dnf
    } else if have("zypper") {
        PkgMgr::Zypper
    } else {
        PkgMgr::Apt
    }
}

fn pkg_name(d: &Dep) -> &'static str {
    match pkg_mgr() {
        PkgMgr::Brew => d.brew,
        PkgMgr::Apt => d.apt,
        PkgMgr::Dnf => d.dnf,
        PkgMgr::Zypper => d.zypper,
    }
}

/// Package manager install command for this platform, as (display, program, args).
fn installer() -> (&'static str, &'static str, &'static [&'static str]) {
    match pkg_mgr() {
        PkgMgr::Brew => ("brew install", "brew", &["install"]),
        PkgMgr::Apt => ("sudo apt install", "apt-get", &["install", "-y"]),
        PkgMgr::Dnf => ("sudo dnf install", "dnf", &["install", "-y"]),
        PkgMgr::Zypper => ("sudo zypper install", "zypper", &["--non-interactive", "install"]),
    }
}

/// Install packages with the platform package manager (apt-get, dnf, zypper or brew). sudo (if needed) prompts on the terminal
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
    // tell the iDRAC when the local terminal is resized (a window-change request; the channel is
    // non-blocking, so a request that would block is kept and retried on the next pass)
    let mut size = (cols, rows);
    let mut resize_pending = false;
    let end = 'run: loop {
        if signaled() {
            break End::Signal;
        }
        let now = term_size();
        if now != size {
            size = now;
            resize_pending = true;
        }
        if resize_pending {
            resize_pending = ch
                .request_pty_size(size.0, size.1, None, None)
                .err()
                .is_some_and(|e| std::io::Error::from(e).kind() == std::io::ErrorKind::WouldBlock);
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
    let needs_login = args.amt
        || !args.enable.is_empty()
        || !args.disable.is_empty()
        || !args.disconnect.is_empty()
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
    if !args.amt && args.enable.is_empty() && args.disable.is_empty() && args.disconnect.is_empty() && args.console.is_none() {
        println!("Target: {}@{}", args.user, args.ip);
        return Ok(());
    }
    // VNC console: --vncpass / BOBCAT_VNC_PASS, then the keychain, then the prompt
    // (but not before --enable vnc, which asks for the new VNC password itself)
    let mut vnc_from_keychain = false;
    let vncpass = match (args.console, args.vncpass.clone()) {
        (Some(Console::Vnc), Some(p)) => p,
        (Some(Console::Vnc), None) if args.enable.contains(&Feature::Vnc) || args.amt => String::new(),
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
    let mut amt_rfb: Option<String> = None;
    if args.amt {
        amt_rfb = amt_act(args, pass)?;
    } else if !args.enable.is_empty() || !args.disable.is_empty() || !args.disconnect.is_empty() {
        let sess = connect(&args.ip, &args.user, pass, args.trust_new_hostkey)?;
        configure(&sess, args)?;
        disconnect(&sess, &args.disconnect)?;
    }
    match args.console {
        Some(Console::Sol) | Some(Console::Ssh) if args.amt => {
            return Err("with --amt only --console vnc works (the AMT KVM); SOL needs the 16994 redirection protocol, which is not supported yet".into());
        }
        Some(Console::Sol) => console_sol(&args.ip, &args.user, pass)?,
        Some(Console::Ssh) => console_ssh(&args.ip, &args.user, pass, args.trust_new_hostkey)?,
        Some(Console::Vnc) => {
            // --enable vnc together with --console vnc and no --vncpass: ask now, after enabling
            let prompted;
            let vncpass = if let Some(p) = amt_rfb.as_deref().filter(|_| vncpass.is_empty()) {
                p
            } else if vncpass.is_empty() && args.amt {
                "" // the bridged AMT KVM needs no VNC password
            } else if vncpass.is_empty() {
                prompted = rpassword::prompt_password("VNC password: ").map_err(|e| e.to_string())?;
                prompted.as_str()
            } else {
                vncpass
            };
            // the AMT KVM is reached through the redirection port 16994 and bridged to a local port
            // (the device's own 5900 port could not be enabled on the test device); no VNC password
            let (host, port, tls) = if args.amt {
                let local = amt_redir::kvm_bridge(&args.ip, 16994, &args.user, pass)?;
                println!("AMT KVM bridged to 127.0.0.1:{local}");
                ("127.0.0.1".to_string(), local, false)
            } else {
                (args.ip.clone(), args.port, !args.notls)
            };
            vnc_text::run(&host, port, tls, vncpass, args.trust_new_hostkey, args.relearn)?
        }
        None => {}
    }
    Ok(())
}

/// Run a racadm command and show what it said; a failure is shown, not fatal (for example
/// `vmdisconnect` has nothing to do when no virtual media is connected).
fn run_show(sess: &Session, cmd: &str) {
    match run(sess, cmd) {
        Ok(out) => println!("[ok]   {cmd}{}", if out.trim().is_empty() { String::new() } else { format!(": {}", out.trim()) }),
        Err(e) => println!("[--]   {e}"),
    }
}

/// One row of `racadm getssninfo`.
struct IdracSession {
    id: String,
    kind: String,
    user: String,
}

/// The iDRAC's sessions. The table is fixed-width text: `SSNID Type User IP Date`, where the type
/// can hold single spaces ("Virtual Console") and columns are separated by two or more.
fn sessions(sess: &Session) -> Result<Vec<IdracSession>, String> {
    let out = run(sess, "racadm getssninfo")?;
    let mut list = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        let Some((id, rest)) = line.trim_start().split_once(char::is_whitespace) else { continue };
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            continue; // header, dashes, blank
        }
        let rest = rest.trim_start();
        let (kind, after) = match rest.find("  ") {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None => (rest, ""),
        };
        let user = after.split_whitespace().next().unwrap_or("");
        list.push(IdracSession { id: id.to_string(), kind: kind.trim().to_string(), user: user.to_string() });
    }
    Ok(list)
}

/// Close every session of one type, by session ID.
fn close_type(sess: &Session, kind: &str) -> Result<(), String> {
    let found: Vec<IdracSession> = sessions(sess)?.into_iter().filter(|s| s.kind.eq_ignore_ascii_case(kind)).collect();
    if found.is_empty() {
        println!("[--]   no {kind} session");
    }
    for s in found {
        println!("closing session {} ({}, user {})", s.id, s.kind, s.user);
        run_show(sess, &format!("racadm closessn -i {}", s.id));
    }
    Ok(())
}

/// `--disconnect`: end virtual media and/or sessions.
fn disconnect(sess: &Session, what: &[Disconnect]) -> Result<(), String> {
    let all = what.contains(&Disconnect::All);
    if what.contains(&Disconnect::List) {
        print!("{}", run(sess, "racadm getssninfo")?);
    }
    if all || what.contains(&Disconnect::Media) {
        // client virtual media (the image a console mapped), then a network remote image,
        // then any session the iDRAC still lists as Virtual Media
        run_show(sess, "racadm vmdisconnect");
        run_show(sess, "racadm remoteimage -d");
        if sessions(sess)?.iter().any(|s| s.kind.eq_ignore_ascii_case("Virtual Media")) {
            close_type(sess, "Virtual Media")?;
        }
    }
    for w in what {
        if let Some(kind) = w.session_type() {
            close_type(sess, kind)?;
        }
    }
    if all || what.contains(&Disconnect::Sessions) {
        // closes every session but the one this runs in
        run_show(sess, "racadm closessn -a");
    }
    Ok(())
}

/// Everything `--amt` asks for. Returns the RFB password it set, if any (for `--console vnc`).
fn amt_act(args: &Args, pass: &str) -> Result<Option<String>, String> {
    if !args.disconnect.is_empty() {
        return Err("--disconnect is for iDRAC sessions, not --amt".into());
    }
    if args.enable.iter().chain(&args.disable).any(|f| *f != Feature::Vnc) {
        return Err("with --amt only --enable vnc / --disable vnc apply (the AMT KVM)".into());
    }
    let device = amt::Amt::new(&args.ip, args.amt_port, &args.user, pass);
    let nothing = !args.info && !args.amt_diagnose_kvm && args.amt_redir_test.is_none() && args.power.is_none() && args.enable.is_empty() && args.disable.is_empty() && args.console.is_none();
    if args.info || nothing {
        device.info()?;
    }
    if let Some(p) = args.power {
        match p.amt_state() {
            None => println!("power: {}", amt::power_name(device.power_state()?)),
            Some(state) => {
                if p != Power::On && !args.yes {
                    let name = p.to_possible_value().map(|v| v.get_name().to_string()).unwrap_or_default();
                    let ans = rpassword::prompt_password(format!("Really send power action `{name}` to {}? Type yes: ", args.ip)).map_err(|e| e.to_string())?;
                    if ans.trim() != "yes" {
                        return Err("stopped".into());
                    }
                }
                device.set_power(state)?;
                println!("power change requested (state {state}); the device is now {}", amt::power_name(device.power_state()?));
            }
        }
    }
    if let Some(p) = &args.amt_redir_test {
        if p.eq_ignore_ascii_case("bridge") {
            // the KVM bridge on its own, for any VNC viewer
            if device.is_this_machine() {
                eprintln!("warning: the AMT device reports this machine's host name; a viewer on this machine shows its own window inside the captured screen (cascading copies). View it from another machine or on a second display.");
            }
            let local = amt_redir::kvm_bridge(&args.ip, 16994, &args.user, pass)?;
            println!("AMT KVM bridged to 127.0.0.1:{local}: connect a VNC viewer there (Ctrl+C stops)");
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }
        let proto: &[u8; 4] = match p.to_ascii_uppercase().as_str() {
            "SOL" => b"SOL ",
            "IDER" => b"IDER",
            "KVMR" | "KVM" => b"KVMR",
            _ => return Err("--amt-redir-test takes SOL, IDER, KVMR or BRIDGE".into()),
        };
        amt_redir::test(&args.ip, 16994, proto, &args.user, pass)?;
        return Ok(None);
    }
    if args.amt_diagnose_kvm {
        let pw = match args.vncpass.clone() {
            Some(p) => p,
            None => rpassword::prompt_password("RFB password to try (8 characters: upper, lower, digit, special): ").map_err(|e| e.to_string())?,
        };
        amt::check_rfb_password(&pw)?;
        device.kvm_diagnose(&pw)?;
        return Ok(None);
    }
    let mut rfb = None;
    if args.enable.contains(&Feature::Vnc) {
        let pw = match args.vncpass.clone() {
            Some(p) => {
                amt::check_rfb_password(&p)?;
                p
            }
            None => {
                // typed without echo, so ask twice and say what is wrong; three tries
                println!("The AMT KVM (VNC) needs its own password, not the admin one: exactly 8 characters with an upper-case letter, a lower-case letter, a digit and a special character.");
                let mut chosen = None;
                for _ in 0..3 {
                    let p = rpassword::prompt_password("New VNC (RFB) password: ").map_err(|e| e.to_string())?;
                    if let Err(e) = amt::check_rfb_password(&p) {
                        println!("{e}");
                        continue;
                    }
                    if rpassword::prompt_password("Repeat it: ").map_err(|e| e.to_string())? != p {
                        println!("the two entries differ");
                        continue;
                    }
                    chosen = Some(p);
                    break;
                }
                chosen.ok_or("no valid VNC password entered")?
            }
        };
        device.kvm_enable(&pw)?;
        println!("[changed] AMT KVM enabled: VNC on {}:5900 with the new password", args.ip);
        rfb = Some(pw);
    }
    if args.disable.contains(&Feature::Vnc) {
        device.kvm_disable()?;
        println!("[changed] AMT KVM disabled (port 5900 off)");
    }
    Ok(rfb)
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
