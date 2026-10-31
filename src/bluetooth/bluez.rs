//! BlueZ over D-Bus: reading adapters and devices, and the operations of the
//! Bluetooth screen. One `Session` serves the screen's worker and the
//! `ghazal bluetooth` command alike.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::dbus::{CallError, Connection, IncomingCall, MethodCall, Reply, Serve, Value};
use super::hardware;
use super::model::{Address, Device, Kind, Link, Observation, Operation, Radio};

const BUS: &str = "/run/dbus/system_bus_socket";
const SERVICE: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const AGENT_ROOT: &str = "/org/ghazal/agent";
const CALL: Duration = Duration::from_secs(10);
const PAIR: Duration = Duration::from_secs(45);
const CONNECT: Duration = Duration::from_secs(25);
/// `bluetoothd` is released by the supervisor within seconds, then waits for the kernel's controller.
const STACK_STARTUP: Duration = Duration::from_secs(30);
/// The PIN most gamepads that ask for one are built with.
const LEGACY_PIN: &str = "0000";

/// BlueZ asks this object to authenticate pairings. Devices that pair without
/// confirmation (the usual gamepad) never reach it; the rest get a fixed PIN,
/// and an unsolicited request to pair is refused unless the user is scanning or pairing.
struct Agent {
    accepting: bool,
    abort: Arc<AtomicBool>,
}

impl Serve for Agent {
    fn call(&mut self, call: &IncomingCall) -> Reply {
        let rejected = || Reply::Error { name: "org.bluez.Error.Rejected".into(), text: "not pairing".into() };
        match call.member.as_str() {
            "RequestPinCode" => Reply::Return(vec![Value::str(LEGACY_PIN)]),
            "RequestConfirmation" | "AuthorizeService" | "DisplayPinCode" | "DisplayPasskey" | "Cancel" | "Release" => Reply::Return(Vec::new()),
            "RequestAuthorization" if self.accepting => Reply::Return(Vec::new()),
            "RequestAuthorization" | "RequestPasskey" => rejected(),
            _ => Reply::Error { name: "org.freedesktop.DBus.Error.UnknownMethod".into(), text: String::new() },
        }
    }

    fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
}

struct Bus {
    connection: Connection,
    agent: Agent,
    registered: bool,
}

pub struct Session {
    socket: PathBuf,
    bus: Option<Bus>,
    abort: Arc<AtomicBool>,
    accepting: bool,
    last_problem: Option<String>,
}

impl Session {
    /// `abort` interrupts a blocked pairing or connection attempt from another thread.
    pub fn new(abort: Arc<AtomicBool>) -> Self {
        Self::at(BUS.into(), abort)
    }

    /// A session on another bus, for tests with a private daemon.
    pub fn at(socket: PathBuf, abort: Arc<AtomicBool>) -> Self {
        Self { socket, bus: None, abort, accepting: false, last_problem: None }
    }

    fn bus(&mut self) -> Result<&mut Bus, CallError> {
        let bus = match self.bus.take() {
            Some(bus) => bus,
            None => Bus {
                connection: Connection::open(&self.socket).map_err(CallError::Io)?,
                agent: Agent { accepting: self.accepting, abort: self.abort.clone() },
                registered: false,
            },
        };
        Ok(self.bus.insert(bus))
    }

    fn request(&mut self, call: MethodCall, timeout: Duration) -> Result<Vec<Value>, CallError> {
        let result = self.bus().and_then(|bus| bus.connection.call(&call, timeout, &mut bus.agent));
        if matches!(result, Err(CallError::Io(_))) {
            self.bus = None;
        }
        result
    }

    fn set_accepting(&mut self, accepting: bool) {
        self.accepting = accepting;
        if let Some(bus) = &mut self.bus {
            bus.agent.accepting = accepting;
        }
    }

