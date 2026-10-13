//! Native-protocol audio fixture confined to the harness's private socket.
use anyhow::{Context, Result};
use pulseaudio::protocol::{self as pa, port_info::*, stream::BufferAttr, *};
use serde_json::{Value, json};
use std::{
    ffi::CString,
    io::{Cursor, Read},
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Mutex},
    time::Duration,
};
fn read(key: &str) -> Value {
    serde_json::from_slice(&std::fs::read(std::env::var(key).unwrap()).unwrap()).unwrap()
}
fn write(key: &str, value: &Value) {
    std::fs::write(std::env::var(key).unwrap(), value.to_string()).unwrap();
}
fn volume() -> ChannelVolume {
    let mut v = ChannelVolume::empty();
    for value in read("BHARTA_TEST_AUDIO").as_array().unwrap() {
        v.push(Volume::from_u32_clamped(value.as_u64().unwrap() as u32));
    }
    v
}
fn ports(names: &[(&str, &str)]) -> Vec<PortInfo> {
    names
        .iter()
        .map(|(name, description)| PortInfo {
            name: CString::new(*name).unwrap(),
            description: Some(CString::new(*description).unwrap()),
            port_type: PortType::Unknown,
            dir: PortDirection::Input,
            priority: 1,
            available: PortAvailable::Yes,
            availability_group: None,
        })
        .collect()
}
fn sinks() -> Vec<SinkInfo> {
    let routing = read("BHARTA_TEST_ROUTING");
    [
        (
            "test",
            "Built-in audio",
            ports(&[("speaker", "Speakers"), ("headphones", "Headphones")]),
        ),
        ("hdmi", "External display", ports(&[("hdmi", "HDMI")])),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, description, ports))| {
        let active_port = ports
            .iter()
            .position(|p| p.name.to_str().unwrap() == routing[name].as_str().unwrap())
            .unwrap();
        SinkInfo {
            index: i as u32,
            name: CString::new(name).unwrap(),
            description: Some(CString::new(description).unwrap()),
            sample_spec: SampleSpec {
                format: SampleFormat::S16Le,
                channels: 2,
                sample_rate: 48000,
            },
            channel_map: ChannelMap::stereo(),
            cvolume: volume(),
            muted: read("BHARTA_TEST_MUTED").as_bool().unwrap(),
            monitor_source_index: Some(100 + i as u32),
            ports,
            active_port,
            ..Default::default()
        }
    })
    .collect()
}
fn streams() -> Vec<SinkInputInfo> {
    let mut inputs = vec![];
    for (index, name, media, value, sink) in [
        (11, "Firefox", "YouTube", 49152, 0),
        (12, "Spotify", "Music", 32768, 1),
        (13, "Fixture", "", 32768, 0),
    ] {
        let mut props = Props::new();
        props.set(Prop::ApplicationName, CString::new(name).unwrap());
        props.set(Prop::MediaName, CString::new(media).unwrap());
        if index == 13 {
            let Ok(pid) = std::fs::read_to_string(std::env::var("BHARTA_TEST_WINDOW_PID").unwrap())
            else {
                continue;
            };
            props.set(
                Prop::ApplicationProcessId,
                CString::new(pid.trim()).unwrap(),
            );
        }
        let mut cvolume = ChannelVolume::empty();
        cvolume.push(Volume::from_u32_clamped(value));
        cvolume.push(Volume::from_u32_clamped(value));
        inputs.push(SinkInputInfo {
            index,
            name: CString::new(name).unwrap(),
            sink_index: sink,
            sample_spec: SampleSpec {
                format: SampleFormat::S16Le,
                channels: 2,
                sample_rate: 48000,
            },
            channel_map: ChannelMap::stereo(),
            cvolume,
            props,
            has_volume: true,
            volume_writable: true,
            ..Default::default()
        });
    }
    inputs
}
fn list<T: TagStructWrite>(
    socket: &mut UnixStream,
    sequence: u32,
    items: &[T],
    version: u16,
) -> Result<()> {
    let mut bytes = vec![];
    let mut writer = TagStructWriter::new(&mut bytes, version);
    writer.write_u32(CommandTag::Reply as u32)?;
    writer.write_u32(sequence)?;
    for item in items {
        writer.write(item)?;
    }
    pa::write_memblock(socket, u32::MAX, &bytes, 0)?;
    Ok(())
}
fn serve(mut socket: UnixStream, state: Arc<Mutex<()>>) -> Result<()> {
    let mut version = pa::MAX_VERSION;
    loop {
        let desc = pa::read_descriptor(&mut socket)?;
        anyhow::ensure!(desc.length < 4 * 1024 * 1024, "Large request");
        let mut bytes = vec![0; desc.length as usize];
        socket.read_exact(&mut bytes)?;
        let (sequence, command) = Command::read_tag_prefixed(&mut Cursor::new(bytes), version)?;
        let lock = state.lock().unwrap();
        match command {
            Command::Auth(auth) => {
                anyhow::ensure!(
                    !auth.supports_shm && !auth.supports_memfd && auth.cookie.len() == 256,
                    "Client must use socket transport"
                );
                version = auth.version.min(pa::MAX_VERSION);
                pa::write_reply_message(
                    &mut socket,
                    sequence,
                    &AuthReply {
                        version,
                        ..Default::default()
                    },
                    version,
                )?;
            }
            Command::SetClientName(_) => {
                pa::write_reply_message(
                    &mut socket,
                    sequence,
                    &SetClientNameReply { client_id: 0 },
                    version,
                )?;
            }
            Command::GetServerInfo => pa::write_reply_message(
                &mut socket,
                sequence,
                &ServerInfo {
                    default_sink_name: Some(
                        CString::new(read("BHARTA_TEST_ROUTING")["default"].as_str().unwrap())
                            .unwrap(),
                    ),
                    sample_spec: SampleSpec {
                        format: SampleFormat::S16Le,
                        channels: 2,
                        sample_rate: 48000,
                    },
                    channel_map: ChannelMap::stereo(),
                    ..Default::default()
                },
                version,
            )?,
            Command::GetSinkInfoList => list(&mut socket, sequence, &sinks(), version)?,
            Command::GetSinkInfo(query) => {
                let routing = read("BHARTA_TEST_ROUTING");
                let name = query.name.as_ref().map(|s| s.to_str().unwrap());
                let name = if name == Some("@DEFAULT_SINK@") {
                    routing["default"].as_str()
                } else {
                    name
                };
                let sink = sinks()
                    .into_iter()
                    .find(|s| query.index == Some(s.index) || name == s.name.to_str().ok())
                    .context("Missing sink")?;
                pa::write_reply_message(&mut socket, sequence, &sink, version)?;
            }
            Command::GetSinkInputInfoList => list(&mut socket, sequence, &streams(), version)?,
            Command::SetSinkVolume(params) => {
                write(
                    "BHARTA_TEST_AUDIO",
                    &json!(
                        params
                            .volume
                            .channels()
                            .iter()
                            .map(Volume::as_u32)
                            .collect::<Vec<_>>()
                    ),
                );
                pa::write_ack_message(&mut socket, sequence)?;
            }
            Command::SetSinkMute(params) => {
                write("BHARTA_TEST_MUTED", &json!(params.mute));
                pa::write_ack_message(&mut socket, sequence)?;
            }
            Command::SetSinkPort(params) => {
                let mut route = read("BHARTA_TEST_ROUTING");
                route[params.name.unwrap().to_str().unwrap()] =
                    json!(params.port_name.to_str().unwrap());
                write("BHARTA_TEST_ROUTING", &route);
                pa::write_ack_message(&mut socket, sequence)?;
            }
            Command::SetDefaultSink(name) => {
                let mut route = read("BHARTA_TEST_ROUTING");
                route["default"] = json!(name.to_str().unwrap());
                write("BHARTA_TEST_ROUTING", &route);
                pa::write_ack_message(&mut socket, sequence)?;
            }
            Command::MoveSinkInput(params) => {
                let mut moves = read("BHARTA_TEST_MOVES");
                moves.as_array_mut().unwrap().push(json!([
                    params.index.unwrap().to_string(),
                    params.device_name.unwrap().to_str().unwrap()
                ]));
                write("BHARTA_TEST_MOVES", &moves);
                pa::write_ack_message(&mut socket, sequence)?;
            }
            Command::SetSinkInputMute(_) | Command::SetSinkInputVolume(_) => {
                pa::write_ack_message(&mut socket, sequence)?
            }
            Command::Subscribe(_) => {
                pa::write_ack_message(&mut socket, sequence)?;
                drop(lock);
                let mut previous = String::new();
                loop {
                    let current = {
                        let _lock = state.lock().unwrap();
                        format!(
                            "{}{}{}{}",
                            read("BHARTA_TEST_AUDIO"),
                            read("BHARTA_TEST_ROUTING"),
                            read("BHARTA_TEST_MUTED"),
                            std::path::Path::new(&std::env::var("BHARTA_TEST_WINDOW_PID").unwrap())
                                .exists()
                        )
                    };
                    if current != previous {
                        pa::write_command_message(
                            &mut socket,
                            u32::MAX,
                            &Command::SubscribeEvent(SubscriptionEvent {
                                event_facility: SubscriptionEventFacility::Sink,
                                event_type: SubscriptionEventType::Changed,
                                index: Some(0),
                            }),
                            version,
                        )?;
                        previous = current;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Command::CreateRecordStream(params) => {
                let expected = if params.direct_on_input_index == Some(12) {
                    101
                } else {
                    100
                };
                if params.direct_on_input_index.is_some() {
                    anyhow::ensure!(
                        params.source_index == Some(expected),
                        "Per-app monitor attached to the wrong output"
                    );
                }
                pa::write_reply_message(
                    &mut socket,
                    sequence,
                    &CreateRecordStreamReply {
                        channel: 0,
                        stream_index: 40,
                        sample_spec: params.sample_spec,
                        channel_map: params.channel_map,
                        buffer_attr: BufferAttr {
                            max_length: 48000,
                            fragment_size: 2400,
                            ..Default::default()
                        },
                        sink_index: expected,
                        ..Default::default()
                    },
                    version,
                )?;
                drop(lock);
                let mut phase = 0f64;
                let mut frame = 0u32;
                loop {
                    let tone =
                        std::fs::read_to_string(std::env::var("BHARTA_TEST_TRAY_TONE").unwrap())?;
                    let dynamic = tone.trim() == "music";
                    let frequency = match params.direct_on_input_index {
                        Some(11) => 750.,
                        Some(_) => 1600.,
                        None if dynamic => 375.,
                        None => tone.trim().parse::<f64>().unwrap_or(375.),
                    };
                    let amplitude = if dynamic {
                        10000. * (0.55 + 0.45 * (frame as f64 * 0.37).sin())
                    } else {
                        10000.
                    };
                    let mut bytes = vec![];
                    for _ in 0..1024 {
                        bytes.extend_from_slice(&((amplitude * phase.sin()) as i16).to_le_bytes());
                        phase = (phase + std::f64::consts::TAU * frequency / 24000.)
                            % std::f64::consts::TAU;
                    }
                    pa::write_memblock(&mut socket, 0, &bytes, 0)?;
                    frame += 1;
                    std::thread::sleep(Duration::from_secs_f64(1024. / 24000.));
                }
            }
            _ => pa::write_error(&mut socket, sequence, &PulseError::NotImplemented)?,
        }
    }
}
fn main() -> Result<()> {
    let path = std::env::var("PULSE_SERVER")?;
    let listener = UnixListener::bind(
        path.strip_prefix("unix:")
            .context("Private Unix socket required")?,
    )?;
    let state = Arc::new(Mutex::new(()));
    for socket in listener.incoming() {
        let state = state.clone();
        let socket = socket?;
        std::thread::spawn(move || {
            if let Err(error) = serve(socket, state) {
                eprintln!("fixture client: {error:#}");
            }
        });
    }
    Ok(())
}
