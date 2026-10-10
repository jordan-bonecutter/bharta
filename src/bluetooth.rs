//! BlueZ access stays on one worker connection so discovery belongs to this panel.
mod agent;
pub use agent::Prompt;
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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
    pub battery: Option<u8>,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub adapters: Vec<(String, bool)>,
    pub devices: Vec<Device>,
    pub scanning: bool,
    pub scan_owned: bool,
    pub busy: Option<String>,
    pub prompt: Option<Prompt>,
    pub action_error: Option<String>,
}
impl Snapshot {
    pub fn enabled(&self) -> bool {
        self.adapters.iter().any(|(_, powered)| *powered)
    }
}
pub enum Request {
    Open(u64),
    Scan,
    Stop,
    Action(String, Action),
    Answer(Option<String>),
    Cancel,
    Close,
}
#[derive(Clone, Copy)]
pub enum Action {
    Pair,
    Connect,
    Disconnect,
}
pub type Update = (u64, Result<Snapshot, String>);

fn parse(objects: Objects) -> Snapshot {
    let mut snapshot = Snapshot::default();
    for (path, interfaces) in objects {
        if let Some(props) = interfaces.get(ADAPTER) {
            snapshot.scanning |= boolean(props, "Discovering");
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
                battery: interfaces
                    .get("org.bluez.Battery1")
                    .and_then(|p| p.get("Percentage"))
                    .and_then(|v| u8::try_from(v).ok())
                    .filter(|n| *n <= 100),
            });
        }
    }
    snapshot.adapters.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot.devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| {
                if !a.connected && !a.paired && !b.connected && !b.paired {
                    b.rssi.cmp(&a.rssi)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
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
    Session::new()?.snapshot()
}

struct Job {
    path: String,
    connection: Connection,
    result: mpsc::Receiver<Result<()>>,
    started: Instant,
    canceled: Arc<AtomicBool>,
    action: Action,
}
struct Session {
    connection: Connection,
    discovery: Vec<String>,
    deadline: Option<Instant>,
    job: Option<Job>,
    agent: agent::Shared,
    action_error: Option<String>,
}
impl Session {
    fn new() -> Result<Self> {
        Ok(Self {
            connection: Connection::system()
                .context("Bluetooth is unavailable: cannot reach the system bus")?,
            discovery: vec![],
            deadline: None,
            job: None,
            agent: Arc::new(Mutex::new(agent::State::default())),
            action_error: None,
        })
    }
    fn snapshot(&self) -> Result<Snapshot> {
        let objects = ObjectManagerProxy::builder(&self.connection)
            .destination(SERVICE)?
            .path("/")?
            .build()?
            .get_managed_objects()
            .map_err(bluez_error)?;
        let mut snapshot = parse(objects);
        snapshot.scan_owned = !self.discovery.is_empty();
        snapshot.scanning |= snapshot.scan_owned;
        snapshot.busy = self.job.as_ref().map(|j| j.path.clone());
        snapshot.prompt = self.agent.lock().unwrap().prompt.clone();
        snapshot.action_error = self.action_error.clone();
        Ok(snapshot)
    }
    fn action(&mut self, path: String, action: Action) -> Result<()> {
        anyhow::ensure!(
            self.job.is_none(),
            "A Bluetooth operation is already running"
        );
        anyhow::ensure!(
            self.snapshot()?.devices.iter().any(|d| d.path == path),
            "Bluetooth device disappeared; scan again"
        );
        self.stop();
        self.action_error = None;
        self.agent = Arc::new(Mutex::new(agent::State::default()));
        let canceled = Arc::new(AtomicBool::new(false));
        let connection = zbus::blocking::connection::Builder::system()?
            .serve_at(
                "/org/bharta/agent",
                agent::Agent {
                    device: path.clone(),
                    state: self.agent.clone(),
                    canceled: canceled.clone(),
                },
            )?
            .build()?;
        if matches!(action, Action::Pair) {
            let manager = Proxy::new(
                &connection,
                SERVICE,
                "/org/bluez",
                "org.bluez.AgentManager1",
            )?;
            let agent_path = OwnedObjectPath::try_from("/org/bharta/agent")?;
            manager.call::<_, _, ()>("RegisterAgent", &(agent_path, "KeyboardDisplay"))?;
        }
        let (tx, result) = mpsc::channel();
        self.job = Some(Job {
            path: path.clone(),
            connection: connection.clone(),
            result,
            started: Instant::now(),
            canceled: canceled.clone(),
            action,
        });
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let proxy = Proxy::new(&connection, SERVICE, path.as_str(), DEVICE)?;
                match action {
                    Action::Pair => {
                        proxy
                            .call::<_, _, ()>("Pair", &())
                            .context("Could not pair Bluetooth device")?;
                        anyhow::ensure!(!canceled.load(Ordering::SeqCst), "Pairing canceled");
                        proxy
                            .set_property("Trusted", true)
                            .context("Could not save Bluetooth trust")?;
                        proxy
                            .call::<_, _, ()>("Connect", &())
                            .context("Paired, but could not connect Bluetooth device")?;
                    }
                    Action::Connect => proxy
                        .call::<_, _, ()>("Connect", &())
                        .context("Could not connect Bluetooth device")?,
                    Action::Disconnect => proxy
                        .call::<_, _, ()>("Disconnect", &())
                        .context("Could not disconnect Bluetooth device")?,
                }
                if canceled.load(Ordering::SeqCst) && !matches!(action, Action::Disconnect) {
                    let _: zbus::Result<()> = proxy.call("Disconnect", &());
                    bail!("Bluetooth operation canceled");
                }
                Ok(())
            })();
            let _ = tx.send(result);
        });
        Ok(())
    }
    fn poll_job(&mut self) {
        if self
            .job
            .as_ref()
            .is_some_and(|j| j.started.elapsed() > Duration::from_secs(90))
        {
            self.cancel();
            self.action_error = Some(
                "Bluetooth operation timed out; put the device in pairing mode and try again"
                    .into(),
            );
            return;
        }
        let result = self.job.as_ref().and_then(|j| match j.result.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("Bluetooth operation stopped")))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(result) = result {
            self.job = None;
            self.agent.lock().unwrap().answer(None);
            self.action_error = result.err().map(|e| format!("{e:#}"));
        }
    }
    fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.canceled.store(true, Ordering::SeqCst);
            self.agent.lock().unwrap().answer(None);
            if let Ok(proxy) = Proxy::new(&job.connection, SERVICE, job.path.as_str(), DEVICE) {
                let method = if matches!(job.action, Action::Pair) {
                    "CancelPairing"
                } else {
                    "Disconnect"
                };
                let _: zbus::Result<()> = proxy.call(method, &());
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
            let result = Proxy::new(&self.connection, SERVICE, path.as_str(), ADAPTER)
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
        for path in self.discovery.drain(..) {
            if let Ok(proxy) = Proxy::new(&self.connection, SERVICE, path.as_str(), ADAPTER) {
                let _: zbus::Result<()> = proxy.call("StopDiscovery", &());
            }
        }
        self.deadline = None;
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel();
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
                Some(Request::Action(path, action)) if open.is_some() => {
                    if let Some(s) = session.as_mut() {
                        error = s.action(path, action).err();
                    }
                }
                Some(Request::Answer(answer)) => {
                    if let Some(s) = session.as_mut() {
                        s.agent.lock().unwrap().answer(answer);
                    }
                }
                Some(Request::Cancel) => {
                    if let Some(s) = session.as_mut() {
                        s.cancel();
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
                    session = Some(Session::new()?);
                }
                let s = session.as_mut().unwrap();
                if let Some(e) = error {
                    return Err(e);
                }
                s.poll_job();
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
    fn nearby_signals_are_strongest_first_after_paired_devices() {
        let mut objects = Objects::new();
        for (path, name, paired, rssi) in [
            ("/org/bluez/hci0/dev_saved", "Saved", true, Some(-90)),
            ("/org/bluez/hci0/dev_weak", "A weak", false, Some(-60)),
            ("/org/bluez/hci0/dev_strong", "Z strong", false, Some(-50)),
            ("/org/bluez/hci0/dev_unknown", "Unknown", false, None),
        ] {
            let mut props = device(Some(name), false, paired);
            if let Some(rssi) = rssi {
                props.insert("RSSI".into(), OwnedValue::from(rssi as i16));
            }
            objects.insert(
                path.try_into().unwrap(),
                HashMap::from([(DEVICE.try_into().unwrap(), props)]),
            );
        }
        objects.insert(
            "/org/bluez/hci0".try_into().unwrap(),
            HashMap::from([(
                ADAPTER.try_into().unwrap(),
                HashMap::from([("Discovering".into(), OwnedValue::from(true))]),
            )]),
        );
        let snapshot = parse(objects);
        assert_eq!(
            snapshot
                .devices
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["Saved", "Z strong", "A weak", "Unknown"]
        );
        assert!(
            snapshot.scanning,
            "Discovery by another client must be visible"
        );
        assert!(!snapshot.scan_owned);
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
            HashMap::from([
                (DEVICE.try_into().unwrap(), props),
                (
                    "org.bluez.Battery1".try_into().unwrap(),
                    HashMap::from([("Percentage".into(), OwnedValue::from(73u8))]),
                ),
            ]),
        );
        let s = parse(objects);
        assert!(s.enabled());
        assert_eq!(s.adapters.len(), 2);
        assert_eq!(s.devices[0].name, "Nearby mouse");
        assert_eq!(s.devices[0].rssi, Some(-64));
        assert_eq!(s.devices[0].battery, Some(73));
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
        assert_eq!(s.devices[2].battery, None);
        assert!(!Snapshot::default().enabled());
    }
}