    /// Answers calls from BlueZ for `duration`.
    pub fn idle(&mut self, duration: Duration) {
        let served = match &mut self.bus {
            Some(bus) => bus.connection.serve_for(duration, &mut bus.agent),
            None => {
                std::thread::sleep(duration);
                Ok(())
            }
        };
        if let Err(error) = served {
            eprintln!("ghazal: bluetooth: {error:#}");
            self.bus = None;
        }
    }

    fn objects(&mut self) -> Result<Parsed, CallError> {
        let call = bluez_call("/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects", Vec::new());
        let reply = self.request(call, CALL)?;
        Ok(parse(reply.first().unwrap_or(&Value::Array(super::dbus::Ty::Byte, Vec::new()))))
    }

    /// The radio and devices as BlueZ reports them. A missing daemon or adapter reads as off.
    pub fn observe(&mut self) -> Observation {
        match self.objects() {
            Ok(parsed) => {
                self.last_problem = None;
                parsed.observation
            }
            Err(error) => {
                let problem = friendly(&error);
                if self.last_problem.as_ref() != Some(&problem) {
                    eprintln!("ghazal: bluetooth: {problem}");
                    self.last_problem = Some(problem);
                }
                Observation { radio: Radio::Off, devices: Vec::new() }
            }
        }
    }

    fn adapter(&mut self) -> Result<String, String> {
        match self.objects() {
            Ok(Parsed { adapter: Some(path), .. }) => Ok(path),
            Ok(_) => Err("no Bluetooth adapter".into()),
            Err(error) => Err(friendly(&error)),
        }
    }

    fn set_property(&mut self, path: &str, interface: &str, name: &str, value: Value) -> Result<(), String> {
        let call = bluez_call(path, "org.freedesktop.DBus.Properties", "Set", vec![Value::str(interface), Value::str(name), Value::variant(value)]);
        self.request(call, CALL).map(drop).map_err(|error| friendly(&error))
    }

    fn invoke(&mut self, path: &str, interface: &str, member: &str, body: Vec<Value>, timeout: Duration) -> Result<(), String> {
        self.request(bluez_call(path, interface, member, body), timeout).map(drop).map_err(|error| friendly(&error))
    }

    pub fn execute(&mut self, operation: Operation) -> Result<(), String> {
        self.abort.store(false, Ordering::Relaxed);
        match operation {
            Operation::Power(true) => self.power_on(),
            Operation::Power(false) => self.power_off(),
            Operation::Pair(address) => self.pair(address),
            Operation::Connect(address) => {
                let adapter = self.adapter()?;
                let device = device_path(&adapter, address);
                self.set_property(&device, DEVICE, "Trusted", Value::Bool(true))?;
                self.connect(&device)
            }
            Operation::Disconnect(address) => {
                let adapter = self.adapter()?;
                self.invoke(&device_path(&adapter, address), DEVICE, "Disconnect", Vec::new(), CALL)
            }
            Operation::Forget(address) => {
                let adapter = self.adapter()?;
                let device = Value::path(&device_path(&adapter, address));
                self.invoke(&adapter, ADAPTER, "RemoveDevice", vec![device], CALL)
            }
        }
    }

    pub fn power_on(&mut self) -> Result<(), String> {
        hardware::start().map_err(|error| format!("{error:#}"))?;
        let adapter = self.wait_for_adapter()?;
        self.set_property(&adapter, ADAPTER, "Powered", Value::Bool(true))?;
        remember(true);
        Ok(())
    }

    fn power_off(&mut self) -> Result<(), String> {
        remember(false);
        // Never started: nothing to power down.
        match self.adapter() {
            Ok(adapter) => self.set_property(&adapter, ADAPTER, "Powered", Value::Bool(false)),
            Err(_) => Ok(()),
        }
    }

    fn wait_for_adapter(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + STACK_STARTUP;
        loop {
            match self.adapter() {
                Ok(adapter) => return Ok(adapter),
                Err(_) if self.abort.load(Ordering::Relaxed) => return Err("cancelled".into()),
                Err(problem) if Instant::now() >= deadline => return Err(format!("the adapter did not appear ({problem})")),
                Err(_) => std::thread::sleep(Duration::from_millis(300)),
            }
        }
    }

