// The iDRAC Java virtual console, run as an external viewer.
//
// The iDRAC serves the viewer as a Java Web Start file (viewer.jnlp). Web Start is gone from
// current Java, so the launch file is only read: its arguments and jar names are taken from it,
// the jars are downloaded over the pinned connection and `java` is started on them directly.
// See RESEARCH.md ("Java console") for why the runtime, the patched library path and the TLS
// settings below are needed.

use crate::html5::{self, request, WebSession};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const MAIN_CLASS: &str = "com.avocent.idrac.kvm.Main";

/// Process id of the running viewer. The thread that waits for it owns the child itself.
static VIEWER_PID: Mutex<Option<u32>> = Mutex::new(None);

fn config_dir() -> PathBuf {
    let mut p = crate::pins_path();
    p.pop();
    p
}

/// Where the downloaded Java runtime lives (macOS only).
fn runtime_dir() -> PathBuf {
    config_dir().join("java")
}

fn viewer_dir(ip: &str) -> PathBuf {
    config_dir().join("java-viewer").join(ip)
}

/// The `java` to run the viewer with: BOBCAT_JAVA (the path of the binary), else the runtime
/// this program downloaded (macOS), else `java` from the PATH (elsewhere).
fn find_java() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BOBCAT_JAVA") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    #[cfg(target_os = "macos")]
    {
        for e in std::fs::read_dir(runtime_dir()).ok()?.flatten() {
            let java = e.path().join("Contents/Home/bin/java");
            if java.exists() {
                return Some(java);
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let ok = Command::new("java").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        ok.then(|| PathBuf::from("java"))
    }
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("curl").args(["-fsS", "--max-time", "900"]).args(args).output().map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// Download Azul Zulu 8 (x86_64 JRE) into the runtime directory. The viewer's native libraries for
/// macOS are Intel-only, so an x86_64 Java is needed even on Apple Silicon (it runs under Rosetta).
/// The archive's SHA-256 is checked against the value Azul publishes before it is unpacked.
#[cfg(target_os = "macos")]
fn download_runtime(app: &AppHandle) -> Result<(), String> {
    if cfg!(target_arch = "aarch64") && !Path::new("/Library/Apple/usr/share/rosetta/rosetta").exists() {
        return Err("Rosetta 2 is needed to run the Intel Java (softwareupdate --install-rosetta --agree-to-license)".into());
    }
    crate::status(app, "looking up the Java runtime...");
    let api = "https://api.azul.com/metadata/v1/zulu/packages/";
    let list = curl(&[&format!("{api}?java_version=8&os=macos&arch=x64&archive_type=tar.gz&java_package_type=jre&latest=true&release_status=ga&availability_types=CA&page=1&page_size=1")])?;
    let list: serde_json::Value = serde_json::from_slice(&list).map_err(|e| format!("unexpected answer from Azul: {e}"))?;
    let pkg = list.get(0).ok_or("Azul lists no Java 8 runtime for macOS")?;
    let (url, name, uuid) = (
        pkg["download_url"].as_str().ok_or("no download URL")?,
        pkg["name"].as_str().ok_or("no file name")?,
        pkg["package_uuid"].as_str().ok_or("no package id")?,
    );
    if !url.starts_with("https://cdn.azul.com/") {
        return Err(format!("unexpected download address {url}"));
    }
    let detail = curl(&[&format!("{api}{uuid}")])?;
    let detail: serde_json::Value = serde_json::from_slice(&detail).map_err(|e| format!("unexpected answer from Azul: {e}"))?;
    let want = detail["sha256_hash"].as_str().ok_or("Azul gave no checksum")?.to_lowercase();

    let dir = runtime_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let archive = dir.join(name);
    crate::status(app, format!("downloading {name} (about 100 MB)..."));
    curl(&["-o", &archive.to_string_lossy(), url])?;
    let data = std::fs::read(&archive).map_err(|e| e.to_string())?;
    let got = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), &data)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != want {
        let _ = std::fs::remove_file(&archive);
        return Err("the downloaded Java runtime does not match its published checksum; not installed".into());
    }
    crate::status(app, "unpacking the Java runtime...");
    let st = Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&dir).status().map_err(|e| format!("could not run tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !st.success() {
        return Err("could not unpack the Java runtime".into());
    }
    let java = find_java().ok_or("the unpacked runtime has no java binary")?;
    let ok = Command::new(&java).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !ok {
        return Err(format!("{} does not run", java.display()));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn download_runtime(_app: &AppHandle) -> Result<(), String> {
    Err("the Java runtime is only downloaded on macOS; install Java 8 or later (or set BOBCAT_JAVA)".into())
}

/// Called by the page after the user agreed to the download.
#[tauri::command]
pub fn download_java_runtime(app: AppHandle) {
    std::thread::spawn(move || match download_runtime(&app) {
        Ok(()) => {
            crate::status(&app, "Java runtime installed");
            let _ = app.emit("java-runtime-ready", ());
        }
        Err(e) => crate::status(&app, format!("error: {e}")),
    });
}

/// `<argument>` values of the launch file, in order.
fn arguments(jnlp: &str) -> Vec<String> {
    jnlp.split("<argument>").skip(1).filter_map(|p| p.split("</argument>").next()).map(str::to_string).collect()
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let i = tag.find(&key)? + key.len();
    tag[i..].split('"').next()
}

/// Path on the iDRAC (`/software/...`) of a jar address from the launch file.
fn software_path(href: &str) -> Option<String> {
    href.find("/software/").map(|i| href[i..].to_string())
}

/// The main jar and the native-library jars for this computer, as paths on the iDRAC.
fn jar_paths(jnlp: &str) -> Result<(String, Vec<String>), String> {
    let mut main = None;
    let mut natives = Vec::new();
    let (os, archs): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("Mac OS X", &["x86_64"])
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("Linux", &["x86_64", "amd64"])
    } else {
        return Err("the Java console is only supported on macOS and 64-bit Linux".into());
    };
    for block in jnlp.split("<resources").skip(1) {
        let (head, body) = block.split_once('>').unwrap_or((block, ""));
        let body = body.split("</resources>").next().unwrap_or("");
        let applies = match (attr(head, "os"), attr(head, "arch")) {
            (None, None) => true,
            (Some(o), Some(a)) => o == os && archs.contains(&a),
            _ => false,
        };
        if !applies {
            continue;
        }
        for tag in body.split("<").skip(1) {
            let Some(href) = attr(tag, "href").and_then(software_path) else { continue };
            if tag.starts_with("nativelib") {
                natives.push(href);
            } else if tag.starts_with("jar") && attr(tag, "main") == Some("true") {
                main = Some(href);
            }
        }
    }
    Ok((main.ok_or("the launch file names no viewer jar")?, natives))
}

/// macOS: the virtual-media library calls dlopen("/usr/lib/libssl.dylib") and is written to fall
/// back to its own built-in copy when that fails. On macOS the call does not fail: Apple's stub
/// library kills the whole process ("loading libcrypto in an unsafe way"). Point the paths at
/// names that do not exist (same length) so the load fails normally and the fallback is used.
/// Only the downloaded copy in this program's cache is changed.
fn patch_openssl_paths(dir: &Path) -> Result<(), String> {
    const PATCHES: [(&[u8], &[u8]); 2] = [
        (b"/usr/lib/libssl.dylib", b"/usr/lib/libssl.dyl_x"),
        (b"/usr/lib/libcrypto.dylib", b"/usr/lib/libcrypto.dyl_x"),
    ];
    for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "jnilib") {
            continue;
        }
        let mut data = std::fs::read(&path).map_err(|e| e.to_string())?;
        let mut changed = false;
        for (from, to) in PATCHES {
            let mut i = 0;
            while i + from.len() <= data.len() {
                if &data[i..i + from.len()] == from {
                    data[i..i + from.len()].copy_from_slice(to);
                    changed = true;
                    i += from.len();
                } else {
                    i += 1;
                }
            }
        }
        if changed {
            std::fs::write(&path, data).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Where the viewer keeps the certificates the user chose to trust ("User's Certificate Store
/// Path" in its Session Options). One folder for all iDRACs, kept between connections.
fn cert_store_dir() -> PathBuf {
    config_dir().join("java-trust")
}

/// Put the iDRAC's certificate into the viewer's store, named the way the viewer names what it
/// saves from "Always trust this certificate" (`EMR_<sha256 of the issuer in OpenSSL's one-line
/// form>_<serial>.pem`). Bobcat has already checked the certificate against its pin (trust on
/// first use; a changed certificate is refused before this point), so the viewer's own warning
/// would only repeat that. A changed certificate is never added, so the viewer still warns.
fn trust_in_viewer(ip: &str, store: &Path) -> Result<(), String> {
    use openssl::{hash::{hash, MessageDigest}, x509::X509};
    let pem = html5::pinned_peer_pem(ip).ok_or("no pinned certificate for this iDRAC")?;
    let cert = X509::from_pem(&pem).map_err(|e| e.to_string())?;
    let mut issuer = String::new();
    for e in cert.issuer_name().entries() {
        let name = e.object().nid().short_name().map_err(|e| e.to_string())?;
        let value = e.data().as_utf8().map_err(|e| e.to_string())?;
        issuer.push_str(&format!("/{name}={value}"));
    }
    let digest = hash(MessageDigest::sha256(), issuer.as_bytes()).map_err(|e| e.to_string())?;
    let hex: String = digest.iter().map(|b| format!("{b:02X}")).collect();
    let serial = cert.serial_number().to_bn().and_then(|b| b.to_hex_str().map(|s| s.to_string())).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(store).map_err(|e| e.to_string())?;
    let file = store.join(format!("EMR_{hex}_{serial}.pem"));
    if std::fs::read(&file).ok().as_deref() != Some(&pem) {
        std::fs::write(&file, &pem).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The viewer shows a warning dialog when the iDRAC's certificate does not chain to a root it
/// knows, and only offers "Always trust this certificate" once a certificate store path is set in
/// its preferences. Set that preference (macOS: `defaults`/PlistBuddy on com.avocent.viewer) to
/// bobcat's folder, unless the user has chosen a path of their own.
#[cfg(target_os = "macos")]
fn set_viewer_cert_store(store: &Path) -> Result<(), String> {
    const KEY: &str = ":/com/avocent/viewer/:dell/:USER_CERT_STORE_PATH";
    const PLISTBUDDY: &str = "/usr/libexec/PlistBuddy";
    std::fs::create_dir_all(store).map_err(|e| e.to_string())?;
    let want = store.display().to_string();
    let exported = Command::new("defaults").args(["export", "com.avocent.viewer", "-"]).output().map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join(format!("bobcat-viewer-prefs-{}.plist", std::process::id()));
    std::fs::write(&tmp, &exported.stdout).map_err(|e| e.to_string())?;
    let buddy = |cmd: &str| Command::new(PLISTBUDDY).arg("-c").arg(cmd).arg(&tmp).output();
    let result = (|| {
        let current = buddy(&format!("Print {KEY}")).map_err(|e| e.to_string())?;
        let current = String::from_utf8_lossy(&current.stdout).trim().to_string();
        if current == want {
            return Ok(());
        }
        if !current.is_empty() {
            return Ok(()); // the user's own path: leave it alone
        }
        // the nodes may not exist yet (errors for existing ones are expected)
        let _ = buddy("Add :/com/avocent/viewer/ dict");
        let _ = buddy("Add :/com/avocent/viewer/:dell/ dict");
        let _ = buddy(&format!("Delete {KEY}"));
        let added = buddy(&format!("Add {KEY} string {want}")).map_err(|e| e.to_string())?;
        if !added.status.success() {
            return Err(format!("could not edit the viewer preferences: {}", String::from_utf8_lossy(&added.stderr).trim()));
        }
        let st = Command::new("defaults").args(["import", "com.avocent.viewer"]).arg(&tmp).status().map_err(|e| e.to_string())?;
        if st.success() { Ok(()) } else { Err("could not save the viewer preferences".into()) }
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(not(target_os = "macos"))]
fn set_viewer_cert_store(_store: &Path) -> Result<(), String> {
    Ok(()) // the viewer's preferences are only edited on macOS
}

/// Fetch one of the viewer's jars into `dir`, unless the copy there is current. The iDRAC sends an
/// ETag with each file and answers 304 (no body) when it is asked "only if changed", so a cached
/// copy costs one small request and a new firmware's files are picked up. Returns the file name and
/// whether it was downloaded just now.
fn download_jar(ip: &str, session: &WebSession, path: &str, dir: &Path) -> Result<(String, bool), String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let (file, tag_file) = (dir.join(&name), dir.join(format!("{name}.etag")));
    let mut headers = vec![("Cookie", session.cookie.clone()), ("ST2", session.st2.clone())];
    if file.is_file() {
        if let Ok(etag) = std::fs::read_to_string(&tag_file) {
            if !etag.trim().is_empty() {
                headers.push(("If-None-Match", etag.trim().to_string()));
            }
        }
    }
    let cached_try = headers.len() > 2;
    let r = request(ip, "GET", path, &headers, None)?;
    if r.status == 304 && cached_try {
        return Ok((name, false));
    }
    if r.status != 200 || r.body.is_empty() {
        return Err(format!("could not download {path} (HTTP {})", r.status));
    }
    std::fs::write(&file, &r.body).map_err(|e| e.to_string())?;
    match r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("etag")) {
        Some((_, etag)) => std::fs::write(&tag_file, etag).map_err(|e| e.to_string())?,
        None => {
            let _ = std::fs::remove_file(&tag_file); // no validator: it will be fetched again next time
        }
    }
    Ok((name, true))
}

fn open(app: &AppHandle, ip: &str, user: &str, pass: &str, remember: bool) -> Result<(), String> {
    stop();
    let Some(java) = find_java() else {
        if cfg!(target_os = "macos") {
            // ask the user before downloading anything
            let _ = app.emit("java-need-runtime", ());
            crate::status(app, "the Java console needs a Java runtime");
            return Ok(());
        }
        return Err("no Java runtime found: install Java 8 or later, or set BOBCAT_JAVA to the java binary".into());
    };
    crate::status(app, format!("logging in to {ip} (the iDRAC takes ~5s)..."));
    let console = html5::login_as(ip, user, pass, "1").map_err(|e| {
        html5::logout_all();
        e
    })?;
    let result = launch(app, ip, &console, &java);
    if result.is_err() {
        html5::logout_all();
    } else if remember {
        crate::keychain::remember(user, ip, pass);
    }
    result
}

fn launch(app: &AppHandle, ip: &str, console: &html5::Console, java: &Path) -> Result<(), String> {
    let auth = [("Cookie", console.session.cookie.clone()), ("ST2", console.session.st2.clone())];
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_millis();
    let r = request(ip, "GET", &format!("/viewer.jnlp({ip}@0@idrac@{ms}@ST1={})", console.st1), &auth, None)?;
    if r.status != 200 {
        return Err(format!("the iDRAC gave no Java launch file (HTTP {})", r.status));
    }
    let jnlp = r.text();
    let mut args = arguments(&jnlp);
    if args.is_empty() {
        return Err("the Java launch file has no arguments".into());
    }
    // virtual media is not supported yet: BOBCAT_JAVA_VM=1 keeps what the iDRAC asked for
    if std::env::var_os("BOBCAT_JAVA_VM").is_none() {
        for a in args.iter_mut().filter(|a| a.starts_with("vm=")) {
            *a = "vm=0".into();
        }
    }
    let main_class = jnlp.split("main-class=\"").nth(1).and_then(|s| s.split('"').next()).unwrap_or(MAIN_CLASS).to_string();
    let (main_jar, natives) = jar_paths(&jnlp)?;

    // the downloaded files are kept between sessions (checked against the iDRAC with their ETag)
    let dir = viewer_dir(ip);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::status(app, "checking the Java viewer files...");
    let (main_name, main_fresh) = download_jar(ip, &console.session, &main_jar, &dir)?;
    let mut unpacked = false;
    for n in &natives {
        let (name, fresh) = download_jar(ip, &console.session, n, &dir)?;
        let marker = dir.join(format!("{name}.unpacked"));
        if fresh || !marker.is_file() {
            let _ = std::fs::remove_file(&marker);
            let st = Command::new("unzip").args(["-o", "-q", &name]).current_dir(&dir).status().map_err(|e| format!("could not run unzip: {e}"))?;
            if !st.success() {
                return Err(format!("could not unpack {name}"));
            }
            std::fs::write(&marker, b"").map_err(|e| e.to_string())?;
            unpacked = true;
        }
    }
    if cfg!(target_os = "macos") && unpacked {
        patch_openssl_paths(&dir)?;
    }
    if html5::debug_on() {
        eprintln!("[bobcat] Java viewer files: main jar {}, native libraries {}", if main_fresh { "downloaded" } else { "from the cache" }, if unpacked { "unpacked" } else { "from the cache" });
    }

    if let Err(e) = set_viewer_cert_store(&cert_store_dir()).and_then(|_| trust_in_viewer(ip, &cert_store_dir())) {
        if html5::debug_on() {
            eprintln!("[bobcat] could not set up the viewer's certificate store: {e}");
        }
    }

    let log = std::fs::File::create(dir.join("viewer.log")).map_err(|e| e.to_string())?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(java)
        .current_dir(&dir)
        .arg(format!("-Djava.library.path={}", dir.display()))
        // the iDRAC closes the console connection during the handshake of a current Java client
        // unless it is held to TLS 1.2 without the extended master secret
        .args(["-Djdk.tls.client.protocols=TLSv1.2", "-Djdk.tls.useExtendedMasterSecret=false"])
        .arg("-cp")
        .arg(&main_name)
        .arg(&main_class)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", java.display()))?;
    *VIEWER_PID.lock().unwrap() = Some(child.id());
    html5::keep_alive(console.session.clone());
    if html5::debug_on() {
        eprintln!("[bobcat] Java viewer started (log: {})", dir.join("viewer.log").display());
    }
    #[cfg(target_os = "macos")]
    {
        crate::status(app, format!("Java console of {ip} opened (docking its window under the toolbar)"));
        crate::java_dock::start(app.clone(), child.id(), ip.to_string());
    }
    #[cfg(not(target_os = "macos"))]
    crate::status(app, format!("Java console of {ip} opened in its own window"));

    let app = app.clone();
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
        let mut current = VIEWER_PID.lock().unwrap();
        if *current == Some(pid) {
            *current = None;
            drop(current);
            html5::logout_all();
            crate::status(&app, "Java console closed");
        }
    });
    Ok(())
}

/// The process id of the running viewer.
pub(crate) fn current_pid() -> Option<u32> {
    *VIEWER_PID.lock().unwrap()
}

/// Is the viewer with this process id still the running one?
pub(crate) fn viewer_running(pid: u32) -> bool {
    *VIEWER_PID.lock().unwrap() == Some(pid)
}

/// Stop the viewer if one is running: ask it to end (SIGTERM), then force it, and log out.
/// Blocks for up to about 6 seconds.
pub fn stop() {
    let Some(pid) = *VIEWER_PID.lock().unwrap() else { return };
    let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
    for i in 0..60 {
        if *VIEWER_PID.lock().unwrap() != Some(pid) {
            return;
        }
        if i == 50 {
            let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tauri::command]
pub fn connect_java(app: AppHandle, ip: String, user: String, pass: String, remember: bool) {
    std::thread::spawn(move || {
        if let Err(e) = open(&app, &ip, &user, &pass, remember) {
            crate::status(&app, format!("error: {e}"));
        }
    });
}

#[tauri::command]
pub fn disconnect_java() {
    std::thread::spawn(stop);
}

/// Type the clipboard into the Java viewer (macOS: key presses through the Accessibility API).
#[tauri::command]
pub async fn java_paste() -> Result<usize, String> {
    #[cfg(target_os = "macos")]
    {
        let text = crate::with_clip(|c| c.get_text())?;
        let n = text.chars().count();
        if n > 10_000 {
            return Err(format!("clipboard too large ({n} characters, max 10000)"));
        }
        crate::java_dock::paste(&text)
    }
    #[cfg(not(target_os = "macos"))]
    Err("pasting into the Java viewer is only implemented on macOS".into())
}

/// Read the Java viewer's screen with OCR and put the text on the clipboard (macOS).
#[tauri::command]
pub async fn java_copy_text() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        crate::java_dock::copy_text()
    }
    #[cfg(not(target_os = "macos"))]
    Err("copying text from the Java viewer is only implemented on macOS".into())
}
