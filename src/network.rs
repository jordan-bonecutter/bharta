use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    process::{Command, Stdio},
};

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

// nmcli escapes literal ':' and '\' in terse output. SSIDs are data, never shell code.
fn fields(line: &str) -> Vec<String> {
    let mut result = vec![String::new()];
    let mut escape = false;
    for c in line.chars() {
        if escape {
            result.last_mut().unwrap().push(c);
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else if c == ':' {
            result.push(String::new());
        } else {
            result.last_mut().unwrap().push(c);
        }
    }
    if escape {
        result.last_mut().unwrap().push('\\');
    }
    result
}
fn run(args: &[&str]) -> Result<String> {
    let output = Command::new("nmcli")
        .env("LC_ALL", "C")
        .args(["--wait", "20"])
        .args(args)
        .output()
        .context("NetworkManager's nmcli is not available")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
fn parse(text: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = text
        .lines()
        .filter_map(|l| {
            let f = fields(l);
            if f.len() != 7 || f[2].is_empty() {
                return None;
            }
            Some(Network {
                backend: Backend::NetworkManager,
                active: f[0] == "*",
                bssid: f[1].clone(),
                ssid: f[2].clone(),
                signal: f[3].parse::<u8>().ok().filter(|s| *s <= 100),
                rssi_dbm: None,
                security: f[4].clone(),
                device: f[6].clone(),
            })
        })
        .collect();
    networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    // Keep the active / strongest access point for each network and adapter.
    let mut seen = std::collections::HashSet::new();
    networks.retain(|n| seen.insert((n.ssid.clone(), n.security.clone(), n.device.clone())));
    networks
}
pub fn scan(rescan: bool) -> Result<Snapshot> {
    if crate::iwd::active()? {
        return crate::iwd::scan(rescan);
    }
    let enabled = run(&["radio", "wifi"])?.trim() == "enabled";
    let text = run(&[
        "--terse",
        "--escape",
        "yes",
        "--fields",
        "IN-USE,BSSID,SSID,SIGNAL,SECURITY,CHAN,DEVICE",
        "device",
        "wifi",
        "list",
        "--rescan",
        if rescan { "yes" } else { "no" },
    ])?;
    Ok(Snapshot {
        backend: Backend::NetworkManager,
        enabled,
        networks: parse(&text),
    })
}
pub fn radio(enabled: bool) -> Result<Snapshot> {
    if crate::iwd::active()? {
        return crate::iwd::radio(enabled);
    }
    run(&["radio", "wifi", if enabled { "on" } else { "off" }])?;
    scan(enabled)
}
pub fn connect(network: &Network, password: String) -> Result<Snapshot> {
    if network.backend == Backend::Iwd {
        return crate::iwd::connect(network, password);
    }
    // Feed secrets via stdin to --ask; never expose a password in argv or logs.
    let mut command = Command::new("nmcli");
    command.env("LC_ALL", "C").args(["--wait", "25"]);
    if !password.is_empty() {
        command.arg("--ask");
    }
    command.args([
        "device",
        "wifi",
        "connect",
        &network.bssid,
        "ifname",
        &network.device,
    ]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start NetworkManager")?;
    if let Some(mut input) = child.stdin.take()
        && !password.is_empty()
    {
        // A closed pipe means nmcli finished without requesting a secret.
        let _ = writeln!(input, "{password}");
    }
    drop(password);
    if !child.wait()?.success() {
        bail!("Connection failed. Check the password or use Advanced settings.");
    }
    scan(false)
}
pub fn disconnect(device: &str) -> Result<Snapshot> {
    if device.starts_with("/net/connman/iwd/") {
        return crate::iwd::disconnect(device);
    }
    run(&["device", "disconnect", device])?;
    scan(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_escaped_ssids_and_prefers_active_access_point() {
        let data = "*:AA\\:BB\\:CC\\:DD\\:EE\\:FF:Cafe\\: guest:65:WPA2:11:wlan0\n:11\\:22\\:33\\:44\\:55\\:66:Cafe\\: guest:99:WPA2:1:wlan0\n:AA:Other\\\\name:40:--:6:wlan0";
        let n = parse(data);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].ssid, "Cafe: guest");
        assert_eq!(n[0].bssid, "AA:BB:CC:DD:EE:FF");
        assert!(n[0].active);
        assert_eq!(n[1].ssid, "Other\\name");
    }
    #[test]
    fn skips_hidden_and_malformed_rows() {
        assert!(parse(":aa::10:--:1:wlan0\nbad").is_empty());
    }
}