    pub fn scan(&mut self, on: bool) -> Result<(), String> {
        let adapter = self.adapter()?;
        if on {
            self.register_agent();
        }
        self.set_accepting(on);
        match self.request(bluez_call(&adapter, ADAPTER, if on { "StartDiscovery" } else { "StopDiscovery" }, Vec::new()), CALL) {
            Ok(_) => Ok(()),
            // Already scanning, or already stopped: the state asked for holds.
            Err(CallError::Remote { name, .. }) if name == "org.bluez.Error.InProgress" || (!on && name == "org.bluez.Error.Failed") => Ok(()),
            Err(error) => Err(friendly(&error)),
        }
    }

    /// Becomes BlueZ's default agent, so pairing needing a PIN or consent reaches us.
    fn register_agent(&mut self) {
        if self.bus().is_ok_and(|bus| bus.registered) {
            return;
        }
        let manager = |member: &str, body: Vec<Value>| bluez_call("/org/bluez", "org.bluez.AgentManager1", member, body);
        // One object per connection: a worker still finishing on its way out must not
        // own the registration of the next.
        let Ok(name) = self.bus().map(|bus| bus.connection.name().trim_start_matches(':').replace('.', "_")) else { return };
        let agent = Value::path(&format!("{AGENT_ROOT}/{name}"));
        let registered = match self.request(manager("RegisterAgent", vec![agent.clone(), Value::str("NoInputNoOutput")]), CALL) {
            Ok(_) => true,
            Err(CallError::Remote { name, .. }) if name == "org.bluez.Error.AlreadyExists" => true,
            Err(error) => {
                eprintln!("ghazal: bluetooth: registering the pairing agent: {}", friendly(&error));
                false
            }
        };
        if registered {
            if let Err(error) = self.request(manager("RequestDefaultAgent", vec![agent]), CALL) {
                eprintln!("ghazal: bluetooth: becoming the default agent: {}", friendly(&error));
            }
        }
        if let Some(bus) = &mut self.bus {
            bus.registered = registered;
        }
    }

    fn pair(&mut self, address: Address) -> Result<(), String> {
        let adapter = self.adapter()?;
        let device = device_path(&adapter, address);
        self.register_agent();
        // Inquiry shares the radio with the connection being made: pairing is
        // more reliable without it.
        let _ = self.request(bluez_call(&adapter, ADAPTER, "StopDiscovery", Vec::new()), CALL);
        self.set_accepting(true);
        let paired = self.request(bluez_call(&device, DEVICE, "Pair", Vec::new()), PAIR);
        self.set_accepting(false);
        match paired {
            Ok(_) => {}
            Err(CallError::Remote { name, .. }) if name == "org.bluez.Error.AlreadyExists" => {}
            Err(CallError::Aborted) => {
                // Calls refuse to start while the flag is up, so lower it before undoing the attempt.
                self.abort.store(false, Ordering::Relaxed);
                let _ = self.request(bluez_call(&device, DEVICE, "CancelPairing", Vec::new()), CALL);
                return Err("cancelled".into());
            }
            Err(error) => return Err(friendly(&error)),
        }
        // Trusted devices may reconnect on their own, which is how a gamepad comes back.
        self.set_property(&device, DEVICE, "Trusted", Value::Bool(true))?;
        // Paired is paired even if the first connection fails: the list shows the link.
        if let Err(reason) = self.connect(&device) {
            eprintln!("ghazal: bluetooth: paired {address} but connecting failed: {reason}");
        }
        Ok(())
    }

    fn connect(&mut self, device: &str) -> Result<(), String> {
        match self.request(bluez_call(device, DEVICE, "Connect", Vec::new()), CONNECT) {
            Ok(_) => Ok(()),
            Err(CallError::Remote { name, .. }) if name == "org.bluez.Error.AlreadyConnected" => Ok(()),
            Err(CallError::Aborted) => {
                self.abort.store(false, Ordering::Relaxed);
                let _ = self.request(bluez_call(device, DEVICE, "Disconnect", Vec::new()), CALL);
                Err("cancelled".into())
            }
            Err(error) => Err(friendly(&error)),
        }
    }

