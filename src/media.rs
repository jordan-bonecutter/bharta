use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::OwnedValue,
};
#[derive(Clone, Debug)]
pub struct Track {
    pub art_url: String,
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_toggle: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub artwork: Option<std::sync::Arc<crate::artwork::Artwork>>,
    pub wifi_name: Option<String>,
    pub wifi_signal: Option<u8>,
    pub wifi_enabled: Option<bool>,
    pub track: Option<Track>,
    pub audio_pids: Vec<u32>,
}
pub enum Update {
    Artwork(std::sync::Arc<crate::artwork::Artwork>),
    Wifi(Result<crate::network::Snapshot, String>),
    Playback(Option<Track>),
    Audio(Vec<u32>),
    ControlFinished(u64, Result<(), String>),
}
pub enum Request {
    Refresh,
    Control {
        panel_id: u64,
        player: String,
        control: Control,
    },
}
type UiSender = smithay_client_toolkit::reexports::calloop::channel::Sender<Update>;
pub fn watch(sender: UiSender) -> std::sync::mpsc::Sender<Request> {
    let wifi = sender.clone();
    std::thread::spawn(move || {
        loop {
            if wifi
                .send(Update::Wifi(
                    crate::network::scan(false).map_err(|e| e.to_string()),
                ))
                .is_err()
            {
                break;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
    let audio = sender.clone();
    std::thread::spawn(move || {
        loop {
            if audio.send(Update::Audio(audio_pids())).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    spawn_playback(sender, None)
}
fn playback_connection(address: Option<&str>) -> zbus::Result<Connection> {
    match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?.build(),
        None => Connection::session(),
    }
}
fn spawn_playback(sender: UiSender, address: Option<String>) -> std::sync::mpsc::Sender<Request> {
    let artwork = crate::artwork::worker(sender.clone());
    let (requests, receiver) = std::sync::mpsc::channel();
    let events = requests.clone();
    let event_address = address.clone();
    std::thread::spawn(move || {
        loop {
            let result = (|| -> anyhow::Result<()> {
                let connection = playback_connection(event_address.as_deref())?;
                listen_player_changes(&connection, &events)
            })();
            if result.is_ok() || events.send(Request::Refresh).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    // A single producer owns playback snapshots and button commands. An old
    // polling result can no longer overwrite a newer command/signal update.
    std::thread::spawn(move || {
        loop {
            let request = match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(request) => request,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Request::Refresh,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let connection = playback_connection(address.as_deref());
            let finished = if let Request::Control {
                panel_id,
                player,
                control,
            } = request
            {
                let result = match &connection {
                    Ok(c) => control_on(c, &player, control).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                Some((panel_id, result))
            } else {
                None
            };
            let track = connection.ok().and_then(|c| track_on(&c));
            let art_url = track
                .as_ref()
                .map(|t| t.art_url.clone())
                .unwrap_or_default();
            if sender.send(Update::Playback(track)).is_err() {
                break;
            }
            let _ = artwork.send(art_url);
            if let Some((id, result)) = finished
                && sender.send(Update::ControlFinished(id, result)).is_err()
            {
                break;
            }
        }
    });
    requests
}
fn listen_player_changes(
    connection: &Connection,
    requests: &std::sync::mpsc::Sender<Request>,
) -> anyhow::Result<()> {
    let messages = zbus::blocking::MessageIterator::from(connection);
    let bus = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let _:()=bus.call("AddMatch",&("type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2',arg0='org.mpris.MediaPlayer2.Player'",))?;
    let _:()=bus.call("AddMatch",&("type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'",))?;
    // Subscribe before the first read so startup cannot miss a state transition.
    requests.send(Request::Refresh)?;
    for message in messages {
        if player_event(&message?) && requests.send(Request::Refresh).is_err() {
            return Ok(());
        }
    }
    anyhow::bail!("Playback bus disconnected")
}
fn player_event(message: &zbus::Message) -> bool {
    let header = message.header();
    if header.message_type() != zbus::message::Type::Signal {
        return false;
    }
    match (
        header.interface().map(|i| i.as_str()),
        header.member().map(|m| m.as_str()),
    ) {
        (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
            if header.path().map(|p| p.as_str()) != Some("/org/mpris/MediaPlayer2") {
                return false;
            }
            let Ok((interface, changed, invalidated)) =
                message
                    .body()
                    .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
            else {
                return false;
            };
            interface == "org.mpris.MediaPlayer2.Player"
                && [
                    "PlaybackStatus",
                    "Metadata",
                    "CanGoPrevious",
                    "CanGoNext",
                    "CanPlay",
                    "CanPause",
                    "CanControl",
                ]
                .iter()
                .any(|p| changed.contains_key(*p) || invalidated.iter().any(|i| i == p))
        }
        (Some("org.freedesktop.DBus"), Some("NameOwnerChanged")) => message
            .body()
            .deserialize::<(String, String, String)>()
            .is_ok_and(|(name, _, _)| name.starts_with("org.mpris.MediaPlayer2.")),
        _ => false,
    }
}
fn metadata_string(meta: &HashMap<String, OwnedValue>, key: &str) -> String {
    meta.get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .into()
}
pub fn track() -> Option<Track> {
    track_on(&Connection::session().ok()?)
}
fn track_on(c: &Connection) -> Option<Track> {
    let bus = Proxy::new(
        c,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .ok()?;
    let mut names: Vec<String> = bus.call("ListNames", &()).ok()?;
    names.sort();
    let mut tracks = vec![];
    for name in names.into_iter().filter(|n| {
        n.starts_with("org.mpris.MediaPlayer2.") && n != "org.mpris.MediaPlayer2.playerctld"
    }) {
        let Ok(player) = Proxy::new(
            c,
            name.as_str(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        ) else {
            continue;
        };
        let Ok(state) = player.get_property::<String>("PlaybackStatus") else {
            continue;
        };
        if state == "Stopped" {
            continue;
        }
        let Ok(meta) = player.get_property::<HashMap<String, OwnedValue>>("Metadata") else {
            continue;
        };
        let title = metadata_string(&meta, "xesam:title");
        if title.is_empty() {
            continue;
        }
        let artist = meta
            .get("xesam:artist")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<String>::try_from(v).ok())
            .unwrap_or_default()
            .join(", ");
        let can_previous = player
            .get_property::<bool>("CanGoPrevious")
            .unwrap_or(false);
        let can_next = player.get_property::<bool>("CanGoNext").unwrap_or(false);
        let can_toggle = player
            .get_property::<bool>(if state == "Playing" {
                "CanPause"
            } else {
                "CanPlay"
            })
            .unwrap_or(false);
        drop(player);
        tracks.push(Track {
            art_url: metadata_string(&meta, "mpris:artUrl"),
            can_previous,
            can_next,
            can_toggle,
            player: name,
            title,
            artist,
            playing: state == "Playing",
        });
    }
    tracks.sort_by_key(|t| !t.playing);
    tracks.into_iter().next()
}
#[derive(Clone, Copy)]
pub enum Control {
    Previous,
    Toggle,
    Next,
}
fn control_on(c: &Connection, player: &str, control: Control) -> anyhow::Result<()> {
    let proxy = Proxy::new(
        c,
        player,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )?;
    let _: () = proxy.call(
        match control {
            Control::Previous => "Previous",
            Control::Toggle => "PlayPause",
            Control::Next => "Next",
        },
        &(),
    )?;
    Ok(())
}
fn parse_audio(value: &Value) -> Vec<u32> {
    let mut pids: Vec<u32> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| {
            v["corked"] == false
                && v["mute"] == false
                && v["volume"].as_object().is_none_or(|channels| {
                    channels
                        .values()
                        .any(|v| v["value"].as_u64().unwrap_or(0) > 0)
                })
        })
        .filter_map(|v| {
            v["properties"]["application.process.id"]
                .as_str()
                .and_then(|p| p.parse().ok())
        })
        .collect();
    pids.sort();
    pids.dedup();
    pids
}
pub fn audio_pids() -> Vec<u32> {
    let Ok(mut child) = Command::new("pactl")
        .args(["--format=json", "list", "sink-inputs"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return vec![];
    };
    let Some(stdout) = child.stdout.take() else {
        return vec![];
    };
    let reader = std::thread::spawn(move || {
        let mut data = vec![];
        let _ = stdout.take(1024 * 1024).read_to_end(&mut data);
        data
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let data = reader.join().unwrap_or_default();
                return if status.success() {
                    serde_json::from_slice(&data)
                        .map(|v| parse_audio(&v))
                        .unwrap_or_default()
                } else {
                    vec![]
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return vec![];
            }
        }
    }
}
pub fn window_pids(v: &Value, workspace: Option<&str>, map: &mut HashMap<u32, HashSet<String>>) {
    let workspace = if v["type"] == "workspace" {
        v["name"].as_str()
    } else {
        workspace
    };
    if let (Some(pid), Some(ws)) = (v["pid"].as_u64(), workspace) {
        map.entry(pid as u32).or_default().insert(ws.into());
    }
    for child in ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|k| v[*k].as_array())
        .flatten()
    {
        window_pids(child, workspace, map);
    }
}
pub fn audible_workspaces(pids: &[u32], map: &HashMap<u32, HashSet<String>>) -> HashSet<String> {
    resolve_workspaces(pids, map, |pid| {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("PPid:").and_then(|p| p.trim().parse().ok()))
    })
}
fn resolve_workspaces(
    pids: &[u32],
    map: &HashMap<u32, HashSet<String>>,
    parent: impl Fn(u32) -> Option<u32>,
) -> HashSet<String> {
    let mut audible = HashSet::new();
    for &pid in pids {
        let mut pid = pid;
        for _ in 0..32 {
            if let Some(workspaces) = map.get(&pid) {
                audible.extend(workspaces.iter().cloned());
                break;
            }
            match parent(pid) {
                Some(p) if p > 1 && p != pid => pid = p,
                _ => break,
            }
        }
    }
    audible
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn muted_and_paused_streams_do_not_light_workspaces() {
        let mut v = serde_json::json!([{"corked":false,"mute":false,"volume":{"left":{"value":65536}},"properties":{"application.process.id":"123"}}]);
        assert_eq!(parse_audio(&v), vec![123]);
        v[0]["corked"] = true.into();
        assert!(parse_audio(&v).is_empty());
        v[0]["corked"] = false.into();
        v[0]["mute"] = true.into();
        assert!(parse_audio(&v).is_empty());
        v[0]["mute"] = false.into();
        v[0]["volume"]["left"]["value"] = 0.into();
        assert!(parse_audio(&v).is_empty());
    }
    #[test]
    fn child_audio_process_maps_to_floating_window_workspace() {
        let tree = serde_json::json!({"nodes":[{"type":"workspace","name":"4","floating_nodes":[{"pid":42}]}]});
        let mut map = HashMap::new();
        window_pids(&tree, None, &mut map);
        assert_eq!(
            resolve_workspaces(&[43], &map, |p| if p == 43 { Some(42) } else { None }),
            HashSet::from(["4".into()])
        );
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct TestBus(std::process::Child);
    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Player(Arc<AtomicBool>);
    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl Player {
        #[zbus(property)]
        fn playback_status(&self) -> &str {
            if self.0.load(Ordering::SeqCst) {
                "Playing"
            } else {
                "Paused"
            }
        }
        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            HashMap::from([(
                "xesam:title".into(),
                zbus::zvariant::Str::from("Test song").into(),
            )])
        }
        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_play(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_pause(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn can_control(&self) -> bool {
            true
        }
    }
    #[test]
    fn player_signals_refresh_before_poll_deadline() {
        let mut bus = TestBus(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim();
        let playing = Arc::new(AtomicBool::new(true));
        let connection = zbus::blocking::connection::Builder::address(address)
            .unwrap()
            .name("org.mpris.MediaPlayer2.bharta_test")
            .unwrap()
            .serve_at("/org/mpris/MediaPlayer2", Player(playing.clone()))
            .unwrap()
            .build()
            .unwrap();
        let (tx, rx) = smithay_client_toolkit::reexports::calloop::channel::channel();
        let _requests = spawn_playback(tx, Some(address.into()));
        let wait_for = |expected: Option<bool>| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(800) {
                if let Ok(Update::Playback(track)) = rx.try_recv()
                    && track.map(|t| t.playing) == expected
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("player event did not arrive within 800ms");
        };
        wait_for(Some(true));
        for state in [false, true] {
            playing.store(state, Ordering::SeqCst);
            connection
                .emit_signal(
                    None::<&str>,
                    "/org/mpris/MediaPlayer2",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(
                        "org.mpris.MediaPlayer2.Player",
                        HashMap::<String, OwnedValue>::new(),
                        vec!["PlaybackStatus"],
                    ),
                )
                .unwrap();
            wait_for(Some(state));
        }
        connection
            .release_name("org.mpris.MediaPlayer2.bharta_test")
            .unwrap();
        wait_for(None);
    }
}
