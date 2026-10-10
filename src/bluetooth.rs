//! BlueZ discovery and read-only kernel device enumeration share one worker.
mod kernel;
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    sync::mpsc,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy, fdo::ObjectManagerProxy},
    names::OwnedInterfaceName,
    zvariant::{OwnedObjectPath, OwnedValue},
};

const SERVICE: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
type Objects = HashMap<OwnedObjectPath, HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>>;

#[derive(Clone, Debug)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub address: String,
    pub connected: bool,
    pub paired: bool,
    pub rssi: Option<i16>,
}
#[derive(Clone, Debug)]
pub struct Receiver {
    pub path: String,
    pub name: String,
    pub id: String,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub bluez: bool,
    pub notice: Option<String>,
    pub receivers: Vec<Receiver>,
    pub adapters: Vec<(String, bool)>,
    pub devices: Vec<Device>,
    pub scanning: bool,
}
impl Snapshot {
    pub fn enabled(&self) -> bool {
        self.bluez && self.adapters.iter().any(|(_, powered)| *powered)
    }
}
pub enum Request {
    Open(u64),
    Scan,
    Stop,
    Close,
}
pub type Update = (u64, Result<Snapshot, String>);

fn parse(objects: Objects) -> Snapshot {
    let mut snapshot = Snapshot {
        bluez: true,
        ..Snapshot::default()
    };
    for (path, interfaces) in objects {
        if let Some(props) = interfaces.get(ADAPTER) {
            snapshot
                .adapters
                .push((path.to_string(), boolean(props, "Powered")));
        }
        if let Some(props) = interfaces.get(DEVICE) {
            let address = string(props, "Address").unwrap_or_else(|| path.to_string());
            let name = string(props, "Alias")
                .or_else(|| string(props, "Name"))
                .unwrap_or_else(|| address.clone());
            snapshot.devices.push(Device {
                path: path.to_string(),
                name,
                address,
                connected: boolean(props, "Connected"),
                paired: boolean(props, "Paired"),
                rssi: props.get("RSSI").and_then(|v| i16::try_from(v).ok()),
            });
        }
    }
    snapshot.adapters.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot.devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.path.cmp(&b.path))
    });
    snapshot
}
fn string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}
// Keep the underlying D-Bus error in diagnostics; missing services and denied
// access need different remedies, neither means the adapter is powered off.
fn bluez_error(error: zbus::fdo::Error) -> anyhow::Error {
    let explanation = match &error {
        zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_) => {
            "Bluetooth discovery service is unavailable"
        }
        zbus::fdo::Error::AccessDenied(_) | zbus::fdo::Error::AuthFailed(_) => {
            "Bluetooth access denied. Your user needs permission to access org.bluez on the system bus"
        }
        _ => "Cannot read Bluetooth devices from BlueZ",
    };
    anyhow::Error::new(error).context(explanation)
}

/// Read-only diagnostics: never starts discovery or changes adapter settings.
pub fn snapshot() -> Result<Snapshot> {
    Session::new().snapshot()
}

