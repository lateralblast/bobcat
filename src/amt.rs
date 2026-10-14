// bobcat-cli: Intel AMT (vPro) management over WS-Management.
//
// AMT answers SOAP requests on TCP 16992 (plain HTTP) with HTTP digest authentication (user `admin`
// and the AMT password). This module reads the device's state, changes its power state and turns
// the built-in KVM (an RFB/VNC server on TCP 5900, protected by an 8-character RFB password) on
// and off. TLS (16993) is not supported yet. License: CC BY-NC-SA 4.0 (LICENSE.md).

use openssl::hash::{MessageDigest, hash};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const AMT: &str = "http://intel.com/wbem/wscim/1/amt-schema/1";
const IPS: &str = "http://intel.com/wbem/wscim/1/ips-schema/1";
const CIM: &str = "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2";

pub struct Amt {
    ip: String,
    port: u16,
    user: String,
    pass: String,
    /// The firmware version from the `Server:` header of the last answer
    pub server: std::cell::RefCell<String>,
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn md5_hex(s: &str) -> String {
    hex(&hash(MessageDigest::md5(), s.as_bytes()).expect("md5"))
}

/// The value of `key` in a `Digest realm="x", nonce="y", qop=auth` header.
fn digest_param(header: &str, key: &str) -> Option<String> {
    let rest = header.trim().strip_prefix("Digest")?.trim_start();
    for part in split_params(rest) {
        let (k, v) = part.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(key) {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Split on commas that are not inside quotes.
fn split_params(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Has the whole HTTP answer arrived? (headers, then Content-Length bytes or the chunked terminator)
fn complete(buf: &[u8]) -> bool {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let body = &buf[end + 4..];
    if let Some(n) = head.lines().find_map(|l| l.strip_prefix("content-length:")) {
        return body.len() >= n.trim().parse::<usize>().unwrap_or(0);
    }
    if head.contains("transfer-encoding: chunked") {
        return body.ends_with(b"0\r\n\r\n");
    }
    // a status that carries no body; otherwise the server closes the connection (Connection: close)
    head.starts_with("http/1.1 204") || head.starts_with("http/1.1 303") || head.starts_with("http/1.1 304")
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let (mut out, mut rest) = (Vec::new(), body);
    loop {
        let Some(nl) = rest.windows(2).position(|w| w == b"\r\n") else { break };
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..nl]).split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        rest = &rest[nl + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
    out
}

/// The text of the first `<x:name>text</x:name>` in an XML document, whatever the prefix (the
/// element may carry attributes, like `<a:Text xml:lang="en-US">`).
pub fn field(xml: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = xml[from..].find(&format!(":{name}")) {
        let after = from + i + name.len() + 1;
        match xml[after..].chars().next() {
            Some('>') => {
                let end = xml[after + 1..].find("</")?;
                return Some(xml[after + 1..after + 1 + end].trim().to_string());
            }
            Some(c) if c.is_whitespace() => {
                let gt = xml[after..].find('>')? + after;
                if xml[..gt].ends_with('/') {
                    from = gt;
                    continue;
                }
                let end = xml[gt + 1..].find("</")?;
                return Some(xml[gt + 1..gt + 1 + end].trim().to_string());
            }
            _ => from = after,
        }
    }
    None
}

/// Every `<x:name>text</x:name>` value in a document (opening tags only, not the closing ones).
fn fields(xml: &str, name: &str) -> Vec<String> {
    let needle = format!(":{name}>");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = xml[from..].find(&needle) {
        let at = from + i;
        let start = at + needle.len();
        let closing = xml[..at].rfind('<').is_some_and(|lt| xml[lt + 1..].starts_with('/'));
        let Some(end) = xml[start..].find("</") else { break };
        if !closing {
            out.push(xml[start..start + end].trim().to_string());
        }
        from = start;
    }
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl Amt {
    pub fn new(ip: &str, port: u16, user: &str, pass: &str) -> Amt {
        Amt { ip: ip.into(), port, user: user.into(), pass: pass.into(), server: Default::default() }
    }

    fn request(&self, body: &str, auth: Option<&str>) -> Result<Resp, String> {
        let addr = (self.ip.as_str(), self.port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad address")?;
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(8)).map_err(|e| format!("cannot connect to {}:{} ({e})", self.ip, self.port))?;
        s.set_read_timeout(Some(Duration::from_secs(30))).ok();
        s.set_write_timeout(Some(Duration::from_secs(10))).ok();
        let mut req = format!(
            "POST /wsman HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/soap+xml;charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.ip,
            self.port,
            body.len()
        );
        if let Some(a) = auth {
            req.push_str(&format!("Authorization: {a}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        while !complete(&buf) {
            match s.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(format!("reading from the AMT device: {e}")),
            }
        }
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no HTTP answer from the AMT device")?;
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let mut lines = head.lines();
        let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).ok_or("bad HTTP status line")?;
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        let raw = &buf[end + 4..];
        let chunked = headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked"));
        let body = if chunked { dechunk(raw) } else { raw.to_vec() };
        let resp = Resp { status, headers, body: String::from_utf8_lossy(&body).to_string() };
        if let Some(srv) = resp.header("Server") {
            *self.server.borrow_mut() = srv.to_string();
        }
        Ok(resp)
    }

    /// POST a SOAP envelope with digest authentication; returns the answer's XML.
    fn call(&self, soap: &str) -> Result<String, String> {
        let r = self.call_inner(soap);
        if std::env::var_os("BOBCAT_DEBUG").is_some() {
            let what = format!("{} {}", field(soap, "Action").unwrap_or_default().rsplit('/').next().unwrap_or(""), field(soap, "ResourceURI").unwrap_or_default().rsplit('/').next().unwrap_or(""));
            match &r {
                Ok(_) => eprintln!("[amt] {what}: ok"),
                Err(e) => eprintln!("[amt] {what}: {e}"),
            }
        }
        r
    }

    fn call_inner(&self, soap: &str) -> Result<String, String> {
        let first = self.request(soap, None)?;
        let resp = if first.status == 401 {
            let ch = first.header("WWW-Authenticate").ok_or("401 without a digest challenge")?;
            let realm = digest_param(ch, "realm").ok_or("no realm in the digest challenge")?;
            let nonce = digest_param(ch, "nonce").ok_or("no nonce in the digest challenge")?;
            let qop = digest_param(ch, "qop").map(|q| q.split(',').next().unwrap_or("auth").trim().to_string());
            let opaque = digest_param(ch, "opaque");
            let mut rnd = [0u8; 8];
            openssl::rand::rand_bytes(&mut rnd).map_err(|e| e.to_string())?;
            let cnonce = hex(&rnd);
            let nc = "00000001";
            let ha1 = md5_hex(&format!("{}:{realm}:{}", self.user, self.pass));
            let ha2 = md5_hex("POST:/wsman");
            let (response, qop_part) = match &qop {
                Some(q) => (md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:{q}:{ha2}")), format!(", qop={q}, nc={nc}, cnonce=\"{cnonce}\"")),
                None => (md5_hex(&format!("{ha1}:{nonce}:{ha2}")), String::new()),
            };
            let mut auth = format!("Digest username=\"{}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"/wsman\", algorithm=MD5, response=\"{response}\"{qop_part}", self.user);
            if let Some(o) = opaque {
                auth.push_str(&format!(", opaque=\"{o}\""));
            }
            self.request(soap, Some(&auth))?
        } else {
            first
        };
        match resp.status {
            200 => Ok(resp.body),
            401 => Err("login failed: wrong user name or password".into()),
            n => {
                let why = field(&resp.body, "Text").unwrap_or_default();
                let code = fields(&resp.body, "Value").into_iter().last().unwrap_or_default();
                let detail = [code, why].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(": ");
                Err(format!("the AMT device answered HTTP {n}{}", if detail.is_empty() { String::new() } else { format!(" ({detail})") }))
            }
        }
    }

    fn envelope(action: &str, resource: &str, selectors: &[(&str, &str)], body: &str) -> String {
        let sel = if selectors.is_empty() {
            String::new()
        } else {
            let items: String = selectors.iter().map(|(k, v)| format!("<w:Selector Name=\"{k}\">{}</w:Selector>", xml_escape(v))).collect();
            format!("<w:SelectorSet>{items}</w:SelectorSet>")
        };
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Envelope xmlns=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:a=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" xmlns:w=\"http://schemas.dmtf.org/wbem/wsman/1/wsman.xsd\"><Header><a:Action>{action}</a:Action><a:To>/wsman</a:To><w:ResourceURI>{resource}</w:ResourceURI><a:MessageID>1</a:MessageID><a:ReplyTo><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address></a:ReplyTo><w:OperationTimeout>PT30S</w:OperationTimeout>{sel}</Header><Body>{body}</Body></Envelope>"
        )
    }

    fn get(&self, resource: &str) -> Result<String, String> {
        self.call(&Self::envelope("http://schemas.xmlsoap.org/ws/2004/09/transfer/Get", resource, &[], ""))
    }

    fn put(&self, resource: &str, instance: &str) -> Result<String, String> {
        self.call(&Self::envelope("http://schemas.xmlsoap.org/ws/2004/09/transfer/Put", resource, &[], instance))
    }

    /// Call a method of a CIM class; the answer must carry ReturnValue 0.
    fn invoke(&self, resource: &str, method: &str, selectors: &[(&str, &str)], input: &str) -> Result<String, String> {
        let action = format!("{resource}/{method}");
        let xml = self.call(&Self::envelope(&action, resource, selectors, input))?;
        match field(&xml, "ReturnValue").as_deref() {
            Some("0") => Ok(xml),
            Some(n) => Err(format!("{method} was refused (ReturnValue {n})")),
            None => Ok(xml),
        }
    }

    // ---- state ---------------------------------------------------------------------------

    /// The device's power state as AMT numbers it.
    pub fn power_state(&self) -> Result<u32, String> {
        let xml = self.get(&format!("{CIM}/CIM_AssociatedPowerManagementService"))?;
        field(&xml, "PowerState").and_then(|v| v.parse().ok()).ok_or_else(|| "no power state in the answer".to_string())
    }

    fn available_power_states(&self) -> Result<Vec<u32>, String> {
        let xml = self.get(&format!("{CIM}/CIM_AssociatedPowerManagementService"))?;
        Ok(fields(&xml, "AvailableRequestedPowerStates").iter().filter_map(|v| v.parse().ok()).collect())
    }

    /// True when the device's host name or OS FQDN is this machine's: a viewer on this machine would
    /// then show its own window inside the captured screen (a hall of mirrors).
    pub fn is_this_machine(&self) -> bool {
        let Ok(gen_) = self.get(&format!("{AMT}/AMT_GeneralSettings")) else { return false };
        let local = std::fs::read_to_string("/etc/hostname")
            .ok()
            .or_else(|| std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()))
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let short = |h: &str| h.split('.').next().unwrap_or("").to_ascii_lowercase();
        !local.is_empty() && [field(&gen_, "HostName"), field(&gen_, "HostOSFQDN")].iter().flatten().any(|h| short(h) == short(&local))
    }

    /// Print what the device reports.
    pub fn info(&self) -> Result<(), String> {
        let gen_ = self.get(&format!("{AMT}/AMT_GeneralSettings"))?;
        let setup = self.get(&format!("{AMT}/AMT_SetupAndConfigurationService"))?;
        let kvm = self.get(&format!("{IPS}/IPS_KVMRedirectionSettingData"))?;
        let sap = self.get(&format!("{CIM}/CIM_KVMRedirectionSAP"))?;
        let redir = self.get(&format!("{AMT}/AMT_RedirectionService"))?;
        let optin = self.get(&format!("{IPS}/IPS_OptInService"))?;
        let power = self.power_state()?;
        let get = |x: &str, n: &str| field(x, n).unwrap_or_else(|| "?".into());
        println!("Intel AMT at {}:{}", self.ip, self.port);
        println!("  firmware:     {}", self.server.borrow().trim_start_matches("Intel(R) Active Management Technology "));
        println!("  host name:    {} ({}) domain {}", get(&gen_, "HostName"), get(&gen_, "HostOSFQDN"), get(&gen_, "DomainName"));
        println!("  provisioning: {}", match get(&setup, "ProvisioningState").as_str() { "0" => "not provisioned", "1" => "in provisioning", "2" => "provisioned", _ => "unknown" });
        println!("  power:        {}", power_name(power));
        println!("  redirection:  listener {}, SOL/IDER {}", if get(&redir, "ListenerEnabled") == "true" { "on" } else { "off" }, match get(&redir, "EnabledState").as_str() {
            "32768" => "both off",
            "32769" => "IDER on",
            "32770" => "SOL on",
            "32771" => "both on",
            _ => "unknown",
        });
        println!("  KVM:          service {}, VNC port 5900 {}, consent required: {}", match get(&sap, "EnabledState").as_str() {
            "2" => "enabled",
            "3" => "disabled",
            "6" => "enabled, offline",
            _ => "unknown",
        }, if get(&kvm, "Is5900PortEnabled") == "true" { "on" } else { "off" }, if get(&optin, "OptInRequired") == "0" { "no" } else { "yes" });
        Ok(())
    }

    // ---- power ---------------------------------------------------------------------------

    /// Ask for a power state change (AMT's CIM_PowerManagementService numbers).
    pub fn set_power(&self, state: u32) -> Result<(), String> {
        let available = self.available_power_states()?;
        if !available.contains(&state) {
            return Err(format!("that power state is not available now (the device offers {:?}, currently {})", available, power_name(self.power_state()?)));
        }
        let svc = format!("{CIM}/CIM_PowerManagementService");
        let input = format!(
            "<p:RequestPowerStateChange_INPUT xmlns:p=\"{svc}\"><p:PowerState>{state}</p:PowerState><p:ManagedElement><a:Address>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:Address><a:ReferenceParameters><w:ResourceURI>{CIM}/CIM_ComputerSystem</w:ResourceURI><w:SelectorSet><w:Selector Name=\"CreationClassName\">CIM_ComputerSystem</w:Selector><w:Selector Name=\"Name\">ManagedSystem</w:Selector></w:SelectorSet></a:ReferenceParameters></p:ManagedElement></p:RequestPowerStateChange_INPUT>"
        );
        let selectors = [("Name", "Intel(r) AMT Power Management Service"), ("CreationClassName", "CIM_PowerManagementService"), ("SystemCreationClassName", "CIM_ComputerSystem"), ("SystemName", "Intel(r) AMT")];
        self.invoke(&svc, "RequestPowerStateChange", &selectors, &input)?;
        Ok(())
    }

    // ---- KVM (RFB on port 5900) ------------------------------------------------------------

    /// The KVM settings instance rebuilt as a Put body, with some fields changed.
    fn kvm_put_body(&self, changes: &[(&str, &str)], keep: &[&str]) -> Result<String, String> {
        let xml = self.get(&format!("{IPS}/IPS_KVMRedirectionSettingData"))?;
        let start = xml.find("IPS_KVMRedirectionSettingData>").ok_or("no KVM settings in the answer")?;
        let inner_start = start + "IPS_KVMRedirectionSettingData>".len();
        // the instance ends at its own closing tag, </x:IPS_KVMRedirectionSettingData>
        let close_name = xml[inner_start..].find("IPS_KVMRedirectionSettingData>").ok_or("bad KVM settings")? + inner_start;
        let end = xml[..close_name].rfind("</").ok_or("bad KVM settings")?;
        let inner = &xml[inner_start..end];
        // children: <x:Name>value</x:Name>
        let mut items: Vec<(String, String)> = Vec::new();
        let mut rest = inner;
        while let Some(lt) = rest.find('<') {
            let after = &rest[lt + 1..];
            let Some(gt) = after.find('>') else { break };
            let tag = &after[..gt];
            if tag.starts_with('/') {
                rest = &after[gt + 1..];
                continue;
            }
            let name = tag.split(|c: char| c.is_whitespace()).next().unwrap_or("").rsplit(':').next().unwrap_or("").to_string();
            let value_start = gt + 1;
            let value_end = after[value_start..].find('<').map(|i| i + value_start).unwrap_or(after.len());
            let value = after[value_start..value_end].to_string();
            items.push((name, value));
            // skip past the closing tag
            let close = after[value_end..].find('>').map(|i| i + value_end + 1).unwrap_or(after.len());
            rest = &after[close..];
        }
        for (k, v) in changes {
            match items.iter_mut().find(|(n, _)| n == k) {
                Some(item) => item.1 = (*v).to_string(),
                None => items.push(((*k).to_string(), (*v).to_string())),
            }
        }
        // only the properties that may be written: the device refuses the read-only capability flags
        items.retain(|(n, _)| keep.contains(&n.as_str()));
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let ns = format!("{IPS}/IPS_KVMRedirectionSettingData");
        let children: String = items.iter().map(|(n, v)| format!("<g:{n}>{v}</g:{n}>")).collect();
        Ok(format!("<g:IPS_KVMRedirectionSettingData xmlns:g=\"{ns}\">{children}</g:IPS_KVMRedirectionSettingData>"))
    }

    fn kvm_state(&self, requested: u32) -> Result<(), String> {
        let sap = format!("{CIM}/CIM_KVMRedirectionSAP");
        let input = format!("<r:RequestStateChange_INPUT xmlns:r=\"{sap}\"><r:RequestedState>{requested}</r:RequestedState></r:RequestStateChange_INPUT>");
        let selectors = [("Name", "KVM Redirection Service Access Point"), ("CreationClassName", "CIM_KVMRedirectionSAP"), ("SystemCreationClassName", "CIM_ComputerSystem"), ("SystemName", "ManagedSystem")];
        self.invoke(&sap, "RequestStateChange", &selectors, &input)?;
        Ok(())
    }

    /// Put the KVM settings. AMT 14.1 validates the Put against a schema that needs the whole
    /// instance (a trimmed set answers SchemaValidationError), so every property goes back as read.
    fn kvm_put(&self, changes: &[(&str, &str)]) -> Result<(), String> {
        const ALL: &[&str] = &[
            "BackToBackFbMode", "DefaultScreen", "DoubleBufferActive", "DoubleBufferEnabled", "ElementName", "EnabledByMEBx",
            "GrayscalePixelFormatSupported", "InitialDecimationModeForLowRes", "InstanceID", "Is5900PortEnabled", "OptInPolicy",
            "RFBPassword", "SessionTimeout", "ZlibControlSupported",
        ];
        let body = self.kvm_put_body(changes, ALL)?;
        self.put(&format!("{IPS}/IPS_KVMRedirectionSettingData"), &body)?;
        Ok(())
    }

    /// Diagnostic: try changes to the KVM settings and print what the device says to each. The
    /// accepted ones really change the settings; OptInPolicy and SessionTimeout are put back to
    /// their original values at the end.
    pub fn kvm_diagnose(&self, rfb_password: &str) -> Result<(), String> {
        let pw = xml_escape(rfb_password);
        let before = self.get(&format!("{IPS}/IPS_KVMRedirectionSettingData"))?;
        let opt = field(&before, "OptInPolicy").unwrap_or_else(|| "false".into());
        let timeout = field(&before, "SessionTimeout").unwrap_or_else(|| "3".into());
        let other_timeout = if timeout == "4" { "5" } else { "4" }.to_string();
        let flip_opt = if opt == "true" { "false" } else { "true" }.to_string();
        println!("current: Is5900PortEnabled={} OptInPolicy={opt} SessionTimeout={timeout} DefaultScreen={}", field(&before, "Is5900PortEnabled").unwrap_or_default(), field(&before, "DefaultScreen").unwrap_or_default());
        let tests: Vec<(String, Vec<(&str, String)>)> = vec![
            ("unchanged".into(), vec![]),
            (format!("SessionTimeout {timeout} -> {other_timeout}"), vec![("SessionTimeout", other_timeout.clone())]),
            (format!("OptInPolicy {opt} -> {flip_opt}"), vec![("OptInPolicy", flip_opt.clone())]),
            (format!("OptInPolicy {flip_opt} + RFBPassword"), vec![("OptInPolicy", flip_opt.clone()), ("RFBPassword", pw.clone())]),
            (format!("OptInPolicy {flip_opt} + Is5900PortEnabled true"), vec![("OptInPolicy", flip_opt.clone()), ("Is5900PortEnabled", "true".into())]),
            (format!("OptInPolicy {flip_opt} + RFBPassword + Is5900PortEnabled true"), vec![("OptInPolicy", flip_opt.clone()), ("RFBPassword", pw.clone()), ("Is5900PortEnabled", "true".into())]),
            ("RFBPassword (OptInPolicy as it was)".into(), vec![("RFBPassword", pw.clone())]),
            ("Is5900PortEnabled true (OptInPolicy as it was)".into(), vec![("Is5900PortEnabled", "true".into())]),
        ];
        let all: Vec<&str> = vec![
            "BackToBackFbMode", "DefaultScreen", "DoubleBufferActive", "DoubleBufferEnabled", "ElementName", "EnabledByMEBx",
            "GrayscalePixelFormatSupported", "InitialDecimationModeForLowRes", "InstanceID", "Is5900PortEnabled", "OptInPolicy",
            "RFBPassword", "SessionTimeout", "ZlibControlSupported",
        ];
        let run = |changes: &[(&str, String)]| -> Result<(), String> {
            let ch: Vec<(&str, &str)> = changes.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let body = self.kvm_put_body(&ch, &all)?;
            self.put(&format!("{IPS}/IPS_KVMRedirectionSettingData"), &body).map(|_| ())
        };
        for (name, changes) in &tests {
            println!("{:<66} {}", name, match run(changes) { Ok(()) => "ACCEPTED".to_string(), Err(e) => e });
        }
        // put the settings this test may have changed back
        let restore = [("OptInPolicy", opt.clone()), ("SessionTimeout", timeout.clone())];
        println!("{:<66} {}", "restoring OptInPolicy and SessionTimeout", match run(&restore) { Ok(()) => "ok".to_string(), Err(e) => e });
        Ok(())
    }

    /// Turn the KVM on: the KVM service first, then the RFB password, then port 5900 (separate Puts,
    /// so a refusal says which of the two the firmware will not take).
    pub fn kvm_enable(&self, rfb_password: &str) -> Result<(), String> {
        self.kvm_state(2).map_err(|e| format!("enabling the KVM service: {e}"))?;
        self.kvm_put(&[("RFBPassword", &xml_escape(rfb_password))])
            .map_err(|e| format!("the KVM service is enabled but setting the RFB password failed: {e}"))?;
        self.kvm_put(&[("Is5900PortEnabled", "true")])
            .map_err(|e| format!("the KVM service is enabled and the RFB password is set, but turning on port 5900 failed: {e}"))?;
        Ok(())
    }

    /// Turn port 5900 and the KVM service off.
    pub fn kvm_disable(&self) -> Result<(), String> {
        self.kvm_put(&[("Is5900PortEnabled", "false")]).map_err(|e| format!("saving the KVM settings: {e}"))?;
        self.kvm_state(3).map_err(|e| format!("port 5900 was turned off but disabling the KVM service failed: {e}"))?;
        Ok(())
    }
}

pub fn power_name(state: u32) -> String {
    match state {
        2 => "on".into(),
        3 | 4 => "sleep".into(),
        6 => "off (hard)".into(),
        7 => "hibernate".into(),
        8 => "off (soft)".into(),
        n => format!("state {n}"),
    }
}

/// AMT's RFB password: exactly 8 characters with an upper-case letter, a lower-case letter, a digit
/// and a special character. The error says which of those is missing.
pub fn check_rfb_password(p: &str) -> Result<(), String> {
    let mut missing = Vec::new();
    if p.chars().any(|c| !c.is_ascii()) {
        missing.push("only ASCII characters".to_string());
    }
    if !p.chars().any(|c| c.is_ascii_uppercase()) {
        missing.push("an upper-case letter".into());
    }
    if !p.chars().any(|c| c.is_ascii_lowercase()) {
        missing.push("a lower-case letter".into());
    }
    if !p.chars().any(|c| c.is_ascii_digit()) {
        missing.push("a digit".into());
    }
    if !p.chars().any(|c| c.is_ascii() && !c.is_ascii_alphanumeric()) {
        missing.push("a special character (such as ! or @)".into());
    }
    let len = p.chars().count();
    match (len == 8, missing.is_empty()) {
        (true, true) => Ok(()),
        (true, false) => Err(format!("the AMT VNC (RFB) password needs {}", missing.join(", "))),
        (false, true) => Err(format!("the AMT VNC (RFB) password must be exactly 8 characters (you typed {len})")),
        (false, false) => Err(format!("the AMT VNC (RFB) password must be exactly 8 characters (you typed {len}) and needs {}", missing.join(", "))),
    }
}
