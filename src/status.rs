use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub name: String,
    pub focused: bool,
    pub urgent: bool,
    pub audible: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub extras: crate::media::Extras,
    pub window_pids: std::collections::HashMap<u32, std::collections::HashSet<String>>,
    pub window_details: std::collections::HashMap<u32, Vec<crate::media::WindowInfo>>,
    pub app: String,
    pub workspaces: Vec<Workspace>,
    pub battery: Option<(u8, bool)>,
    pub network: bool,
    pub connected: bool,
}

fn connect() -> Result<UnixStream> {
    let socket = UnixStream::connect(
        std::env::var("SWAYSOCK").context("SWAYSOCK is missing; run inside Sway")?,
    )?;
    socket.set_read_timeout(Some(crate::config::get().duration("timeouts.ipc_ms")))?;
    socket.set_write_timeout(Some(crate::config::get().duration("timeouts.ipc_ms")))?;
    Ok(socket)
}
fn request(socket: &mut UnixStream, kind: u32, payload: &str) -> Result<()> {
    socket.write_all(b"i3-ipc")?;
    socket.write_all(&(payload.len() as u32).to_ne_bytes())?;
    socket.write_all(&kind.to_ne_bytes())?;
    socket.write_all(payload.as_bytes())?;
    Ok(())
}
fn response(socket: &mut impl Read) -> Result<Value> {
    let mut header = [0; 14];
    socket.read_exact(&mut header)?;
    if &header[..6] != b"i3-ipc" {
        bail!("Invalid IPC header");
    }
    let len = u32::from_ne_bytes(header[6..10].try_into()?) as usize;
    if len > 16 * 1024 * 1024 {
        bail!("IPC response too large");
    }
    let mut body = vec![0; len];
    socket.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}
pub fn ipc(kind: u32, payload: &str) -> Result<Value> {
    let mut socket = connect()?;
    request(&mut socket, kind, payload)?;
    response(&mut socket)
}

pub enum Update {
    Refresh,
    Focus(String),
    AnnouncePopup(u128),
    PopupOpened(u128),
}

