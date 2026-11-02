//! Syncthing's REST API on the handheld itself: plain HTTP/1.0 to localhost, so
//! every reply ends when the connection closes (no chunked bodies to decode).
//! Every call blocks for at most a few seconds; callers run them off the UI thread.

use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::model::{DeviceId, FolderId, Password, PendingDevice, PendingFolder, Snapshot, Summary};
use super::wire;

const CONNECT: Duration = Duration::from_secs(1);
/// Scans and config commits answer within a second on the handheld; restarts
/// answer before they happen.
const REPLY: Duration = Duration::from_secs(8);
/// How long a restart request is retried while the web server restarts.
const RESTART_RETRIES: Duration = Duration::from_secs(5);
/// The largest reply: the whole configuration of a busy install is well under this.
const LIMIT: u64 = 4 * 1024 * 1024;

/// The key Syncthing generated for its REST API. `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_graphic())).then(|| Self(text.to_owned()))
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("ApiKey(..)") }
}

/// What config.xml says about the web interface.
#[derive(Debug, PartialEq, Eq)]
pub struct GuiConfig {
    pub api_key: Option<ApiKey>,
    /// Both a user and a password hash are set.
    pub has_login: bool,
}

impl GuiConfig {
    /// Reads the `<gui>` element. Syncthing writes this file itself, one element
    /// per line and never with attributes on these three, so a full XML parser
    /// would buy nothing.
    pub fn parse(xml: &str) -> Self {
        let gui = xml.split_once("<gui").and_then(|(_, rest)| rest.split_once("</gui>")).map_or("", |(gui, _)| gui);
        let text = |name: &str| {
            let open = format!("<{name}>");
            gui.split_once(&open).and_then(|(_, rest)| rest.split_once(&format!("</{name}>"))).map(|(value, _)| value.trim()).filter(|value| !value.is_empty())
        };
        Self { api_key: text("apikey").and_then(ApiKey::parse), has_login: text("user").is_some() && text("password").is_some() }
    }

    pub fn read(path: &Path) -> std::io::Result<Self> { std::fs::read_to_string(path).map(|xml| Self::parse(&xml)) }
}

/// Why a call did not work. `message` is what the menu shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Nothing listens on the port: Syncthing is stopped, starting or restarting.
    NotRunning,
    TimedOut,
    /// config.xml is missing or has no API key: Syncthing has not run on this card yet.
    NoApiKey,
    /// Syncthing refused the API key.
    Refused,
    /// Syncthing answered with an error.
    Status { code: u16, message: String },
    /// A reply this version of the menu does not understand.
    Malformed(String),
    /// Anything else: the socket, or a file the menu keeps.
    Io(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Self::NotRunning => "Syncthing is not running".into(),
            Self::TimedOut => "Syncthing did not answer in time".into(),
            Self::NoApiKey => "Syncthing has not started on this card yet".into(),
            Self::Refused => "Syncthing refused the menu's API key".into(),
            Self::Status { code, message } if message.is_empty() => format!("Syncthing answered {code}"),
            Self::Status { message, .. } => format!("Syncthing: {message}"),
            Self::Malformed(reason) => format!("Unexpected reply from Syncthing: {reason}"),
            Self::Io(reason) => reason.clone(),
        }
    }

    fn connecting(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::ConnectionRefused => Self::NotRunning,
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Self::TimedOut,
            _ => Self::Io(format!("connecting to Syncthing: {error}")),
        }
    }

    fn talking(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Self::TimedOut,
            // A restart or shutdown closes the connection under us.
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe => Self::NotRunning,
            _ => Self::Io(format!("talking to Syncthing: {error}")),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message()) }
}

#[derive(Clone, Copy, Debug)]
enum Method { Get, Post, Put, Patch }

impl Method {
    fn word(self) -> &'static str {
        match self { Self::Get => "GET", Self::Post => "POST", Self::Put => "PUT", Self::Patch => "PATCH" }
    }
}

/// Percent-encodes a query value (folder IDs are chosen by people).
fn query(value: &str) -> String {
    value.bytes().map(|byte| match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(byte).to_string(),
        byte => format!("%{byte:02X}"),
    }).collect()
}

