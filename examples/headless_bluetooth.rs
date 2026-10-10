//! Fake BlueZ on the harness's private D-Bus, never the desktop system bus.
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc,
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
    connected: bool,
    paired: bool,
}
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
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
        self.connected
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired
    }
    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> i16 {
        -52
    }
}
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"),
        "Only run from the headless harness"
    );
    // Session address is private; the harness points its fake system bus here too.
    let scanning = Arc::new(AtomicBool::new(false));
    let connection: Connection = Builder::session()?
        .name("org.bluez")?
        .serve_at("/", zbus::fdo::ObjectManager)?
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
                connected: true,
                paired: true,
            },
        )?
        .serve_at(
            "/org/bluez/hci0/dev_saved",
            Device {
                name: "Desk keyboard",
                connected: false,
                paired: true,
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
                    connected: false,
                    paired: false,
                },
            )?;
            discovered = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