// A separate blocking subscription wakes the sampler on compositor changes.
// Reconnect on failure; the sampler's timeout still keeps the clock working.
pub fn watch(sender: std::sync::mpsc::Sender<Update>) {
    std::thread::spawn(move || {
        loop {
            let result = (|| -> Result<()> {
                let mut socket = connect()?;
                listen(&mut socket, &sender)
            })();
            if result.is_ok() {
                break;
            }
            if sender.send(Update::Refresh).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn listen(socket: &mut UnixStream, sender: &std::sync::mpsc::Sender<Update>) -> Result<()> {
    request(socket, 2, r#"["workspace","window","tick"]"#)?;
    if response(socket)?["success"] != true {
        bail!("Sway subscription rejected");
    }
    socket.set_read_timeout(None)?;
    sender.send(Update::Refresh)?;
    loop {
        let event = response(socket)?;
        let update = event["payload"]
            .as_str()
            .and_then(|p| p.strip_prefix("bharta-popup:"))
            .and_then(|p| p.parse().ok())
            .map(Update::PopupOpened)
            .unwrap_or(Update::Refresh);
        if sender.send(update).is_err() {
            return Ok(());
        }
    }
}

pub fn focus(name: &str) -> Result<()> {
    // Sway's quoted arguments use backslash escaping. Never invoke a shell.
    let quoted = name.replace('\\', "\\\\").replace('"', "\\\"");
    let result = ipc(0, &format!("workspace \"{quoted}\""))?;
    if !result
        .as_array()
        .is_some_and(|a| !a.is_empty() && a.iter().all(|v| v["success"] == true))
    {
        bail!("Sway rejected workspace switch: {result}");
    }
    Ok(())
}

fn focused_app(v: &Value) -> Option<String> {
    if v["focused"] == true {
        return v["app_id"]
            .as_str()
            .or(v["window_properties"]["class"].as_str())
            .map(str::to_owned);
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|k| v[*k].as_array())
        .flatten()
        .find_map(focused_app)
}

impl Status {
    pub fn read(output: Option<&str>) -> Self {
        let mut s = Self {
            app: "Desktop".into(),
            ..Self::default()
        };
        if let Ok(v) = ipc(1, "") {
            s.connected = true;
            s.workspaces = v
                .as_array()
                .into_iter()
                .flatten()
                .filter(|w| output.is_none_or(|o| w["output"].as_str() == Some(o)))
                .filter_map(|w| {
                    Some(Workspace {
                        name: w["name"].as_str()?.into(),
                        // Each output's displayed workspace stays active when
                        // keyboard focus moves to another monitor or the bar.
                        focused: w["focused"] == true || (output.is_some() && w["visible"] == true),
                        urgent: w["urgent"] == true,
                        audible: false,
                    })
                })
                .collect();
        }
        if let Ok(v) = ipc(4, "") {
            crate::media::window_pids(&v, None, &mut s.window_pids);
            crate::media::window_details(&v, None, &mut s.window_details);
            s.app = focused_app(&v).unwrap_or_else(|| "Desktop".into());
        }
        if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
            for entry in entries.flatten() {
                let p = entry.path();
                if fs::read_to_string(p.join("type")).is_ok_and(|t| t.trim() == "Battery")
                    && let Ok(n) = fs::read_to_string(p.join("capacity"))
                        .unwrap_or_default()
                        .trim()
                        .parse::<u8>()
                {
                    let charging =
                        fs::read_to_string(p.join("status")).is_ok_and(|t| t.trim() == "Charging");
                    s.battery = Some((n.min(100), charging));
                    break;
                }
            }
        }
        s.network = fs::read_dir("/sys/class/net")
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name() != "lo"
                    && fs::read_to_string(e.path().join("operstate"))
                        .is_ok_and(|s| s.trim() == "up")
            });
        s
    }
    pub fn update_audio(&mut self) {
        let audible = crate::media::audible_workspaces(
            &crate::media::source_titles(&self.extras.audio_sources, &self.extras.tracks),
            &self.window_pids,
            &self.window_details,
        );
        for w in &mut self.workspaces {
            w.audible = audible.contains(&w.name);
        }
    }
    pub fn demo() -> Self {
        Self {
            app: "Finder".into(),
            workspaces: (1..=4)
                .map(|n| Workspace {
                    name: n.to_string(),
                    focused: n == 1,
                    urgent: false,
                    audible: n == 2,
                })
                .collect(),
            battery: Some((86, false)),
            network: true,
            connected: true,
            extras: crate::media::Extras {
                wifi_name: Some("Home Wi-Fi".into()),
                wifi_enabled: Some(true),
                wifi_signal: Some(80),
                track: Some(crate::media::Track {
                    art_url: String::new(),
                    player: "demo".into(),
                    title: "Dreams".into(),
                    artist: "Fleetwood Mac".into(),
                    playing: true,
                    can_previous: true,
                    can_next: true,
                    can_toggle: true,
                }),
                ..Default::default()
            },
            window_pids: Default::default(),
            window_details: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_events_wake_sampler_without_poll_delay() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || listen(&mut client, &tx));
        server
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(
            response(&mut server).unwrap(),
            serde_json::json!(["workspace", "window", "tick"])
        );
        request(&mut server, 2, r#"{"success":true}"#).unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(500)),
            Ok(Update::Refresh)
        ));
        request(&mut server, 0x80000000, r#"{"change":"focus"}"#).unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(500)),
            Ok(Update::Refresh)
        ));
        request(
            &mut server,
            0x80000007,
            r#"{"first":false,"payload":"bharta-popup:12345"}"#,
        )
        .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(500)),
            Ok(Update::PopupOpened(12345))
        ));
        drop(server);
        assert!(thread.join().unwrap().is_err());
    }
    #[test]
    fn malformed_and_oversized_ipc_frames_are_rejected() {
        assert!(response(&mut &b"invalid-header"[..]).is_err());
        let mut frame = Vec::from(&b"i3-ipc"[..]);
        frame.extend_from_slice(&(17_u32 * 1024 * 1024).to_ne_bytes());
        frame.extend_from_slice(&0_u32.to_ne_bytes());
        assert!(response(&mut frame.as_slice()).is_err());
    }
    #[test]
    fn finds_floating_xwayland_app() {
        let v = serde_json::json!({"nodes": [{"floating_nodes": [{"focused": true, "window_properties": {"class": "Terminal"}}]}]});
        assert_eq!(focused_app(&v).as_deref(), Some("Terminal"));
    }
    #[test]
    fn empty_workspace_has_no_app() {
        assert_eq!(
            focused_app(&serde_json::json!({"focused": true, "type": "workspace"})),
            None
        );
    }
}
