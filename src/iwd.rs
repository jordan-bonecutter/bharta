use crate::network::{Backend, Network, Snapshot};
use anyhow::{Context, Result, bail};
use zbus::{
    blocking::{Connection, Proxy, fdo::ObjectManagerProxy},
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},
};
const SERVICE: &str = "net.connman.iwd";
fn proxy<'a>(
    connection: &'a Connection,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Proxy::new(connection, SERVICE, path, interface)
}
pub fn active() -> Result<bool> {
    let c = Connection::system()?;
    let bus = Proxy::new(
        &c,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    // Prefer NM if it owns the devices (including its optional iwd backend).
    let nm: bool = bus.call("NameHasOwner", &("org.freedesktop.NetworkManager",))?;
    let iwd: bool = bus.call("NameHasOwner", &(SERVICE,))?;
    Ok(iwd && !nm)
}
pub fn scan(rescan: bool) -> Result<Snapshot> {
    let c = Connection::system()?;
    let objects=ObjectManagerProxy::builder(&c).destination(SERVICE)?.path("/")?.build()?.get_managed_objects()
        .context("Cannot read iwd. Your user needs permission to access net.connman.iwd on the system bus")?;
    let mut snapshot = Snapshot {
        backend: Backend::Iwd,
        ..Snapshot::default()
    };
    for (path, interfaces) in objects {
        if !interfaces.contains_key("net.connman.iwd.Station") {
            continue;
        }
        let station = proxy(&c, path.as_str(), "net.connman.iwd.Station")?;
        let device = proxy(&c, path.as_str(), "net.connman.iwd.Device")?;
        let enabled: bool = device.get_property("Powered")?;
        snapshot.enabled |= enabled;
        if !enabled {
            continue;
        }
        if rescan {
            let _: () = station.call("Scan", &())?;
            let deadline =
                std::time::Instant::now() + crate::config::get().duration("timeouts.wifi_ms");
            while station.get_property::<bool>("Scanning")? && std::time::Instant::now() < deadline
            {
                std::thread::sleep(crate::config::get().duration("timeouts.wifi_poll_ms"));
            }
        }
        let state: String = station.get_property("State")?;
        let current = station
            .get_property::<OwnedObjectPath>("ConnectedNetwork")
            .ok();
        let ordered: Vec<(OwnedObjectPath, i16)> = station.call("GetOrderedNetworks", &())?;
        let mut networks: Vec<_> = ordered
            .into_iter()
            .map(|(path, rssi)| (path, Some(rssi)))
            .collect();
        // Scan RSSI is cached; diagnostics supplies the current connected BSS in dBm.
        let live_rssi = proxy(&c, path.as_str(), "net.connman.iwd.StationDiagnostic")
            .ok()
            .and_then(|p| {
                p.call::<_, _, std::collections::HashMap<String, OwnedValue>>("GetDiagnostics", &())
                    .ok()
            })
            .and_then(|d| diagnostic_rssi(&d));
        if let Some(current) = &current
            && !networks.iter().any(|(p, _)| p == current)
        {
            networks.insert(0, (current.clone(), None));
        }
        for (network_path, strength) in networks {
            let n = proxy(&c, network_path.as_str(), "net.connman.iwd.Network")?;
            let security: String = n.get_property("Type")?;
            snapshot.networks.push(Network {
                backend: Backend::Iwd,
                ssid: n.get_property("Name")?,
                bssid: network_path.to_string(),
                device: path.to_string(),
                signal: if current.as_ref() == Some(&network_path) {
                    live_rssi
                        .map(|rssi| signal_percent(rssi * 100))
                        .or_else(|| strength.map(signal_percent))
                } else {
                    strength.map(signal_percent)
                },
                rssi_dbm: if current.as_ref() == Some(&network_path) {
                    live_rssi.or_else(|| strength.map(|s| s / 100))
                } else {
                    strength.map(|s| s / 100)
                },
                security: match security.as_str() {
                    "open" => "--",
                    "psk" => "WPA",
                    "8021x" => "802.1X",
                    other => other,
                }
                .into(),
                active: (state == "connected" || state == "roaming")
                    && current.as_ref() == Some(&network_path),
            });
        }
    }
    snapshot
        .networks
        .sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    Ok(snapshot)
}
fn signal_percent(rssi: i16) -> u8 {
    // Match NetworkManager's RSSI quality scale: -100 dBm = 0%, -40 dBm = 100%.
    // Keep iwd's hundredths of a dBm until the final conversion.
    (100 - ((-4000 - i32::from(rssi).clamp(-10000, -4000)) * 100 / 6000)) as u8
}
fn diagnostic_rssi(values: &std::collections::HashMap<String, OwnedValue>) -> Option<i16> {
    ["AverageRSSI", "RSSI"].iter().find_map(|key| {
        values
            .get(*key)
            .and_then(|v| i16::try_from(v).ok())
            .filter(|rssi| (-120..0).contains(rssi))
    })
}
pub fn radio(enabled: bool) -> Result<Snapshot> {
    let c = Connection::system()?;
    let objects = ObjectManagerProxy::builder(&c)
        .destination(SERVICE)?
        .path("/")?
        .build()?
        .get_managed_objects()?;
    for (path, interfaces) in objects {
        if interfaces.contains_key("net.connman.iwd.Device") {
            proxy(&c, path.as_str(), "net.connman.iwd.Device")?.set_property("Powered", enabled)?;
        }
    }
    scan(false)
}
pub fn disconnect(device: &str) -> Result<Snapshot> {
    let c = Connection::system()?;
    let _: () = proxy(&c, device, "net.connman.iwd.Station")?.call("Disconnect", &())?;
    scan(false)
}
struct Agent {
    owner: String,
    network: String,
    secret: std::sync::Mutex<Option<String>>,
}
#[zbus::interface(name = "net.connman.iwd.Agent")]
impl Agent {
    fn request_passphrase(
        &self,
        network: ObjectPath<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        if header.sender().map(|s| s.as_str()) != Some(self.owner.as_str())
            || network.as_str() != self.network
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "Unexpected credential request".into(),
            ));
        }
        self.secret
            .lock()
            .unwrap()
            .take()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| zbus::fdo::Error::Failed("No password provided".into()))
    }
    fn release(&self) {
        self.secret.lock().unwrap().take();
    }
    fn cancel(&self, _reason: &str) {
        self.secret.lock().unwrap().take();
    }
}
pub fn connect(network: &Network, password: String) -> Result<Snapshot> {
    let c = Connection::system()?;
    let bus = Proxy::new(
        &c,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: String = bus.call("GetNameOwner", &(SERVICE,))?;
    let path = ObjectPath::try_from("/org/bharta/WifiAgent")?;
    c.object_server().at(
        path.clone(),
        Agent {
            owner,
            network: network.bssid.clone(),
            secret: std::sync::Mutex::new(Some(password)),
        },
    )?;
    let manager = proxy(&c, "/net/connman/iwd", "net.connman.iwd.AgentManager")?;
    let _: () = manager.call("RegisterAgent", &(path.clone(),))?;
    let result: Result<(), zbus::Error> =
        proxy(&c, &network.bssid, "net.connman.iwd.Network")?.call("Connect", &());
    let _: Result<(), _> = manager.call("UnregisterAgent", &(path,));
    if let Err(e) = result {
        bail!(
            "iwd connection failed: {e}. Check the password or provision enterprise credentials with iwd."
        );
    }
    scan(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connected_diagnostics_uses_dbm_and_prefers_average() {
        let values = std::collections::HashMap::from([
            ("AverageRSSI".into(), OwnedValue::from(-66i16)),
            ("RSSI".into(), OwnedValue::from(-49i16)),
        ]);
        assert_eq!(diagnostic_rssi(&values), Some(-66));
        assert_eq!(signal_percent(diagnostic_rssi(&values).unwrap() * 100), 57);
        assert_eq!(diagnostic_rssi(&std::collections::HashMap::new()), None);
    }
    #[test]
    fn signal_is_clamped() {
        assert_eq!(signal_percent(-10000), 0);
        assert_eq!(signal_percent(-7500), 42);
        assert_eq!(signal_percent(-6600), 57);
        assert_eq!(signal_percent(-4900), 85);
        assert_eq!(signal_percent(-4000), 100);
    }
}
