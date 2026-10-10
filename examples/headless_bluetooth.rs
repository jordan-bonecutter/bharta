//! Fake BlueZ on the harness's private D-Bus, never the desktop system bus.
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zbus::blocking::{Connection, connection::Builder};

struct Adapter {
    scanning: Arc<AtomicBool>,
    log: String,
}
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    fn start_discovery(&self) {
        self.scanning.store(true, Ordering::SeqCst);
        self.record("start");
    }
    fn stop_discovery(&self) {
        self.scanning.store(false, Ordering::SeqCst);
        self.record("stop");
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.scanning.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
    }
}
impl Adapter {
    fn record(&self, event: &str) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.log) {
            let _ = writeln!(file, "{event}");
        }
    }
}
struct Device {
    name: &'static str,
    connected: AtomicBool,
    paired: AtomicBool,
    trusted: AtomicBool,
    agents: Arc<Mutex<std::collections::HashMap<String, String>>>,
}
impl Device {
    fn record(&self, event: &str) {
        let log = std::env::var("BHARTA_TEST_BLUETOOTH_LOG").unwrap() + ".actions";
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        writeln!(file, "{event}").unwrap();
    }
}
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    async fn pair(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header.sender().unwrap().to_string();
        let agent = self
            .agents
            .lock()
            .unwrap()
            .get(&sender)
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::Failed("No registered agent".into()))?;
        let proxy = zbus::Proxy::new(
            connection,
            sender.as_str(),
            agent.as_str(),
            "org.bluez.Agent1",
        )
        .await?;
        let device =
            zbus::zvariant::OwnedObjectPath::try_from(header.path().unwrap().as_str()).unwrap();
        self.record("confirmation");
        proxy
            .call::<_, _, ()>("RequestConfirmation", &(device, 123456u32))
            .await?;
        self.paired.store(true, Ordering::SeqCst);
        self.record("paired");
        Ok(())
    }
    fn connect(&self) -> zbus::fdo::Result<()> {
        if !self.paired.load(Ordering::SeqCst) || !self.trusted.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed(
                "Device must be paired and trusted".into(),
            ));
        }
        self.connected.store(true, Ordering::SeqCst);
        self.record("connected");
        Ok(())
    }
    fn disconnect(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.record("disconnected");
    }
    fn cancel_pairing(&self) {}
    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.trusted.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn set_trusted(&self, value: bool) {
        self.trusted.store(value, Ordering::SeqCst);
    }

    #[zbus(property)]
    fn alias(&self) -> &str {
        self.name
    }
    #[zbus(property)]
    fn address(&self) -> &str {
        "AA:BB:CC:DD:EE:FF"
    }
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired.load(Ordering::SeqCst)
    }
    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> i16 {
        -52
    }
}
struct Manager {
    agents: Arc<Mutex<std::collections::HashMap<String, String>>>,
}
#[zbus::interface(name = "org.bluez.AgentManager1")]
impl Manager {
    fn register_agent(
        &self,
        path: zbus::zvariant::OwnedObjectPath,
        _capability: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        self.agents
            .lock()
            .unwrap()
            .insert(header.sender().unwrap().to_string(), path.to_string());
    }
}
struct Battery(u8);
#[zbus::interface(name = "org.bluez.Battery1")]
impl Battery {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        self.0
    }
}
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"),
        "Only run from the headless harness"
    );
    // Session address is private; the harness points its fake system bus here too.
    let agents = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let scanning = Arc::new(AtomicBool::new(false));
    let connection: Connection = Builder::session()?
        .name("org.bluez")?
        .serve_at("/", zbus::fdo::ObjectManager)?
        .serve_at(
            "/org/bluez",
            Manager {
                agents: agents.clone(),
            },
        )?
        .serve_at("/org/bluez/hci0/dev_connected", Battery(73))?
        .serve_at(
            "/org/bluez/hci0",
            Adapter {
                scanning: scanning.clone(),
                log: std::env::var("BHARTA_TEST_BLUETOOTH_LOG")?,
            },
        )?
        .serve_at(
            "/org/bluez/hci0/dev_connected",
            Device {
                name: "Studio headphones",
                connected: AtomicBool::new(true),
                paired: AtomicBool::new(true),
                trusted: AtomicBool::new(true),
                agents: agents.clone(),
            },
        )?
        .serve_at(
            "/org/bluez/hci0/dev_saved",
            Device {
                name: "Desk keyboard",
                connected: AtomicBool::new(false),
                paired: AtomicBool::new(true),
                trusted: AtomicBool::new(true),
                agents: agents.clone(),
            },
        )?
        .build()?;
    let mut discovered = false;
    loop {
        if scanning.load(Ordering::SeqCst) && !discovered {
            connection.object_server().at(
                "/org/bluez/hci0/dev_nearby",
                Device {
                    name: "Nearby speaker",
                    connected: AtomicBool::new(false),
                    paired: AtomicBool::new(false),
                    trusted: AtomicBool::new(false),
                    agents: agents.clone(),
                },
            )?;
            discovered = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
