use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    process::{Command, Stdio},
    time::Instant,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioProcess {
    pub pid: u32,
    pub application: String,
    pub media: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub workspace: String,
    pub title: String,
    pub application: String,
}
#[derive(Clone, Debug, Default)]
pub struct Extras {
    pub volume: Option<(u32, bool)>,
    pub artwork: Option<std::sync::Arc<crate::artwork::Artwork>>,
    pub wifi_name: Option<String>,
    pub wifi_signal: Option<u8>,
    pub wifi_enabled: Option<bool>,
    pub track: Option<Track>,
    pub tracks: Vec<Track>,
    pub artworks: HashMap<String, std::sync::Arc<crate::artwork::Artwork>>,
    pub audio_sources: Vec<AudioProcess>,
}
pub enum Update {
    Artwork(std::sync::Arc<crate::artwork::Artwork>),
    Wifi(Result<crate::network::Snapshot, String>),
    Playback(Vec<Track>),
    Audio(Vec<AudioProcess>),
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
type UiSender = std::sync::mpsc::Sender<Update>;
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
            std::thread::sleep(crate::config::get().duration("intervals.media_ms"));
        }
    });
    let audio = sender.clone();
    std::thread::spawn(move || {
        loop {
            if audio.send(Update::Audio(audio_sources())).is_err() {
                break;
            }
            std::thread::sleep(crate::config::get().duration("intervals.media_retry_ms"));
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
            std::thread::sleep(crate::config::get().duration("intervals.media_retry_ms"));
        }
    });
    // A single producer owns playback snapshots and button commands. An old
    // polling result can no longer overwrite a newer command/signal update.
    std::thread::spawn(move || {
        loop {
            let request =
                match receiver.recv_timeout(crate::config::get().duration("intervals.media_ms")) {
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
            let tracks = connection.ok().map(|c| tracks_on(&c)).unwrap_or_default();
            let art_urls = tracks.iter().map(|t| t.art_url.clone()).collect();
            if sender.send(Update::Playback(tracks)).is_err() {
                break;
            }
            let _ = artwork.send(art_urls);
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
    tracks_on(c).into_iter().next()
}
fn tracks_on(c: &Connection) -> Vec<Track> {
    let bus = Proxy::new(
        c,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    );
    let Ok(bus) = bus else {
        return vec![];
    };
    let Ok(mut names) = bus.call::<_, _, Vec<String>>("ListNames", &()) else {
        return vec![];
    };
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
    tracks
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
fn parse_audio(value: &Value) -> Vec<AudioProcess> {
    let mut processes: Vec<AudioProcess> = value
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
            let properties = &v["properties"];
            let pid = properties["application.process.id"]
                .as_str()
                .and_then(|p| p.parse().ok())
                .or_else(|| u32::try_from(properties["application.process.id"].as_u64()?).ok())?;
            Some(AudioProcess {
                pid,
                application: application_name(properties).into(),
                media: properties["media.title"]
                    .as_str()
                    .or_else(|| properties["media.name"].as_str())
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect();
    processes
        .sort_by(|a, b| (a.pid, &a.application, &a.media).cmp(&(b.pid, &b.application, &b.media)));
    processes.dedup();
    processes
}
pub fn audio_sources() -> Vec<AudioProcess> {
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
    let deadline = Instant::now() + crate::config::get().duration("timeouts.command_ms");
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
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(crate::config::get().duration("timeouts.command_poll_ms"))
            }
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
pub fn window_details(v: &Value, workspace: Option<&str>, map: &mut HashMap<u32, Vec<WindowInfo>>) {
    let workspace = if v["type"] == "workspace" {
        v["name"].as_str()
    } else {
        workspace
    };
    if let (Some(pid), Some(workspace)) = (v["pid"].as_u64(), workspace) {
        let properties = &v["window_properties"];
        let info = WindowInfo {
            workspace: workspace.into(),
            title: v["name"].as_str().unwrap_or("").into(),
            application: v["app_id"]
                .as_str()
                .or_else(|| properties["class"].as_str())
                .unwrap_or("")
                .into(),
        };
        let windows = map.entry(pid as u32).or_default();
        if !windows.contains(&info) {
            windows.push(info);
        }
    }
    for child in ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| v[*key].as_array())
        .flatten()
    {
        window_details(child, workspace, map);
    }
}
pub fn audible_workspaces(
    sources: &[AudioProcess],
    map: &HashMap<u32, HashSet<String>>,
    details: &HashMap<u32, Vec<WindowInfo>>,
) -> HashSet<String> {
    resolve_audio_workspaces(sources, map, details, |pid| {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("PPid:").and_then(|p| p.trim().parse().ok()))
    })
}
pub fn application_name(properties: &Value) -> &str {
    let name = properties["application.name"].as_str().unwrap_or("");
    if name.to_ascii_lowercase().starts_with("alsa ") {
        return properties["application.process.binary"]
            .as_str()
            .or_else(|| name.split_once('[').and_then(|(_, n)| n.strip_suffix(']')))
            .unwrap_or("Audio");
    }
    if name.is_empty() {
        properties["application.process.binary"]
            .as_str()
            .unwrap_or("Audio")
    } else {
        name
    }
}
pub fn player_matches(application: &str, player: &str) -> bool {
    let app = normalize(application);
    let app = app
        .strip_prefix("mozilla")
        .or_else(|| app.strip_prefix("google"))
        .unwrap_or(&app);
    !app.is_empty() && normalize(player).contains(app)
}
pub fn source_titles(sources: &[AudioProcess], tracks: &[Track]) -> Vec<AudioProcess> {
    sources
        .iter()
        .cloned()
        .map(|mut source| {
            if source.media.as_deref().is_none_or(|m| {
                matches!(
                    normalize(m).as_str(),
                    "" | "audiostream" | "alsaplayback" | "playback"
                )
            }) {
                let candidates: Vec<_> = tracks
                    .iter()
                    .filter(|t| t.playing && player_matches(&source.application, &t.player))
                    .collect();
                if let [track] = candidates.as_slice() {
                    source.media = Some(track.title.clone());
                }
            }
            source
        })
        .collect()
}
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn resolve_audio_workspaces(
    sources: &[AudioProcess],
    map: &HashMap<u32, HashSet<String>>,
    details: &HashMap<u32, Vec<WindowInfo>>,
    parent: impl Fn(u32) -> Option<u32>,
) -> HashSet<String> {
    let mut audible = HashSet::new();
    for source in sources {
        let mut pid = source.pid;
        for _ in 0..32 {
            let Some(workspaces) = map.get(&pid) else {
                match parent(pid) {
                    Some(p) if p > 1 && p != pid => pid = p,
                    _ => break,
                }
                continue;
            };
            if workspaces.len() <= 1 {
                audible.extend(workspaces.iter().cloned());
                break;
            }
            let windows = details.get(&pid).map(Vec::as_slice).unwrap_or_default();
            let media = source.media.as_deref().map(normalize).unwrap_or_default();
            let application = normalize(&source.application);
            let media_matches: HashSet<_> = windows
                .iter()
                .filter(|window| {
                    let title = normalize(&window.title);
                    !media.is_empty()
                        && !title.is_empty()
                        && (title.contains(&media) || media.contains(&title))
                })
                .map(|window| window.workspace.clone())
                .collect();
            let matches = if !media_matches.is_empty() {
                media_matches
            } else {
                let app_matches: HashSet<_> = windows
                    .iter()
                    .filter(|window| {
                        let app = normalize(&window.application);
                        !application.is_empty()
                            && !app.is_empty()
                            && (app.contains(&application) || application.contains(&app))
                    })
                    .map(|window| window.workspace.clone())
                    .collect();
                if app_matches.len() == 1 {
                    app_matches
                } else {
                    HashSet::new()
                }
            };
            audible.extend(matches);
            break;
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
        assert_eq!(parse_audio(&v)[0].pid, 123);
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
            resolve_audio_workspaces(
                &[AudioProcess {
                    pid: 43,
                    application: String::new(),
                    media: None
                }],
                &map,
                &HashMap::new(),
                |p| if p == 43 { Some(42) } else { None }
            ),
            HashSet::from(["4".into()])
        );
    }
    #[test]
    fn same_browser_pid_uses_media_title_to_select_its_workspace() {
        let map = HashMap::from([(42, HashSet::from(["2".into(), "3".into()]))]);
        let details = HashMap::from([(
            42,
            vec![
                WindowInfo {
                    workspace: "2".into(),
                    title: "Firefox — Mail".into(),
                    application: "firefox".into(),
                },
                WindowInfo {
                    workspace: "3".into(),
                    title: "YouTube — Firefox".into(),
                    application: "firefox".into(),
                },
            ],
        )]);
        let source = AudioProcess {
            pid: 42,
            application: "Firefox".into(),
            media: Some("YouTube".into()),
        };
        assert_eq!(
            resolve_audio_workspaces(&[source], &map, &details, |_| None),
            HashSet::from(["3".into()])
        );
        let ambiguous = AudioProcess {
            pid: 42,
            application: "Firefox".into(),
            media: None,
        };
        assert!(resolve_audio_workspaces(&[ambiguous], &map, &details, |_| None).is_empty());
        let generic = AudioProcess {
            pid: 42,
            application: "Firefox".into(),
            media: Some("AudioStream".into()),
        };
        let track = Track {
            player: "org.mpris.MediaPlayer2.firefox.instance123".into(),
            title: "YouTube".into(),
            art_url: String::new(),
            artist: String::new(),
            playing: true,
            can_previous: false,
            can_next: false,
            can_toggle: true,
        };
        assert_eq!(
            resolve_audio_workspaces(&source_titles(&[generic], &[track]), &map, &details, |_| {
                None
            }),
            HashSet::from(["3".into()])
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
    use std::time::Duration;
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
        let (tx, rx) = std::sync::mpsc::channel();
        let _requests = spawn_playback(tx, Some(address.into()));
        let wait_for = |expected: Option<bool>| {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(800) {
                if let Ok(Update::Playback(tracks)) = rx.try_recv()
                    && tracks.first().map(|t| t.playing) == expected
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
