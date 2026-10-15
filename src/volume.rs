use anyhow::{Context, Result, ensure};
use pulseaudio::protocol::*;
use serde_json::Value;
use std::{sync::mpsc, time::Instant};
#[derive(Clone, Debug)]
pub struct Channel {
    pub name: String,
    pub value: u32,
}
impl Channel {
    pub fn percent(&self) -> u32 {
        (self.value as f64 * 100. / 65536.).round() as u32
    }
}
#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub description: String,
    pub available: bool,
}
#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    pub description: String,
    pub muted: bool,
    pub channels: Vec<Channel>,
    pub ports: Vec<Port>,
    pub active_port: String,
}
impl Output {
    pub fn available(&self) -> bool {
        // Portless sinks (including virtual outputs) have no jack detection.
        // A sink with only disconnected ports must not become a fallback row.
        self.ports.is_empty() || self.ports.iter().any(|port| port.available)
    }

    pub fn percent(&self) -> u32 {
        self.channels
            .iter()
            .map(Channel::percent)
            .max()
            .unwrap_or(0)
    }
}
#[derive(Clone, Debug)]
pub struct Stream {
    pub pid: u32,
    pub media: Option<String>,
    pub index: u32,
    pub application: String,
    pub icon: String,
    pub name: String,
    pub percent: u32,
    pub muted: bool,
    pub corked: bool,
}
#[derive(Clone, Debug, Default)]
pub struct ProfileOutput {
    pub card: String,
    pub profile: String,
    pub port: String,
    pub description: String,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub default: String,
    pub outputs: Vec<Output>,
    pub streams: Vec<Stream>,
    pub profiles: Vec<ProfileOutput>,
}
impl Snapshot {
    pub fn active(&self) -> Option<&Output> {
        self.outputs.iter().find(|s| s.name == self.default)
    }
}
#[derive(Clone, Debug)]
pub enum Control {
    Volume {
        output: String,
        channel: Option<String>,
        percent: u8,
    },
    Mute(String),
    Output(String),
    Port(String, String),
    StreamVolume(u32, u8),
    StreamMute(u32),
    Profile(ProfileOutput),
}
pub enum Request {
    Refresh,
    Control(u64, Control),
}
pub struct Update {
    pub completed: Option<u64>,
    pub result: Result<Snapshot, String>,
}
fn parse(value: &Value, default: String) -> Result<Snapshot> {
    let mut outputs = vec![];
    for v in value.as_array().context("Invalid audio output list")? {
        let Some(name) = v["name"].as_str() else {
            continue;
        };
        // Channel-map order, not JSON object order, is pactl's positional order.
        let channels = v["channel_map"]
            .as_str()
            .unwrap_or("")
            .split(',')
            .filter_map(|name| {
                let name = name.trim();
                Some(Channel {
                    name: name.into(),
                    value: u32::try_from(v["volume"][name]["value"].as_u64()?).ok()?,
                })
            })
            .collect();
        let ports = v["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(Port {
                    name: p["name"].as_str()?.into(),
                    description: p["description"].as_str().unwrap_or("Port").into(),
                    available: !matches!(p["availability"].as_str(), Some("not available" | "no")),
                })
            })
            .collect();
        outputs.push(Output {
            name: name.into(),
            description: v["description"].as_str().unwrap_or(name).into(),
            muted: v["mute"].as_bool().unwrap_or(false),
            channels,
            ports,
            active_port: v["active_port"]
                .as_str()
                .or_else(|| v["active_port"]["name"].as_str())
                .unwrap_or("")
                .into(),
        });
    }
    Ok(Snapshot {
        default,
        outputs,
        streams: vec![],
        profiles: vec![],
    })
}
fn parse_streams(value: &Value) -> Vec<Stream> {
    let mut streams: Vec<_> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let index = u32::try_from(v["index"].as_u64()?).ok()?;
            let properties = &v["properties"];
            let app = crate::media::application_name(properties);
            let media = properties["media.title"]
                .as_str()
                .or_else(|| properties["media.name"].as_str())
                .filter(|m| {
                    let m = m.trim().to_ascii_lowercase();
                    !m.is_empty()
                        && !m.starts_with("alsa ")
                        && !matches!(
                            m.as_str(),
                            "audiostream" | "audio stream" | "playback" | "music"
                        )
                });
            let application = app.to_string();
            let name = match media {
                Some(media) if media != app => format!("{app} · {media}"),
                _ => app.to_string(),
            };
            let percent = v["volume"]
                .as_object()
                .into_iter()
                .flat_map(|channels| channels.values())
                .filter_map(|channel| channel["value"].as_u64())
                .max()
                .unwrap_or(0)
                .saturating_mul(100)
                .div_ceil(65536)
                .min(150) as u32;
            Some(Stream {
                pid: properties["application.process.id"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                media: media.map(str::to_owned),
                index,
                icon: properties["application.icon_name"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| app.to_lowercase().replace(' ', "")),
                application,
                name,
                percent,
                muted: v["mute"].as_bool().unwrap_or(false),
                corked: v["corked"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    streams.sort_by(|a, b| {
        a.application
            .to_lowercase()
            .cmp(&b.application.to_lowercase())
            .then_with(|| a.index.cmp(&b.index))
    });
    streams
}
pub fn read() -> Result<Snapshot> {
    let (default, outputs, streams) = crate::pulse::snapshot_json()?;
    let mut snapshot = parse(&outputs, default)?;
    snapshot.streams = parse_streams(&streams);
    snapshot.profiles = profile_outputs(crate::pulse::cards().unwrap_or_default(), &outputs);
    Ok(snapshot)
}
fn profile_outputs(cards: Vec<CardInfo>, outputs: &Value) -> Vec<ProfileOutput> {
    let mut targets = Vec::new();
    {
        for card in cards {
            for port in &card.ports {
                let port_name = port.name.to_string_lossy();
                // PulseAudio's output direction is wire value 1. pulseaudio 0.3.1
                // names that value Input (a sink input), and names microphones Output.
                if port.dir != port_info::PortDirection::Input
                    || port.available == port_info::PortAvailable::No
                {
                    continue;
                }
                if outputs.as_array().into_iter().flatten().any(|o| {
                    o["card"].as_u64() == Some(u64::from(card.index))
                        && o["ports"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|p| p["name"].as_str() == Some(&port_name))
                }) {
                    continue;
                }
                // Prefer a stereo profile while retaining the card's input when possible.
                let profile = card
                    .profiles
                    .iter()
                    .filter(|p| {
                        p.num_sinks > 0 && p.available != 0 && port.profiles.contains(&p.name)
                    })
                    .max_by_key(|p| {
                        (
                            p.name.to_string_lossy().contains("stereo"),
                            p.num_sources > 0,
                            p.priority,
                        )
                    });
                if let Some(profile) = profile {
                    targets.push(ProfileOutput {
                        card: card.name.to_string_lossy().into_owned(),
                        profile: profile.name.to_string_lossy().into_owned(),
                        port: port_name.into_owned(),
                        description: port
                            .description
                            .as_ref()
                            .unwrap_or(&port.name)
                            .to_string_lossy()
                            .into_owned(),
                    });
                }
            }
        }
    }
    targets
}
fn volume_values(output: &Output, channel: Option<&str>, percent: u8) -> Result<Vec<u32>> {
    ensure!(!output.channels.is_empty(), "Output has no volume channels");
    ensure!(percent <= 100, "Volume must be between 0 and 100 percent");
    if let Some(name) = channel {
        ensure!(
            output.channels.iter().any(|c| c.name == name),
            "Channel is no longer available"
        );
    }
    let target = (u32::from(percent) * 65536 + 50) / 100;
    let peak = output.channels.iter().map(|c| c.value).max().unwrap_or(0);
    Ok(output
        .channels
        .iter()
        .map(|c| match channel {
            Some(name) if c.name == name => target,
            Some(_) => c.value,
            None if peak > 0 => (u64::from(c.value) * u64::from(target) / u64::from(peak)) as u32,
            None => target,
        })
        .collect())
}
// Immediate visual feedback uses the same channel math as the audio worker.
pub fn preview(snapshot: &mut Snapshot, control: &Control) {
    if let Control::Volume {
        output,
        channel,
        percent,
    } = control
        && let Some(sink) = snapshot.outputs.iter_mut().find(|s| &s.name == output)
        && let Ok(values) = volume_values(sink, channel.as_deref(), *percent)
    {
        for (c, value) in sink.channels.iter_mut().zip(values) {
            c.value = value;
        }
    }
}
fn apply(control: Control) -> Result<()> {
    let current = read()?;
    crate::pulse::with(|connection| {
        let command = match control {
            Control::Profile(target) => {
                let card = connection
                    .list::<CardInfo>(&Command::GetCardInfoList)?
                    .into_iter()
                    .find(|c| c.name.to_string_lossy() == target.card)
                    .context("Audio card disconnected")?;
                connection.request(&Command::SetCardProfile(SetCardProfileParams {
                    card_index: None,
                    card_name: Some(crate::pulse::name(&target.card)?),
                    profile_name: crate::pulse::name(&target.profile)?,
                }))?;
                let deadline =
                    Instant::now() + crate::config::get().duration("timeouts.command_ms");
                loop {
                    let sinks = connection.list::<SinkInfo>(&Command::GetSinkInfoList)?;
                    if let Some(sink) = sinks.iter().find(|s| {
                        s.card_index == Some(card.index)
                            && s.ports
                                .iter()
                                .any(|p| p.name.to_string_lossy() == target.port)
                    }) {
                        let name = sink.name.to_string_lossy();
                        connection.request(&Command::SetSinkPort(SetPortParams {
                            index: Some(sink.index),
                            name: None,
                            port_name: crate::pulse::name(&target.port)?,
                        }))?;
                        return activate_output(connection, &name);
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Audio output did not appear after activating its profile"
                    );
                    std::thread::sleep(crate::config::get().duration("timeouts.command_poll_ms"));
                }
            }
            Control::StreamVolume(index, percent) => {
                ensure!(percent <= 150, "Stream volume must be at most 150 percent");
                let stream = connection
                    .streams()?
                    .into_iter()
                    .find(|s| s.index == index)
                    .context("Audio stream ended")?;
                Command::SetSinkInputVolume(SetStreamVolumeParams {
                    index,
                    volume: crate::pulse::volumes(std::iter::repeat_n(
                        (u32::from(percent) * 65536 + 50) / 100,
                        stream.cvolume.channels().len(),
                    )),
                })
            }
            Control::StreamMute(index) => {
                let stream = current
                    .streams
                    .iter()
                    .find(|s| s.index == index)
                    .context("Audio stream ended")?;
                Command::SetSinkInputMute(SetStreamMuteParams {
                    index,
                    mute: !stream.muted,
                })
            }
            control => {
                let name = match &control {
                    Control::Volume { output, .. }
                    | Control::Mute(output)
                    | Control::Output(output)
                    | Control::Port(output, _) => output,
                    _ => unreachable!(),
                };
                let output = current
                    .outputs
                    .iter()
                    .find(|s| &s.name == name)
                    .context("Output disconnected")?;
                match control {
                    Control::Volume {
                        output: name,
                        channel,
                        percent,
                    } => Command::SetSinkVolume(SetDeviceVolumeParams {
                        device_index: None,
                        device_name: Some(crate::pulse::name(&name)?),
                        volume: crate::pulse::volumes(volume_values(
                            output,
                            channel.as_deref(),
                            percent,
                        )?),
                    }),
                    Control::Mute(name) => Command::SetSinkMute(SetDeviceMuteParams {
                        device_index: None,
                        device_name: Some(crate::pulse::name(&name)?),
                        mute: !output.muted,
                    }),
                    Control::Port(name, port) => {
                        ensure!(
                            output.ports.iter().any(|p| p.name == port && p.available),
                            "Audio port is unavailable"
                        );
                        connection.request(&Command::SetSinkPort(SetPortParams {
                            index: None,
                            name: Some(crate::pulse::name(&name)?),
                            port_name: crate::pulse::name(&port)?,
                        }))?;
                        return activate_output(connection, &name);
                    }
                    Control::Output(name) => return activate_output(connection, &name),
                    _ => unreachable!(),
                }
            }
        };
        connection.request(&command)?;
        Ok(())
    })
}
fn activate_output(connection: &mut crate::pulse::Connection, name: &str) -> Result<()> {
    connection.request(&Command::SetDefaultSink(crate::pulse::name(name)?))?;
    for stream in connection.streams()? {
        // Streams can end between enumeration and moving them.
        let _ = connection.request(&Command::MoveSinkInput(MoveStreamParams {
            index: Some(stream.index),
            device_index: None,
            device_name: Some(crate::pulse::name(name)?),
        }));
    }
    Ok(())
}
pub fn watch(sender: std::sync::mpsc::Sender<Update>) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut subscription = crate::pulse::Connection::subscribe().ok();
        let mut request = Request::Refresh;
        loop {
            let (completed, result) = match request {
                Request::Refresh => (None, read()),
                Request::Control(id, control) => (Some(id), apply(control).and_then(|_| read())),
            };
            if sender
                .send(Update {
                    completed,
                    result: result.map_err(|e| e.to_string()),
                })
                .is_err()
            {
                break;
            }
            let deadline = Instant::now() + crate::config::get().duration("intervals.volume_ms");
            request = loop {
                let wait = if subscription.is_some() {
                    crate::config::get().duration("intervals.idle_poll_ms")
                } else {
                    deadline.saturating_duration_since(Instant::now())
                };
                match rx.recv_timeout(wait.min(deadline.saturating_duration_since(Instant::now())))
                {
                    Ok(r) => break r,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Some(events) = subscription.as_mut() {
                    match events.changed() {
                        Ok(true) => break Request::Refresh,
                        Ok(false) => {}
                        Err(_) => subscription = None,
                    }
                }
                if Instant::now() >= deadline {
                    if subscription.is_none() {
                        subscription = crate::pulse::Connection::subscribe().ok();
                    }
                    break Request::Refresh;
                }
            };
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inactive_hdmi_profiles_and_speaker_return_are_discovered() {
        let mut card = CardInfo {
            index: 1,
            name: c"card".into(),
            props: Props::new(),
            owner_module_index: None,
            driver: None,
            ports: vec![],
            profiles: vec![],
            active_profile: Some(c"analog".into()),
        };
        for (name, sources, priority) in [
            ("hdmi-stereo", 0, 100),
            ("hdmi-stereo+input", 1, 90),
            ("analog", 1, 200),
        ] {
            card.profiles.push(CardProfileInfo {
                name: crate::pulse::name(name).unwrap(),
                description: None,
                priority,
                available: 1,
                num_sinks: 1,
                num_sources: sources,
            });
        }
        for (name, profiles) in [
            (
                "hdmi-output-0",
                vec![c"hdmi-stereo".into(), c"hdmi-stereo+input".into()],
            ),
            ("speaker", vec![c"analog".into()]),
        ] {
            card.ports.push(CardPortInfo {
                name: crate::pulse::name(name).unwrap(),
                description: None,
                port_type: Default::default(),
                props: Props::new(),
                dir: port_info::PortDirection::Input,
                priority: 0,
                available: port_info::PortAvailable::Yes,
                availability_group: None,
                profiles,
                latency_offset: 0,
            });
        }
        // Pin the actual PulseAudio wire direction, independent of the crate's names.
        assert_eq!(card.ports[0].dir as u8, 1);
        let mut microphone = card.ports[1].clone();
        microphone.name = c"analog-input-internal-mic".into();
        microphone.description = Some(c"Internal Microphone".into());
        microphone.dir = port_info::PortDirection::Output;
        assert_eq!(microphone.dir as u8, 2);
        // A duplex profile has sinks, so profile.num_sinks alone cannot filter a mic.
        card.ports.push(microphone);
        let analog = serde_json::json!([{"card":1,"ports":[{"name":"speaker"}]}]);
        let targets = profile_outputs(vec![card.clone()], &analog);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].profile, "hdmi-stereo+input");
        let hdmi = serde_json::json!([{"card":1,"ports":[{"name":"hdmi-output-0"}]}]);
        assert_eq!(
            profile_outputs(vec![card.clone()], &hdmi)[0].port,
            "speaker"
        );
        card.ports[0].available = port_info::PortAvailable::No;
        assert!(profile_outputs(vec![card.clone()], &analog).is_empty());
        card.ports[0].available = port_info::PortAvailable::Yes;
        for p in &mut card.profiles {
            p.available = 0;
        }
        assert!(profile_outputs(vec![card], &analog).is_empty());
    }
    fn fixture() -> Snapshot {
        parse(&serde_json::json!([{
        "name":"speaker", "channel_map":"front-right,front-left", "volume":{
            "front-left":{"value":32768}, "front-right":{"value":65536}},
        "ports":[{"name":"headphones","availability":"not available"}], "active_port":{"name":"speaker"}
    }]), "speaker".into()).unwrap()
    }
    #[test]
    fn channel_changes_preserve_other_channels_and_server_order() {
        let s = fixture();
        let o = s.active().unwrap();
        assert_eq!(
            volume_values(o, Some("front-left"), 25).unwrap(),
            [65536, 16384]
        );
        assert_eq!(volume_values(o, None, 50).unwrap(), [32768, 16384]);
        assert!(volume_values(o, Some("missing"), 50).is_err());
        assert!(volume_values(o, None, 101).is_err());
        assert!(!o.ports[0].available);
        assert_eq!(o.active_port, "speaker");
    }
    #[test]
    fn parses_distinct_app_stream_names_and_states() {
        let streams = parse_streams(&serde_json::json!([
            {"index":7,"corked":false,"mute":false,"volume":{"left":{"value":32768}},"properties":{"application.name":"Firefox","media.name":"YouTube"}},
            {"index":8,"corked":true,"mute":true,"volume":{"left":{"value":65536}},"properties":{"application.name":"Music"}}
        ]));
        assert_eq!(streams[0].name, "Firefox · YouTube");
        assert_eq!(streams[0].media.as_deref(), Some("YouTube"));
        assert_eq!(streams[0].percent, 50);
        assert!(!streams[0].corked && !streams[0].muted);
        assert_eq!(streams[1].name, "Music");
        assert!(streams[1].corked && streams[1].muted);
        let raw = parse_streams(&serde_json::json!([
            {"index":9,"properties":{"application.name":"PipeWire ALSA [fruisic]", "media.name":"ALSA Playback", "application.icon_name":"fruisic"}},
            {"index":10,"properties":{"application.name":"Firefox", "media.name":"AudioStream"}}
        ]));
        assert!(
            raw.iter()
                .any(|s| s.name == "fruisic" && s.icon == "fruisic")
        );
        assert!(raw.iter().any(|s| s.name == "Firefox" && s.media.is_none()));
    }
    #[test]
    fn source_order_survives_pause_and_metadata_changes() {
        let mut inputs = serde_json::json!([
            {"index":3,"corked":false,"properties":{"application.name":"Spotify", "media.name":"A song"}},
            {"index":2,"corked":true,"properties":{"application.name":"Firefox", "media.name":"Z video"}},
            {"index":1,"corked":false,"properties":{"application.name":"Firefox", "media.name":"A video"}}
        ]);
        let order = |v: &Value| parse_streams(v).iter().map(|s| s.index).collect::<Vec<_>>();
        assert_eq!(order(&inputs), vec![1, 2, 3]);
        inputs[0]["corked"] = true.into();
        inputs[1]["corked"] = false.into();
        inputs[1]["properties"]["media.name"] = "A different video".into();
        inputs[2]["properties"]["media.name"] = "Z different video".into();
        assert_eq!(order(&inputs), vec![1, 2, 3]);
    }
    #[test]
    fn silent_and_disconnected_outputs() {
        let mut s = fixture();
        let o = &mut s.outputs[0];
        for c in &mut o.channels {
            c.value = 0;
        }
        assert_eq!(volume_values(o, None, 50).unwrap(), [32768, 32768]);
        s.default = "removed".into();
        assert!(s.active().is_none());
        assert!(parse(&Value::Null, String::new()).is_err());
    }
    #[test]
    fn output_choices_exclude_disconnected_ports_but_keep_portless_sinks() {
        let snapshot = parse(
            &serde_json::json!([
                {"name":"unplugged-hdmi", "ports":[{"name":"hdmi", "availability":"no"}]},
                {"name":"unplugged-headset", "ports":[{"name":"headphones", "availability":"not available"}]},
                {"name":"speakers", "ports":[{"name":"speaker", "availability":"unknown"}, {"name":"headphones", "availability":"no"}]},
                {"name":"connected", "ports":[{"name":"hdmi", "availability":"yes"}]},
                {"name":"virtual", "ports":[]}
            ]),
            "unplugged-hdmi".into(),
        ).unwrap();
        assert_eq!(
            snapshot
                .outputs
                .iter()
                .filter(|o| o.available())
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>(),
            ["speakers", "connected", "virtual"]
        );
        // Retain the server's active sink for volume controls while it reroutes.
        assert!(snapshot.active().is_some());
    }
    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire-Pulse server; creates a temporary null sink"]
    fn live_volume_and_mute_roundtrip() {
        struct Module(u32);
        impl Drop for Module {
            fn drop(&mut self) {
                let _ = crate::pulse::with(|c| c.request(&Command::UnloadModule(self.0)));
            }
        }
        let name = format!("bharta-test-{}", std::process::id());
        let args = format!("sink_name={name}");
        let id = crate::pulse::with(|c| {
            c.request(&Command::LoadModule(LoadModuleParams {
                name: c"module-null-sink".into(),
                arguments: Some(crate::pulse::name(&args)?),
            }))
        })
        .unwrap();
        let id = TagStructReader::new(&mut std::io::Cursor::new(id), MAX_VERSION)
            .read_u32()
            .unwrap();
        let _module = Module(id);
        apply(Control::Volume {
            output: name.clone(),
            channel: None,
            percent: 40,
        })
        .unwrap();
        apply(Control::Volume {
            output: name.clone(),
            channel: Some("front-left".into()),
            percent: 20,
        })
        .unwrap();
        let snapshot = read().unwrap();
        let output = snapshot.outputs.iter().find(|o| o.name == name).unwrap();
        assert_eq!(
            output
                .channels
                .iter()
                .find(|c| c.name == "front-left")
                .unwrap()
                .percent(),
            20
        );
        assert_eq!(
            output
                .channels
                .iter()
                .find(|c| c.name == "front-right")
                .unwrap()
                .percent(),
            40
        );
        // Create a paused stereo app on the temporary output; stream controls
        // must retain the channel count and work independently of sink volume.
        let mut playback = crate::pulse::Connection::open().unwrap();
        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"bharta stream test");
        let bytes = playback
            .request(&Command::CreatePlaybackStream(PlaybackStreamParams {
                sample_spec: SampleSpec {
                    format: SampleFormat::S16Le,
                    channels: 2,
                    sample_rate: 48000,
                },
                channel_map: ChannelMap::stereo(),
                sink_name: Some(crate::pulse::name(&name).unwrap()),
                cvolume: Some(ChannelVolume::norm(2)),
                props,
                flags: pulseaudio::protocol::stream::StreamFlags {
                    start_corked: true,
                    ..Default::default()
                },
                ..Default::default()
            }))
            .unwrap();
        let stream = CreatePlaybackStreamReply::read(
            &mut TagStructReader::new(&mut std::io::Cursor::new(bytes), MAX_VERSION),
            MAX_VERSION,
        )
        .unwrap();
        apply(Control::StreamVolume(stream.stream_index, 60)).unwrap();
        apply(Control::StreamMute(stream.stream_index)).unwrap();
        let inputs = playback.streams().unwrap();
        let input = inputs
            .iter()
            .find(|s| s.index == stream.stream_index)
            .unwrap();
        assert!(input.muted);
        assert_eq!(input.cvolume.channels().len(), 2);
        assert!(input.cvolume.channels().iter().all(|v| v.as_u32() == 39322));
        apply(Control::StreamMute(stream.stream_index)).unwrap();
        assert!(
            !playback
                .streams()
                .unwrap()
                .iter()
                .find(|s| s.index == stream.stream_index)
                .unwrap()
                .muted
        );
        playback
            .request(&Command::DeletePlaybackStream(stream.channel))
            .unwrap();
        apply(Control::Mute(name.clone())).unwrap();
        assert!(
            read()
                .unwrap()
                .outputs
                .iter()
                .find(|o| o.name == name)
                .unwrap()
                .muted
        );
    }
}