    /// Leaves the radio as the screen found it: no scan outlives the screen.
    pub fn shutdown(&mut self) {
        // Leaving the screen raised the flag to interrupt whatever was running.
        self.abort.store(false, Ordering::Relaxed);
        if self.bus.is_some() {
            if let Ok(adapter) = self.adapter() {
                let _ = self.request(bluez_call(&adapter, ADAPTER, "StopDiscovery", Vec::new()), CALL);
            }
        }
        self.bus = None;
    }
}

fn remember(enabled: bool) {
    if let Err(error) = (hardware::Preference { enabled }).save() {
        eprintln!("ghazal: bluetooth: {error:#}");
    }
}

fn bluez_call(path: &str, interface: &str, member: &str, body: Vec<Value>) -> MethodCall {
    MethodCall { destination: SERVICE.into(), path: path.into(), interface: interface.into(), member: member.into(), body }
}

fn device_path(adapter: &str, address: Address) -> String {
    format!("{adapter}/{}", address.object_name())
}

/// A message for the screen's notice line.
fn friendly(error: &CallError) -> String {
    match error {
        CallError::Remote { name, text } => match name.as_str() {
            "org.bluez.Error.AuthenticationFailed" => "authentication failed".into(),
            "org.bluez.Error.AuthenticationCanceled" => "cancelled".into(),
            "org.bluez.Error.AuthenticationRejected" => "the device rejected pairing".into(),
            "org.bluez.Error.AuthenticationTimeout" => "pairing timed out".into(),
            "org.bluez.Error.ConnectionAttemptFailed" => "could not connect".into(),
            "org.bluez.Error.NotReady" => "the adapter is not ready".into(),
            "org.bluez.Error.InProgress" => "another operation is in progress".into(),
            "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner" => "the Bluetooth service is not running".into(),
            _ if text.is_empty() => name.rsplit('.').next().unwrap_or(name).to_lowercase(),
            _ => text.to_lowercase(),
        },
        other => other.to_string(),
    }
}

struct Parsed {
    /// Object path of the first adapter.
    adapter: Option<String>,
    observation: Observation,
}

type Properties<'a> = BTreeMap<&'a str, &'a Value>;

fn properties<'a>(interfaces: &'a Value, wanted: &str) -> Option<Properties<'a>> {
    let (_, values) = interfaces.entries().find(|(name, _)| name.as_str() == Some(wanted))?;
    Some(values.entries().filter_map(|(key, value)| Some((key.as_str()?, value))).collect())
}

/// Reads the reply of `GetManagedObjects` (`a{oa{sa{sv}}}`).
fn parse(objects: &Value) -> Parsed {
    let mut adapter = None;
    let mut radio = Radio::Off;
    for (path, interfaces) in objects.entries() {
        if let (Some(path), Some(properties)) = (path.as_str(), properties(interfaces, ADAPTER)) {
            let flag = |name: &str| properties.get(name).and_then(|value| value.as_bool()).unwrap_or(false);
            if adapter.is_none() {
                adapter = Some(path.to_owned());
                radio = if flag("Powered") { Radio::On { scanning: flag("Discovering") } } else { Radio::Off };
            }
        }
    }
    let mut devices: Vec<Device> = match &adapter {
        Some(adapter) => objects
            .entries()
            .filter(|(path, _)| path.as_str().is_some_and(|path| path.starts_with(&format!("{adapter}/dev_"))))
            .filter_map(|(_, interfaces)| device(&properties(interfaces, DEVICE)?))
            .collect(),
        None => Vec::new(),
    };
    devices.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.rssi.cmp(&a.rssi)).then_with(|| a.name.cmp(&b.name)));
    Parsed { adapter, observation: Observation { radio, devices } }
}

