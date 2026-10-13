use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Backend {
    #[default]
    NetworkManager,
    Iwd,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub backend: Backend,
    pub ssid: String,
    pub bssid: String,
    pub device: String,
    pub signal: Option<u8>,
    pub rssi_dbm: Option<i16>,
    pub security: String,
    pub active: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub backend: Backend,
    pub enabled: bool,
    pub networks: Vec<Network>,
}
fn proxy<'a>(c: &'a Connection, path: &'a str, interface: &'a str) -> Result<Proxy<'a>> {
    Ok(Proxy::new(c, SERVICE, path, interface)?)
}
fn manager(c: &Connection) -> Result<Proxy<'_>> {
    proxy(c, ROOT, SERVICE)
}
fn devices(c: &Connection) -> Result<Vec<OwnedObjectPath>> {
    let devices: Vec<OwnedObjectPath> = manager(c)?.call("GetDevices", &())?;
    Ok(devices
        .into_iter()
        .filter(|path| {
            proxy(c, path.as_str(), "org.freedesktop.NetworkManager.Device")
                .and_then(|p| Ok(p.get_property::<u32>("DeviceType")?))
                .is_ok_and(|kind| kind == 2)
        })
        .collect())
}
fn security(flags: u32, wpa: u32, rsn: u32) -> &'static str {
    if (wpa | rsn) & 0x200 != 0 {
        "802.1X"
    } else if rsn & 0x400 != 0 {
        "WPA3"
    } else if rsn & 0x800 != 0 {
        "--"
    }
    // Enhanced open (OWE).
    else if rsn != 0 {
        "WPA2"
    } else if wpa != 0 {
        "WPA"
    } else if flags & 1 != 0 {
        "WEP"
    } else {
        "--"
    }
}
fn normalize(networks: &mut Vec<Network>) {
    networks.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    let mut seen = HashSet::new();
    networks.retain(|n| seen.insert((n.ssid.clone(), n.security.clone(), n.device.clone())));
}
pub fn scan(rescan: bool) -> Result<Snapshot> {
    if crate::iwd::active()? {
        return crate::iwd::scan(rescan);
    }
    let c = Connection::system()?;
    let enabled = manager(&c)?.get_property::<bool>("WirelessEnabled")?
        && manager(&c)?.get_property::<bool>("WirelessHardwareEnabled")?;
    let mut networks = vec![];
    if enabled {
        for device in devices(&c)? {
            let wifi = proxy(
                &c,
                device.as_str(),
                "org.freedesktop.NetworkManager.Device.Wireless",
            )?;
            if rescan {
                let previous: i64 = wifi.get_property("LastScan")?;
                let options: HashMap<String, OwnedValue> = HashMap::new();
                // A scan can already be running or throttled. Keep the cached list.
                if wifi.call::<_, _, ()>("RequestScan", &(options,)).is_ok() {
                    let deadline =
                        Instant::now() + crate::config::get().duration("timeouts.wifi_ms");
                    while wifi.get_property::<i64>("LastScan")? == previous
                        && Instant::now() < deadline
                    {
                        std::thread::sleep(crate::config::get().duration("timeouts.wifi_poll_ms"));
                    }
                }
            }
            let active: OwnedObjectPath = wifi.get_property("ActiveAccessPoint")?;
            let aps: Vec<OwnedObjectPath> = wifi.call("GetAllAccessPoints", &())?;
            for path in aps {
                // Access points can disappear during a scan.
                let result = (|| -> Result<Network> {
                    let ap = proxy(
                        &c,
                        path.as_str(),
                        "org.freedesktop.NetworkManager.AccessPoint",
                    )?;
                    let ssid: Vec<u8> = ap.get_property("Ssid")?;
                    Ok(Network {
                        backend: Backend::NetworkManager,
                        ssid: String::from_utf8_lossy(&ssid).into_owned(),
                        bssid: path.to_string(),
                        device: device.to_string(),
                        signal: Some(ap.get_property::<u8>("Strength")?.min(100)),
                        rssi_dbm: None,
                        security: security(
                            ap.get_property("Flags")?,
                            ap.get_property("WpaFlags")?,
                            ap.get_property("RsnFlags")?,
                        )
                        .into(),
                        active: path == active,
                    })
                })();
                if let Ok(network) = result
                    && !network.ssid.is_empty()
                {
                    networks.push(network);
                }
            }
        }
    }
    normalize(&mut networks);
    Ok(Snapshot {
        backend: Backend::NetworkManager,
        enabled,
        networks,
    })
}
pub fn radio(enabled: bool) -> Result<Snapshot> {
    if crate::iwd::active()? {
        return crate::iwd::radio(enabled);
    }
    manager(&Connection::system()?)?.set_property("WirelessEnabled", enabled)?;
    scan(enabled)
}
pub fn connect(network: &Network, password: String) -> Result<Snapshot> {
    if network.backend == Backend::Iwd {
        return crate::iwd::connect(network, password);
    }
    connect_nm(network, false, password)
}
pub fn connect_hidden(ssid: &str, password: String) -> Result<Snapshot> {
    validate_ssid(ssid)?;
    if crate::iwd::active()? {
        return crate::iwd::connect_hidden(ssid, password);
    }
    let c = Connection::system()?;
    let device = devices(&c)?
        .into_iter()
        .next()
        .context("No Wi-Fi adapter available")?;
    connect_nm(
        &Network {
            backend: Backend::NetworkManager,
            ssid: ssid.into(),
            device: device.to_string(),
            bssid: "/".into(),
            signal: None,
            rssi_dbm: None,
            security: if password.is_empty() { "--" } else { "WPA2" }.into(),
            active: false,
        },
        true,
        password,
    )
}
fn validate_ssid(ssid: &str) -> Result<()> {
    ensure!(
        !ssid.is_empty() && ssid.len() <= 32 && !ssid.contains('\0'),
        "Network name must contain 1–32 bytes and no NUL characters."
    );
    Ok(())
}
type Settings = HashMap<String, HashMap<String, OwnedValue>>;
fn owned(value: impl Into<Value<'static>>) -> Result<OwnedValue> {
    Ok(value.into().try_to_owned()?)
}
fn connect_nm(network: &Network, hidden: bool, password: String) -> Result<Snapshot> {
    let c = Connection::system()?;
    let device = OwnedObjectPath::try_from(network.device.as_str())?;
    let ap = OwnedObjectPath::try_from(network.bssid.as_str())?;
    if password.is_empty() {
        let settings = proxy(
            &c,
            "/org/freedesktop/NetworkManager/Settings",
            "org.freedesktop.NetworkManager.Settings",
        )?;
        let profiles: Vec<OwnedObjectPath> = settings.call("ListConnections", &())?;
        for path in profiles {
            let saved: Settings = proxy(
                &c,
                path.as_str(),
                "org.freedesktop.NetworkManager.Settings.Connection",
            )?
            .call("GetSettings", &())?;
            let matches = saved
                .get("802-11-wireless")
                .and_then(|s| s.get("ssid"))
                .and_then(|v| <Vec<u8>>::try_from(v.try_clone().ok()?).ok())
                .is_some_and(|ssid| ssid == network.ssid.as_bytes());
            if matches {
                let active: OwnedObjectPath =
                    manager(&c)?.call("ActivateConnection", &(path, &device, &ap))?;
                return wait_connected(&c, &active);
            }
        }
    }
    ensure!(
        network.security != "802.1X",
        "Set up this enterprise network in your desktop's network settings first"
    );
    ensure!(
        network.security != "WEP",
        "Set up this legacy WEP network in your desktop's network settings first"
    );
    let mut settings = Settings::new();
    settings.insert(
        "connection".into(),
        HashMap::from([
            ("id".into(), owned(network.ssid.clone())?),
            ("type".into(), owned("802-11-wireless")?),
        ]),
    );
    let mut wireless = HashMap::from([
        ("ssid".into(), owned(network.ssid.as_bytes().to_vec())?),
        ("mode".into(), owned("infrastructure")?),
        ("hidden".into(), owned(hidden)?),
    ]);
    if !password.is_empty() {
        let key_mgmt = if network.security == "WPA3" {
            "sae"
        } else {
            "wpa-psk"
        };
        wireless.insert("security".into(), owned("802-11-wireless-security")?);
        settings.insert(
            "802-11-wireless-security".into(),
            HashMap::from([
                ("key-mgmt".into(), owned(key_mgmt)?),
                ("psk".into(), owned(password)?),
            ]),
        );
    }
    settings.insert("802-11-wireless".into(), wireless);
    let (_, active): (OwnedObjectPath, OwnedObjectPath) =
        manager(&c)?.call("AddAndActivateConnection", &(settings, device, ap))?;
    wait_connected(&c, &active)
}
fn wait_connected(c: &Connection, active: &OwnedObjectPath) -> Result<Snapshot> {
    let connection = proxy(
        c,
        active.as_str(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )?;
    let deadline = Instant::now() + crate::config::get().duration("timeouts.wifi_ms");
    loop {
        match connection.get_property::<u32>("State")? {
            2 => return scan(false),
            3 | 4 => bail!("Connection failed. Check the password or network settings."),
            _ if Instant::now() >= deadline => bail!("Wi-Fi connection timed out"),
            _ => std::thread::sleep(crate::config::get().duration("timeouts.wifi_poll_ms")),
        }
    }
}
pub fn disconnect(device: &str) -> Result<Snapshot> {
    if device.starts_with("/net/connman/iwd/") {
        return crate::iwd::disconnect(device);
    }
    let c = Connection::system()?;
    let _: () =
        proxy(&c, device, "org.freedesktop.NetworkManager.Device")?.call("Disconnect", &())?;
    scan(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn security_flags_distinguish_open_personal_and_enterprise() {
        assert_eq!(security(0, 0, 0), "--");
        assert_eq!(security(1, 0, 0), "WEP");
        assert_eq!(security(1, 0x100, 0), "WPA");
        assert_eq!(security(1, 0, 0x100), "WPA2");
        assert_eq!(security(1, 0, 0x400), "WPA3");
        assert_eq!(security(1, 0, 0x200), "802.1X");
    }
    #[test]
    fn hidden_ssid_validation_preserves_spaces_and_counts_bytes() {
        assert!(validate_ssid("  Cafe: guest\\name  ").is_ok());
        assert!(validate_ssid(&"é".repeat(16)).is_ok());
        for ssid in [String::new(), "é".repeat(17), "bad\0name".into()] {
            assert!(validate_ssid(&ssid).is_err());
        }
    }
}