struct Session {
    connection: Option<Connection>,
    connection_error: Option<String>,
    discovery: Vec<String>,
    deadline: Option<Instant>,
}
impl Session {
    fn new() -> Self {
        let (connection, connection_error) = match Connection::system() {
            Ok(c) => (Some(c), None),
            Err(e) => (
                None,
                Some(format!("Cannot reach the Bluetooth system bus: {e}")),
            ),
        };
        Self {
            connection,
            connection_error,
            discovery: vec![],
            deadline: None,
        }
    }
    fn snapshot(&self) -> Result<Snapshot> {
        let mut kernel = kernel::read();
        let bluez: Result<Snapshot> = (|| {
            let connection = self.connection.as_ref().context(
                self.connection_error
                    .clone()
                    .unwrap_or_else(|| "Bluetooth system bus is unavailable".into()),
            )?;
            let objects = ObjectManagerProxy::builder(connection)
                .destination(SERVICE)?
                .path("/")?
                .build()?
                .get_managed_objects()
                .map_err(bluez_error)?;
            Ok(parse(objects))
        })();
        match bluez {
            Ok(mut s) => {
                s.receivers = kernel.receivers;
                s.scanning = !self.discovery.is_empty();
                Ok(s)
            }
            Err(e) => {
                kernel.notice = Some(format!("{e:#}"));
                Ok(kernel)
            }
        }
    }
    fn scan(&mut self) -> Result<()> {
        if !self.discovery.is_empty() {
            return Ok(());
        }
        let snapshot = self.snapshot()?;
        if !snapshot.enabled() {
            bail!("No powered Bluetooth adapter");
        }
        for (path, powered) in snapshot.adapters {
            if !powered {
                continue;
            }
            let result = Proxy::new(
                self.connection
                    .as_ref()
                    .context("Bluetooth system bus is unavailable")?,
                SERVICE,
                path.as_str(),
                ADAPTER,
            )
            .and_then(|p| p.call::<_, _, ()>("StartDiscovery", &()));
            if let Err(e) = result {
                self.stop();
                return Err(e).context("Could not scan for Bluetooth devices");
            }
            self.discovery.push(path);
        }
        self.deadline = Some(Instant::now() + Duration::from_secs(12));
        Ok(())
    }
    fn stop(&mut self) {
        let Some(connection) = &self.connection else {
            return;
        };
        for path in self.discovery.drain(..) {
            if let Ok(proxy) = Proxy::new(connection, SERVICE, path.as_str(), ADAPTER) {
                let _: zbus::Result<()> = proxy.call("StopDiscovery", &());
            }
        }
        self.deadline = None;
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn watch() -> (mpsc::Sender<Request>, mpsc::Receiver<Update>) {
    let (tx, requests) = mpsc::channel();
    let (updates, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut session: Option<Session> = None;
        let mut open = None;
        loop {
            let request = if open.is_some() {
                match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(r) => Some(r),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match requests.recv() {
                    Ok(r) => Some(r),
                    Err(_) => break,
                }
            };
            let mut error = None;
            match request {
                Some(Request::Close) => {
                    session = None;
                    open = None;
                    continue;
                }
                Some(Request::Open(id)) => {
                    open = Some(id);
                }
                Some(Request::Scan) if open.is_some() => {
                    if let Some(s) = session.as_mut() {
                        error = s.scan().err();
                    }
                }
                Some(Request::Stop) => {
                    // Dropping the connection also releases discovery if StopDiscovery fails.
                    session = None;
                }
                _ => {}
            }
            let Some(id) = open else {
                continue;
            };
            let result = (|| {
                if session
                    .as_ref()
                    .is_some_and(|s| s.deadline.is_some_and(|d| Instant::now() >= d))
                {
                    session = None;
                }
                if session.is_none() {
                    session = Some(Session::new());
                }
                let s = session.as_mut().unwrap();
                if let Some(e) = error {
                    return Err(e);
                }
                s.snapshot()
            })()
            .map_err(|e| format!("{e:#}"));
            if result.is_err() {
                session = None;
            }
            if updates.send((id, result)).is_err() {
                break;
            }
        }
    });
    (tx, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_and_permission_errors_explain_the_remedy_and_keep_the_cause() {
        let missing = bluez_error(zbus::fdo::Error::ServiceUnknown(
            "The name is not activatable".into(),
        ));
        let message = format!("{missing:#}");
        assert!(message.contains("discovery service is unavailable"));
        assert!(message.contains("not activatable"));
        let denied = bluez_error(zbus::fdo::Error::AccessDenied(
            "Policy rejected the call".into(),
        ));
        let message = format!("{denied:#}");
        assert!(message.contains("permission"));
        assert!(message.contains("Policy rejected"));
        assert!(!message.contains("discovery service is unavailable"));
    }
    fn device(name: Option<&str>, connected: bool, paired: bool) -> HashMap<String, OwnedValue> {
        let mut props = HashMap::from([
            ("Connected".into(), OwnedValue::from(connected)),
            ("Paired".into(), OwnedValue::from(paired)),
            ("Address".into(), zbus::zvariant::Str::from("AA:BB").into()),
        ]);
        if let Some(name) = name {
            props.insert("Alias".into(), zbus::zvariant::Str::from(name).into());
        }
        props
    }
    #[test]
    fn handles_multiple_adapters_and_unnamed_devices() {
        let mut objects = Objects::new();
        for (path, powered) in [("/org/bluez/hci0", false), ("/org/bluez/hci1", true)] {
            objects.insert(
                path.try_into().unwrap(),
                HashMap::from([(
                    ADAPTER.try_into().unwrap(),
                    HashMap::from([("Powered".into(), OwnedValue::from(powered))]),
                )]),
            );
        }
        let mut props = device(Some(""), false, false);
        props.insert(
            "Name".into(),
            zbus::zvariant::Str::from("Nearby mouse").into(),
        );
        props.insert("RSSI".into(), OwnedValue::from(-64i16));
        objects.insert(
            "/org/bluez/hci1/dev_a".try_into().unwrap(),
            HashMap::from([(DEVICE.try_into().unwrap(), props)]),
        );
        let s = parse(objects);
        assert!(s.enabled());
        assert_eq!(s.adapters.len(), 2);
        assert_eq!(s.devices[0].name, "Nearby mouse");
        assert_eq!(s.devices[0].rssi, Some(-64));
    }
    #[test]
    fn groups_connected_saved_and_available_with_optional_properties() {
        let mut objects = Objects::new();
        for (path, props) in [
            (
                "/org/bluez/hci0/dev_a",
                device(Some("Z headphones"), true, true),
            ),
            (
                "/org/bluez/hci0/dev_b",
                device(Some("A keyboard"), false, true),
            ),
            ("/org/bluez/hci0/dev_c", device(None, false, false)),
        ] {
            objects.insert(
                path.try_into().unwrap(),
                HashMap::from([(DEVICE.try_into().unwrap(), props)]),
            );
        }
        objects.insert(
            "/org/bluez/hci0".try_into().unwrap(),
            HashMap::from([(
                ADAPTER.try_into().unwrap(),
                HashMap::from([("Powered".into(), OwnedValue::from(true))]),
            )]),
        );
        let s = parse(objects);
        assert!(s.enabled());
        assert_eq!(
            s.devices
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["Z headphones", "A keyboard", "AA:BB"]
        );
        assert_eq!(s.devices[2].rssi, None);
        assert!(!Snapshot::default().enabled());
    }
}