fn rank(device: &Device) -> u8 {
    match (device.link, device.kind) {
        (Link::Paired { connected: true }, _) => 0,
        (Link::Paired { connected: false }, _) => 1,
        (Link::New, Kind::Controller) => 2,
        (Link::New, Kind::Audio) => 3,
        (Link::New, Kind::Other) => 4,
    }
}

fn device(properties: &Properties) -> Option<Device> {
    let text = |name: &str| properties.get(name).and_then(|value| value.as_str());
    let flag = |name: &str| properties.get(name).and_then(|value| value.as_bool()).unwrap_or(false);
    let address: Address = text("Address")?.parse().ok()?;
    let paired = flag("Paired");
    // Scanning also finds nameless beacons; only a device that says what it is, or one we know, is listed.
    if text("Name").is_none() && !paired {
        return None;
    }
    let link = if paired { Link::Paired { connected: flag("Connected") } } else { Link::New };
    Some(Device {
        address,
        name: text("Alias").or_else(|| text("Name")).map_or_else(|| address.to_string(), str::to_owned),
        kind: kind(properties),
        link,
        rssi: properties.get("RSSI").and_then(|value| value.as_i16()),
    })
}

/// Gamepads announce themselves through the icon BlueZ derives, the class of
/// device (peripheral: joystick or gamepad) or the GATT appearance.
fn kind(properties: &Properties) -> Kind {
    let icon = properties.get("Icon").and_then(|value| value.as_str()).unwrap_or("");
    let class = properties.get("Class").and_then(|value| value.as_u32());
    let appearance = properties.get("Appearance").and_then(|value| value.as_u16());
    let major = class.map(|class| (class >> 8) & 0x1f);
    let minor = class.map(|class| (class >> 2) & 0xf);
    if icon == "input-gaming" || (major == Some(5) && matches!(minor, Some(1 | 2))) || matches!(appearance, Some(0x03C3 | 0x03C4)) {
        Kind::Controller
    } else if icon.starts_with("audio-") || major == Some(4) {
        Kind::Audio
    } else {
        Kind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::super::dbus::Ty;
    use super::*;

    fn entry(key: Value, value: Value) -> Value {
        Value::Entry(Box::new(key), Box::new(value))
    }

    fn dict(entries: Vec<Value>) -> Value {
        Value::Array(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Variant)), entries)
    }

    fn interface(name: &str, properties: Vec<(&str, Value)>) -> Value {
        let properties = properties.into_iter().map(|(key, value)| entry(Value::str(key), Value::variant(value))).collect();
        entry(Value::str(name), dict(properties))
    }

    fn object(path: &str, interfaces: Vec<Value>) -> Value {
        let interfaces = Value::Array(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Array(Box::new(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Variant)))))), interfaces);
        entry(Value::path(path), interfaces)
    }

    fn device_object(last: &str, properties: Vec<(&str, Value)>) -> Value {
        let mut all = vec![("Address", Value::str(&format!("00:11:22:33:44:{last}")))];
        all.extend(properties);
        object(&format!("/org/bluez/hci0/dev_00_11_22_33_44_{last}"), vec![interface(DEVICE, all)])
    }

    fn managed(adapter: Option<(bool, bool)>, devices: Vec<Value>) -> Value {
        let mut objects = Vec::new();
        if let Some((powered, discovering)) = adapter {
            objects.push(object("/org/bluez/hci0", vec![interface(ADAPTER, vec![("Powered", Value::Bool(powered)), ("Discovering", Value::Bool(discovering))])]));
        }
        objects.extend(devices);
        Value::Array(Ty::Entry(Box::new(Ty::Path), Box::new(Ty::Array(Box::new(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Array(Box::new(Ty::Entry(Box::new(Ty::Str), Box::new(Ty::Variant)))))))))), objects)
    }

    #[test]
    fn no_adapter_reads_as_off() {
        let parsed = parse(&managed(None, vec![]));
        assert_eq!(parsed.adapter, None);
        assert_eq!(parsed.observation, Observation { radio: Radio::Off, devices: vec![] });
    }

    #[test]
    fn the_adapter_reports_power_and_scanning() {
        for (powered, discovering, radio) in [(false, false, Radio::Off), (true, false, Radio::On { scanning: false }), (true, true, Radio::On { scanning: true })] {
            let parsed = parse(&managed(Some((powered, discovering)), vec![]));
            assert_eq!(parsed.adapter.as_deref(), Some("/org/bluez/hci0"));
            assert_eq!(parsed.observation.radio, radio);
        }
    }

    #[test]
    fn devices_are_classified_filtered_and_ordered() {
        let objects = managed(
            Some((true, true)),
            vec![
                // Nameless beacons are hidden.
                device_object("01", vec![("Paired", Value::Bool(false)), ("RSSI", Value::I16(-40))]),
                device_object("02", vec![("Name", Value::str("Speaker")), ("Alias", Value::str("Speaker")), ("Icon", Value::str("audio-card")), ("RSSI", Value::I16(-30))]),
                device_object("03", vec![("Name", Value::str("Pad")), ("Alias", Value::str("Pad")), ("Icon", Value::str("input-gaming")), ("RSSI", Value::I16(-70))]),
                device_object("04", vec![("Name", Value::str("Far pad")), ("Alias", Value::str("Far pad")), ("Class", Value::U32(0x2508)), ("RSSI", Value::I16(-80))]),
                device_object("05", vec![("Alias", Value::str("Mine")), ("Paired", Value::Bool(true)), ("Connected", Value::Bool(false)), ("Icon", Value::str("input-gaming"))]),
                device_object("06", vec![("Name", Value::str("Live")), ("Alias", Value::str("Live")), ("Paired", Value::Bool(true)), ("Connected", Value::Bool(true)), ("Appearance", Value::U16(0x03C4))]),
                device_object("07", vec![("Name", Value::str("Watch")), ("Alias", Value::str("Watch")), ("RSSI", Value::I16(-20))]),
                // Another adapter's device is not ours.
                object("/org/bluez/hci1/dev_00_11_22_33_44_08", vec![interface(DEVICE, vec![("Address", Value::str("00:11:22:33:44:08")), ("Name", Value::str("Elsewhere"))])]),
            ],
        );
        let devices = parse(&objects).observation.devices;
        let summary: Vec<(&str, Kind, Link)> = devices.iter().map(|device| (device.name.as_str(), device.kind, device.link)).collect();
        assert_eq!(
            summary,
            [
                ("Live", Kind::Controller, Link::Paired { connected: true }),
                ("Mine", Kind::Controller, Link::Paired { connected: false }),
                ("Pad", Kind::Controller, Link::New),
                ("Far pad", Kind::Controller, Link::New),
                ("Speaker", Kind::Audio, Link::New),
                ("Watch", Kind::Other, Link::New),
            ]
        );
        assert_eq!(devices[2].rssi, Some(-70));
        assert_eq!(devices[2].address.to_string(), "00:11:22:33:44:03");
    }

    #[test]
    fn errors_read_as_plain_sentences() {
        let remote = |name: &str, text: &str| CallError::Remote { name: name.into(), text: text.into() };
        assert_eq!(friendly(&remote("org.bluez.Error.AuthenticationFailed", "x")), "authentication failed");
        assert_eq!(friendly(&remote("org.freedesktop.DBus.Error.ServiceUnknown", "x")), "the Bluetooth service is not running");
        assert_eq!(friendly(&remote("org.bluez.Error.Failed", "Protocol not available")), "protocol not available");
        assert_eq!(friendly(&remote("org.bluez.Error.Rejected", "")), "rejected");
        assert_eq!(friendly(&CallError::Timeout), "no reply from the Bluetooth service");
        assert_eq!(friendly(&CallError::Aborted), "cancelled");
    }

    #[test]
    fn the_agent_answers_what_pairing_asks() {
        let mut agent = Agent { accepting: false, abort: Arc::new(AtomicBool::new(false)) };
        let ask = |agent: &mut Agent, member: &str| {
            agent.call(&IncomingCall {
                serial: 1,
                sender: ":1.2".into(),
                path: AGENT_ROOT.into(),
                interface: Some("org.bluez.Agent1".into()),
                member: member.into(),
                body: Vec::new(),
                expects_reply: true,
            })
        };
        assert_eq!(ask(&mut agent, "RequestPinCode"), Reply::Return(vec![Value::str("0000")]));
        assert_eq!(ask(&mut agent, "RequestConfirmation"), Reply::Return(Vec::new()));
        assert!(matches!(ask(&mut agent, "RequestAuthorization"), Reply::Error { .. }));
        agent.accepting = true;
        assert_eq!(ask(&mut agent, "RequestAuthorization"), Reply::Return(Vec::new()));
        assert!(matches!(ask(&mut agent, "RequestPasskey"), Reply::Error { .. }));
        assert!(matches!(ask(&mut agent, "Frobnicate"), Reply::Error { .. }));
        assert!(!agent.aborted());
        agent.abort.store(true, Ordering::Relaxed);
        assert!(agent.aborted());
    }

    /// What the fake BlueZ below believes.
    #[derive(Default)]
    struct World {
        powered: bool,
        discovering: bool,
        paired: bool,
        trusted: bool,
        connected: bool,
        forgotten: bool,
        /// Pair() takes a while, long enough to be cancelled.
        slow_pair: bool,
        calls: Vec<String>,
    }

    impl World {
        fn objects(&self) -> Value {
            let mut device = vec![
                ("Address", Value::str("00:11:22:33:44:55")),
                ("Name", Value::str("Pad")),
                ("Alias", Value::str("Pad")),
                ("Icon", Value::str("input-gaming")),
                ("Paired", Value::Bool(self.paired)),
                ("Connected", Value::Bool(self.connected)),
            ];
            if !self.paired {
                device.push(("RSSI", Value::I16(-50)));
            }
            managed(Some((self.powered, self.discovering)), vec![object("/org/bluez/hci0/dev_00_11_22_33_44_55", vec![interface(DEVICE, device)])])
        }
    }

    struct FakeBluez {
        world: Arc<std::sync::Mutex<World>>,
    }

    impl Serve for FakeBluez {
        fn call(&mut self, call: &IncomingCall) -> Reply {
            let ok = Reply::Return(Vec::new());
            let slow = {
                let mut world = self.world.lock().unwrap();
                world.calls.push(call.member.clone());
                call.member == "Pair" && world.slow_pair
            };
            if slow {
                std::thread::sleep(Duration::from_millis(600));
            }
            let mut world = self.world.lock().unwrap();
            match call.member.as_str() {
                "GetManagedObjects" => return Reply::Return(vec![world.objects()]),
                "Set" => match (call.body[1].as_str(), call.body[2].as_bool()) {
                    (Some("Powered"), Some(on)) => world.powered = on,
                    (Some("Trusted"), Some(on)) => world.trusted = on,
                    other => return Reply::Error { name: "org.bluez.Error.Failed".into(), text: format!("cannot set {other:?}") },
                },
                "StartDiscovery" => world.discovering = true,
                "StopDiscovery" => world.discovering = false,
                "Pair" => world.paired = true,
                "Connect" => world.connected = true,
                "Disconnect" => world.connected = false,
                "RemoveDevice" => (world.paired, world.connected, world.forgotten) = (false, false, true),
                "RegisterAgent" | "RequestDefaultAgent" | "CancelPairing" => {}
                other => return Reply::Error { name: "org.bluez.Error.Failed".into(), text: format!("unexpected {other}") },
            }
            ok
        }

        fn aborted(&self) -> bool {
            false
        }
    }

    /// Drives `Session` against a stand-in for bluetoothd on a real `dbus-daemon`:
    /// `GHAZAL_TEST_DBUS_DAEMON=$(command -v dbus-daemon) cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a dbus-daemon binary"]
    fn a_session_works_a_device_through_a_fake_bluez() {
        use super::super::dbus::{Connection, Deaf};
        use super::super::model::Operation;
        super::super::dbus::testing::with_daemon(|socket| {
            let abort = Arc::new(AtomicBool::new(false));
            let mut session = Session::at(socket.to_owned(), abort.clone());
            let off = Observation { radio: Radio::Off, devices: Vec::new() };
            // No BlueZ on the bus yet: the radio reads as off.
            assert_eq!(session.observe(), off);

            let world = Arc::new(std::sync::Mutex::new(World::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let (ready, started) = std::sync::mpsc::channel();
            let service = {
                let (world, stop, socket) = (world.clone(), stop.clone(), socket.to_owned());
                std::thread::spawn(move || {
                    let mut connection = Connection::open(&socket).unwrap();
                    let claim = MethodCall {
                        destination: "org.freedesktop.DBus".into(),
                        path: "/org/freedesktop/DBus".into(),
                        interface: "org.freedesktop.DBus".into(),
                        member: "RequestName".into(),
                        body: vec![Value::str(SERVICE), Value::U32(0)],
                    };
                    connection.call(&claim, Duration::from_secs(5), &mut Deaf).unwrap();
                    ready.send(()).unwrap();
                    let mut bluez = FakeBluez { world };
                    while !stop.load(Ordering::Relaxed) {
                        connection.serve_for(Duration::from_millis(50), &mut bluez).unwrap();
                    }
                })
            };
            started.recv().unwrap();
            let pad: Address = "00:11:22:33:44:55".parse().unwrap();
            let observed = |session: &mut Session| session.observe();

            // Powered off, then on, with the pad in range but unpaired.
            assert_eq!(observed(&mut session).radio, Radio::Off);
            session.set_property("/org/bluez/hci0", ADAPTER, "Powered", Value::Bool(true)).unwrap();
            let seen = observed(&mut session);
            assert_eq!((seen.radio, seen.devices.len(), seen.devices[0].link, seen.devices[0].rssi), (Radio::On { scanning: false }, 1, Link::New, Some(-50)));

            // Scanning registers the pairing agent first.
            session.scan(true).unwrap();
            assert_eq!(observed(&mut session).radio, Radio::On { scanning: true });
            assert!(world.lock().unwrap().calls.iter().any(|call| call == "RegisterAgent") && world.lock().unwrap().calls.iter().any(|call| call == "RequestDefaultAgent"));

            // Pairing stops the scan, trusts the pad and connects it.
            session.execute(Operation::Pair(pad)).unwrap();
            {
                let world = world.lock().unwrap();
                assert!(!world.discovering && world.paired && world.trusted && world.connected);
            }
            assert_eq!(observed(&mut session).devices[0].link, Link::Paired { connected: true });
            session.execute(Operation::Disconnect(pad)).unwrap();
            assert_eq!(observed(&mut session).devices[0].link, Link::Paired { connected: false });
            session.execute(Operation::Connect(pad)).unwrap();
            assert_eq!(observed(&mut session).devices[0].link, Link::Paired { connected: true });
            session.execute(Operation::Forget(pad)).unwrap();
            assert!(world.lock().unwrap().forgotten);
            assert_eq!(observed(&mut session).devices[0].link, Link::New);

            // A pairing that hangs can be cancelled, and BlueZ is told.
            world.lock().unwrap().slow_pair = true;
            let canceller = {
                let abort = abort.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    abort.store(true, Ordering::Relaxed);
                })
            };
            assert_eq!(session.execute(Operation::Pair(pad)), Err("cancelled".to_owned()));
            canceller.join().unwrap();
            assert!(!abort.load(Ordering::Relaxed), "the flag is lowered so BlueZ can be told");
            std::thread::sleep(Duration::from_millis(900));
            assert!(world.lock().unwrap().calls.iter().any(|call| call == "CancelPairing"));

            stop.store(true, Ordering::Relaxed);
            service.join().unwrap();
            session.shutdown();
        });
    }
}
