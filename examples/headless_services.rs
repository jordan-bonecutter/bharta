//! NetworkManager and login1 fixtures on the harness's private bus.
use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::HashMap, fs::OpenOptions, io::Write};
use zbus::{
    blocking::connection::Builder,
    zvariant::{OwnedObjectPath, OwnedValue},
};
const ROOT: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/0";
const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/0";
fn state() -> Value {
    serde_json::from_slice(
        &std::fs::read(std::env::var("BHARTA_TEST_WIFI_STATE").unwrap()).unwrap(),
    )
    .unwrap()
}
fn save(state: &Value) {
    std::fs::write(
        std::env::var("BHARTA_TEST_WIFI_STATE").unwrap(),
        state.to_string(),
    )
    .unwrap();
}
fn record(value: Value) {
    let mut f = OpenOptions::new()
        .append(true)
        .open(std::env::var("BHARTA_TEST_WIFI_LOG").unwrap())
        .unwrap();
    writeln!(f, "{value}").unwrap();
}
fn path(index: usize) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("{ROOT}/AccessPoint/{index}")).unwrap()
}
fn ssid(index: usize) -> String {
    match index {
        0 => "Home".into(),
        1 => "Cafe".into(),
        2 => "Guest".into(),
        3 => "1234".into(),
        i => format!("Extra network {:02}", i - 4),
    }
}
struct Manager;
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(DEVICE).unwrap()]
    }
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        state()["enabled"].as_bool().unwrap()
    }
    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn set_wireless_enabled(&self, enabled: bool) {
        let mut s = state();
        s["enabled"] = json!(enabled);
        save(&s);
    }
    fn add_and_activate_connection(
        &self,
        settings: HashMap<String, HashMap<String, OwnedValue>>,
        _device: OwnedObjectPath,
        ap: OwnedObjectPath,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let wireless = &settings["802-11-wireless"];
        let name =
            String::from_utf8(Vec::<u8>::try_from(wireless["ssid"].try_clone().unwrap()).unwrap())
                .unwrap();
        let hidden = wireless
            .get("hidden")
            .is_some_and(|v| bool::try_from(v).unwrap());
        let password = settings
            .get("802-11-wireless-security")
            .and_then(|s| s.get("psk"))
            .and_then(|v| <&str>::try_from(v).ok());
        record(
            json!({"action":"connect", "ssid":name, "hidden":hidden, "secret_supplied":password.is_some(), "ap":ap.as_str()}),
        );
        if (hidden || name == "Guest") && password != Some("1234") {
            return Err(zbus::fdo::Error::Failed(
                "Incorrect fixture password".into(),
            ));
        }
        let mut s = state();
        s["active"] = json!(name);
        save(&s);
        Ok((
            OwnedObjectPath::try_from(format!("{ROOT}/Settings/0")).unwrap(),
            OwnedObjectPath::try_from(ACTIVE).unwrap(),
        ))
    }
}
struct Device;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        2
    }
    fn disconnect(&self) {
        let mut s = state();
        s["active"] = json!("");
        save(&s);
    }
}
struct Wireless(std::sync::atomic::AtomicI64);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        let s = state();
        let mut paths = vec![path(0), path(1), path(2)];
        if s["active"] == "1234" {
            paths.push(path(3));
        }
        if s["many"] == true {
            paths.extend((4..24).map(path));
        }
        paths
    }
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        (0..24)
            .find(|i| ssid(*i) == state()["active"].as_str().unwrap_or(""))
            .map(path)
            .unwrap_or_else(|| OwnedObjectPath::try_from("/").unwrap())
    }
    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        record(json!({"action":"scan"}));
    }
}
struct Ap(usize);
#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl Ap {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        ssid(self.0).into_bytes()
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        match self.0 {
            0 => 85,
            1 => 48,
            2 => 24,
            3 => 70,
            _ => 20,
        }
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        u32::from(self.0 != 1)
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        if self.0 == 1 { 0 } else { 0x100 }
    }
}
struct Settings;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl Settings {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        vec![]
    }
}
struct Active;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }
}
struct Login;
impl Login {
    fn record(&self, action: &str) -> zbus::fdo::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var("BHARTA_TEST_SESSION_ACTIONS").unwrap())
            .unwrap();
        writeln!(f, "{action}").unwrap();
        Err(zbus::fdo::Error::Failed(
            "Session action test failure".into(),
        ))
    }
}
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Login {
    fn reboot(&self, _interactive: bool) -> zbus::fdo::Result<()> {
        self.record("reboot")
    }
    fn power_off(&self, _interactive: bool) -> zbus::fdo::Result<()> {
        self.record("poweroff")
    }
}
fn main() -> Result<()> {
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"),
        "Only run in the private harness"
    );
    let mut builder = Builder::system()?
        .name("org.freedesktop.NetworkManager")?
        .name("org.freedesktop.login1")?
        .serve_at(ROOT, Manager)?
        .serve_at(DEVICE, Device)?
        .serve_at(DEVICE, Wireless(std::sync::atomic::AtomicI64::new(0)))?
        .serve_at(format!("{ROOT}/Settings"), Settings)?
        .serve_at(ACTIVE, Active)?
        .serve_at("/org/freedesktop/login1", Login)?;
    for i in 0..24 {
        builder = builder.serve_at(path(i), Ap(i))?;
    }
    let _connection = builder.build()?;
    loop {
        std::thread::park();
    }
}