/// Splits a raw HTTP/1.x response into its status code and body.
fn response(bytes: &[u8]) -> Result<(u16, &[u8]), Failure> {
    let split = bytes.windows(4).position(|window| window == b"\r\n\r\n").ok_or_else(|| Failure::Malformed("no end of headers".into()))?;
    let (head, body) = (&bytes[..split], &bytes[split + 4..]);
    let status_line = head.split(|byte| *byte == b'\n').next().map(String::from_utf8_lossy).unwrap_or_default();
    let mut words = status_line.split_whitespace();
    match (words.next(), words.next().and_then(|code| code.parse::<u16>().ok())) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => Ok((code, body)),
        _ => Err(Failure::Malformed(format!("status line {:?}", status_line.trim()))),
    }
}

pub struct Client {
    address: SocketAddr,
    key: ApiKey,
}

impl Client {
    pub fn new(address: SocketAddr, key: ApiKey) -> Self { Self { address, key } }

    /// The client for the Syncthing whose config.xml is at `config`.
    pub fn from_config(address: SocketAddr, config: &Path) -> Result<Self, Failure> {
        let gui = GuiConfig::read(config).map_err(|_| Failure::NoApiKey)?;
        gui.api_key.map(|key| Self::new(address, key)).ok_or(Failure::NoApiKey)
    }

    fn exchange(&self, method: Method, path: &str, body: &[u8]) -> Result<Vec<u8>, Failure> {
        let mut stream = TcpStream::connect_timeout(&self.address, CONNECT).map_err(|error| Failure::connecting(&error))?;
        let talking = |error: std::io::Error| Failure::talking(&error);
        stream.set_read_timeout(Some(REPLY)).map_err(talking)?;
        stream.set_write_timeout(Some(REPLY)).map_err(talking)?;
        let head = format!(
            "{} {path} HTTP/1.0\r\nHost: {}\r\nX-API-Key: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            method.word(), self.address, self.key.0, body.len(),
        );
        stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body)).map_err(talking)?;
        let mut reply = Vec::new();
        stream.take(LIMIT + 1).read_to_end(&mut reply).map_err(talking)?;
        if reply.len() as u64 > LIMIT { return Err(Failure::Malformed(format!("{path}: reply larger than {LIMIT} bytes"))); }
        let (code, body) = response(&reply)?;
        match code {
            200..=299 => Ok(body.to_vec()),
            401 | 403 => Err(Failure::Refused),
            code => Err(Failure::Status { code, message: String::from_utf8_lossy(body).lines().next().unwrap_or_default().chars().take(120).collect() }),
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Failure> {
        let body = self.exchange(Method::Get, path, &[])?;
        serde_json::from_slice(&body).map_err(|error| Failure::Malformed(format!("{}: {error}", path.split('?').next().unwrap_or(path))))
    }

    fn send(&self, method: Method, path: &str, body: &impl Serialize) -> Result<(), Failure> {
        let body = serde_json::to_vec(body).map_err(|error| Failure::Malformed(error.to_string()))?;
        self.exchange(method, path, &body).map(drop)
    }

    fn post(&self, path: &str) -> Result<(), Failure> { self.exchange(Method::Post, path, &[]).map(drop) }

    fn folders(&self) -> Result<Vec<(wire::FolderConfig, wire::FolderStatus)>, Failure> {
        let configs: Vec<wire::FolderConfig> = self.get("/rest/config/folders")?;
        configs.into_iter().map(|config| {
            let status = self.get(&format!("/rest/db/status?folder={}", query(&config.id)))?;
            Ok((config, status))
        }).collect()
    }

    /// Everything the screen shows. `destination` places pending folders on the card.
    pub fn snapshot(&self, destination: impl Fn(&str, &[super::model::Folder]) -> super::model::Destination) -> Result<Snapshot, Failure> {
        let replies = wire::Replies {
            status: self.get("/rest/system/status")?,
            folders: self.folders()?,
            devices: self.get("/rest/config/devices")?,
            connections: self.get("/rest/system/connections")?,
            statistics: self.get("/rest/stats/device")?,
            pending_devices: self.get("/rest/cluster/pending/devices")?,
            pending_folders: self.get("/rest/cluster/pending/folders")?,
        };
        wire::snapshot(replies, destination).map_err(Failure::Malformed)
    }

    /// The status bar's word: folder states and connections only.
    pub fn summary(&self) -> Result<Summary, Failure> {
        let folders = self.folders()?;
        let connections: wire::Connections = self.get("/rest/system/connections")?;
        let states: Vec<_> = folders.iter().map(|(config, status)| wire::folder_state(config, status)).collect();
        Ok(Summary::of(states.iter(), connections.connections.values().filter(|connection| connection.connected).count()))
    }

    pub fn rescan(&self, folder: &FolderId) -> Result<(), Failure> {
        self.post(&format!("/rest/db/scan?folder={}", query(folder.as_str())))
    }

    pub fn accept_device(&self, device: &PendingDevice) -> Result<(), Failure> {
        self.send(Method::Post, "/rest/config/devices", &wire::NewDevice { device_id: device.id.as_str(), name: &device.name })
    }

    /// Ignored devices are kept in the configuration's top level, which has no
    /// endpoint of its own: the whole configuration goes back, as Syncthing's
    /// own web interface does it.
    pub fn ignore_device(&self, device: &PendingDevice, now: &str) -> Result<(), Failure> {
        let mut config: wire::WholeConfig = self.get("/rest/config")?;
        let entry = wire::IgnoredDevice { time: now, device_id: device.id.as_str(), name: &device.name, address: &device.address };
        config.remote_ignored_devices.push(serde_json::to_value(entry).map_err(|error| Failure::Malformed(error.to_string()))?);
        self.send(Method::Put, "/rest/config", &config)
    }

    pub fn accept_folder(&self, folder: &PendingFolder, path: &Path) -> Result<(), Failure> {
        let path = path.to_str().ok_or_else(|| Failure::Io(format!("{} is not UTF-8", path.display())))?;
        let devices = folder.offered_by.iter().map(|device| wire::FolderDevice { device_id: device.as_str().to_owned() }).collect();
        self.send(Method::Post, "/rest/config/folders", &wire::NewFolder { id: folder.id.as_str(), label: &folder.label, path, devices })
    }

    /// Each device that offers the folder remembers that it is ignored.
    pub fn ignore_folder(&self, folder: &PendingFolder, now: &str) -> Result<(), Failure> {
        folder.offered_by.iter().try_for_each(|device| self.ignore_folder_from(device, folder, now))
    }

    fn ignore_folder_from(&self, device: &DeviceId, folder: &PendingFolder, now: &str) -> Result<(), Failure> {
        let path = format!("/rest/config/devices/{device}");
        let current: wire::DeviceIgnores = self.get(&path)?;
        let entry = serde_json::to_value(wire::IgnoredFolder { time: now, id: folder.id.as_str(), label: &folder.label }).map_err(|error| Failure::Malformed(error.to_string()))?;
        let ignored_folders = current.ignored_folders.into_iter().chain([entry]).collect();
        self.send(Method::Patch, &path, &wire::DeviceIgnoresUpdate { ignored_folders })
    }

    /// Sets the web interface's login. Syncthing hashes the password with bcrypt
    /// before it stores it.
    pub fn set_login(&self, password: &Password) -> Result<(), Failure> {
        self.send(Method::Patch, "/rest/config/gui", &wire::Login { user: Password::USER, password: password.expose() })
    }

    /// Syncthing exits with status 3 (`--no-restart`); the supervisor starts it
    /// again. A login change just before restarts the web server inside the
    /// process, and a request in that moment finds nothing listening (seen with
    /// 2.1.3): keep asking for a few seconds.
    pub fn restart(&self) -> Result<(), Failure> {
        let deadline = std::time::Instant::now() + RESTART_RETRIES;
        loop {
            match self.post("/rest/system/restart") {
                Err(Failure::NotRunning) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
                result => return result,
            }
        }
    }

    /// Syncthing exits cleanly.
    pub fn shutdown(&self) -> Result<(), Failure> { self.post("/rest/system/shutdown") }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    const CONFIG: &str = r#"<configuration version="52">
    <device id="HJ56AJG" name="x"><address>dynamic</address></device>
    <gui enabled="true" tls="false" sendBasicAuthPrompt="false">
        <address>127.0.0.1:8384</address>
        <user>ghazal</user>
        <password>$2a$10$not.a.real.hash.for.tests</password>
        <apikey>k3yFromTheTestConfigXml0123456789</apikey>
    </gui>
    <ldap></ldap>
</configuration>"#;

    #[test]
    fn config_xml_yields_the_api_key_and_whether_a_login_is_set() {
        let gui = GuiConfig::parse(CONFIG);
        assert_eq!(gui, GuiConfig { api_key: ApiKey::parse("k3yFromTheTestConfigXml0123456789"), has_login: true });
        let fresh = GuiConfig::parse(&CONFIG.replace("        <user>ghazal</user>\n", "").replace("<password>$2a$10$not.a.real.hash.for.tests</password>", "<password></password>"));
        assert!(!fresh.has_login);
        assert!(fresh.api_key.is_some());
        assert_eq!(GuiConfig::parse("<configuration></configuration>"), GuiConfig { api_key: None, has_login: false });
        assert!(!format!("{:?}", gui.api_key).contains("k3yFrom"));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(query("abcd1-saves"), "abcd1-saves");
        assert_eq!(query("my saves/ü&x"), "my%20saves%2F%C3%BC%26x");
    }

    #[test]
    fn responses_split_into_status_and_body() {
        assert_eq!(response(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{}").unwrap(), (200, &b"{}"[..]));
        assert_eq!(response(b"HTTP/1.1 404 Not Found\r\n\r\nno such folder\n").unwrap(), (404, &b"no such folder\n"[..]));
        assert!(matches!(response(b"garbage"), Err(Failure::Malformed(_))));
        assert!(matches!(response(b"SSH-2.0\r\n\r\n"), Err(Failure::Malformed(_))));
    }

    /// Serves one canned reply and hands back the request it got.
    fn serve(reply: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            // Headers, then a body as long as Content-Length says.
            loop {
                let read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head.lines().find_map(|line| line.strip_prefix("Content-Length: ")).and_then(|value| value.parse().ok()).unwrap_or(0);
                    if body.len() >= length { break; }
                }
                if read == 0 { break; }
            }
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, handle)
    }

    fn client(address: SocketAddr) -> Client { Client::new(address, ApiKey::parse("secret-key").unwrap()) }

    #[test]
    fn requests_carry_the_key_and_a_json_body() {
        let (address, request) = serve("HTTP/1.0 200 OK\r\n\r\n");
        client(address).set_login(&Password::parse("abcd-efgh").unwrap()).unwrap();
        let request = request.join().unwrap();
        assert!(request.starts_with("PATCH /rest/config/gui HTTP/1.0\r\n"), "{request}");
        assert!(request.contains("\r\nX-API-Key: secret-key\r\n"));
        assert!(request.ends_with(r#"{"user":"ghazal","password":"abcd-efgh"}"#), "{request}");
    }

    #[test]
    fn errors_are_classified() {
        let (address, request) = serve("HTTP/1.0 403 Forbidden\r\n\r\nCSRF Error\n");
        assert_eq!(client(address).shutdown(), Err(Failure::Refused));
        request.join().unwrap();
        let (address, request) = serve("HTTP/1.0 404 Not Found\r\n\r\nno such folder\n");
        assert_eq!(client(address).rescan(&FolderId::parse("gone").unwrap()), Err(Failure::Status { code: 404, message: "no such folder".into() }));
        assert!(request.join().unwrap().starts_with("POST /rest/db/scan?folder=gone HTTP/1.0"));
        let (address, request) = serve("HTTP/1.0 200 OK\r\n\r\n{not json");
        assert!(matches!(client(address).summary(), Err(Failure::Malformed(reason)) if reason.starts_with("/rest/config/folders")));
        request.join().unwrap();
    }

    #[test]
    fn a_closed_port_means_not_running() {
        let address = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(client(address).shutdown(), Err(Failure::NotRunning));
    }
}
