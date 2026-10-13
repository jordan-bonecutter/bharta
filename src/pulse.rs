//! PulseAudio native protocol over the user's socket, also supported by PipeWire-Pulse.
//! Shared memory is disabled so this works in the static musl executable too.
use anyhow::{Context, Result, bail, ensure};
use pulseaudio::protocol::{self as pa, *};
use pulseaudio::protocol::{
    port_info::PortAvailable,
    stream::{BufferAttr, StreamFlags},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    ffi::CString,
    io::{Cursor, ErrorKind, Read},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Instant,
};

pub struct Connection {
    socket: UnixStream,
    version: u16,
    sequence: u32,
    pending: Vec<u8>,
}
impl Connection {
    pub fn open() -> Result<Self> {
        let path = socket_path()?;
        let socket = UnixStream::connect(&path)
            .with_context(|| format!("Cannot connect to the audio server at {}", path.display()))?;
        Self::authenticate(socket, cookie()?)
    }
    fn authenticate(socket: UnixStream, cookie: Vec<u8>) -> Result<Self> {
        let timeout = crate::config::get().duration("timeouts.command_ms");
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        let mut connection = Self {
            socket,
            version: pa::MAX_VERSION,
            sequence: 0,
            pending: vec![],
        };
        let reply = connection.request(&Command::Auth(AuthParams {
            version: pa::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }))?;
        let auth = AuthReply::read(
            &mut TagStructReader::new(&mut Cursor::new(reply), pa::MAX_VERSION),
            pa::MAX_VERSION,
        )?;
        ensure!(
            auth.version >= 24,
            "Audio server protocol is too old (need PulseAudio 2.0+)"
        );
        connection.version = auth.version.min(pa::MAX_VERSION);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"bharta");
        connection.request(&Command::SetClientName(props))?;
        Ok(connection)
    }
    pub fn request(&mut self, command: &Command) -> Result<Vec<u8>> {
        self.sequence = self.sequence.wrapping_add(1);
        pa::write_command_message(&mut self.socket, self.sequence, command, self.version)?;
        let deadline = Instant::now() + crate::config::get().duration("timeouts.command_ms");
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "Audio server did not respond");
            self.socket.set_read_timeout(Some(remaining))?;
            let (channel, bytes) = self.packet()?;
            ensure!(
                channel == u32::MAX,
                "Unexpected audio samples during a control request"
            );
            let mut cursor = Cursor::new(bytes);
            let (sequence, response) = Command::read_tag_prefixed(&mut cursor, self.version)?;
            if sequence != self.sequence {
                continue;
            }
            match response {
                Command::Reply => {
                    return Ok(cursor.get_ref()[cursor.position() as usize..].to_vec());
                }
                Command::Error(error) => bail!("Audio server rejected request: {error:?}"),
                _ => bail!("Unexpected audio response"),
            }
        }
    }
    pub fn list<T: TagStructRead>(&mut self, command: &Command) -> Result<Vec<T>> {
        let bytes = self.request(command)?;
        let mut cursor = Cursor::new(bytes);
        let mut items = vec![];
        while cursor.position() < cursor.get_ref().len() as u64 {
            items.push(T::read(
                &mut TagStructReader::new(&mut cursor, self.version),
                self.version,
            )?);
        }
        Ok(items)
    }
    pub fn packet(&mut self) -> Result<(u32, Vec<u8>)> {
        let descriptor = pa::read_descriptor(&mut self.socket)?;
        ensure!(
            descriptor.length as usize <= pa::MAX_MEMBLOCKQ_LENGTH,
            "Oversized audio packet"
        );
        ensure!(
            descriptor.flags.is_empty(),
            "Unexpected shared audio memory"
        );
        let mut bytes = vec![0; descriptor.length as usize];
        self.socket.read_exact(&mut bytes)?;
        Ok((descriptor.channel, bytes))
    }
    pub fn streams(&mut self) -> Result<Vec<SinkInputInfo>> {
        self.list(&Command::GetSinkInputInfoList)
    }
    pub fn record(&mut self, stream: Option<u32>) -> Result<u32> {
        // A per-app monitor must be attached to that app's own sink, not the
        // current default output. Never substitute mixed output if it disappears.
        let sink = if let Some(index) = stream {
            let input = self
                .streams()?
                .into_iter()
                .find(|s| s.index == index)
                .context("Audio stream ended")?;
            self.list::<SinkInfo>(&Command::GetSinkInfo(GetSinkInfo {
                index: Some(input.sink_index),
                name: None,
            }))?
        } else {
            self.list::<SinkInfo>(&Command::GetSinkInfo(GetSinkInfo {
                index: None,
                name: Some(c"@DEFAULT_SINK@".into()),
            }))?
        };
        let source = sink
            .first()
            .and_then(|sink| sink.monitor_source_index)
            .context("Output has no monitor")?;
        let bytes = self.request(&Command::CreateRecordStream(RecordStreamParams {
            sample_spec: SampleSpec {
                format: SampleFormat::S16Le,
                channels: 1,
                sample_rate: 24_000,
            },
            channel_map: ChannelMap::mono(),
            source_index: Some(source),
            direct_on_input_index: stream,
            buffer_attr: BufferAttr {
                max_length: 48_000,
                fragment_size: 2_400,
                ..Default::default()
            },
            flags: StreamFlags {
                adjust_latency: true,
                ..Default::default()
            },
            ..Default::default()
        }))?;
        let reply = CreateRecordStreamReply::read(
            &mut TagStructReader::new(&mut Cursor::new(bytes), self.version),
            self.version,
        )?;
        ensure!(
            reply.sample_spec.format == SampleFormat::S16Le
                && reply.sample_spec.channels == 1
                && reply.sample_spec.sample_rate == 24_000,
            "Audio server changed meter sample format"
        );
        self.socket.set_read_timeout(None)?;
        Ok(reply.channel)
    }
    pub fn shutdown_handle(&self) -> Result<UnixStream> {
        Ok(self.socket.try_clone()?)
    }
    pub fn subscribe() -> Result<Self> {
        let mut connection = Self::open()?;
        connection.request(&Command::Subscribe(
            SubscriptionMask::SINK
                | SubscriptionMask::SINK_INPUT
                | SubscriptionMask::SERVER
                | SubscriptionMask::CARD,
        ))?;
        connection.socket.set_nonblocking(true)?;
        Ok(connection)
    }
    pub fn changed(&mut self) -> Result<bool> {
        let mut changed = false;
        let mut bytes = [0; 4096];
        loop {
            match self.socket.read(&mut bytes) {
                Ok(0) => bail!("Audio event socket disconnected"),
                Ok(n) => self.pending.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
            ensure!(
                self.pending.len() <= pa::MAX_MEMBLOCKQ_LENGTH + pa::DESCRIPTOR_SIZE,
                "Oversized audio event"
            );
            while self.pending.len() >= pa::DESCRIPTOR_SIZE {
                let desc = pa::read_descriptor(&mut Cursor::new(&self.pending))?;
                ensure!(
                    desc.length as usize <= pa::MAX_MEMBLOCKQ_LENGTH,
                    "Oversized audio event"
                );
                let end = pa::DESCRIPTOR_SIZE + desc.length as usize;
                if self.pending.len() < end {
                    break;
                }
                let (_, command) = Command::read_tag_prefixed(
                    &mut Cursor::new(&self.pending[pa::DESCRIPTOR_SIZE..end]),
                    self.version,
                )?;
                changed |= matches!(
                    command,
                    Command::SubscribeEvent(SubscriptionEvent {
                        event_facility: SubscriptionEventFacility::Sink
                            | SubscriptionEventFacility::SinkInput
                            | SubscriptionEventFacility::Server
                            | SubscriptionEventFacility::Card,
                        ..
                    })
                );
                self.pending.drain(..end);
            }
        }
        Ok(changed)
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
fn socket_path() -> Result<PathBuf> {
    if let Some(server) = std::env::var_os("PULSE_SERVER") {
        let server = server.to_string_lossy();
        let path = server.strip_prefix("unix:").unwrap_or(&server);
        ensure!(
            path.starts_with('/'),
            "PULSE_SERVER must name a local Unix socket"
        );
        return Ok(path.into());
    }
    if let Some(runtime) = std::env::var_os("PULSE_RUNTIME_PATH") {
        return Ok(PathBuf::from(runtime).join("native"));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions.
            PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
        });
    Ok(runtime.join("pulse/native"))
}
fn cookie() -> Result<Vec<u8>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let paths = if let Some(path) = std::env::var_os("PULSE_COOKIE") {
        vec![PathBuf::from(path)]
    } else {
        vec![config.join("pulse/cookie"), home.join(".pulse-cookie")]
    };
    for path in paths {
        match std::fs::read(path) {
            Ok(cookie) => {
                ensure!(cookie.len() == 256, "Invalid audio authentication cookie");
                return Ok(cookie);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    // Local servers normally authorize by the Unix socket's peer credentials.
    Ok(vec![0; 256])
}
thread_local! { static CLIENT: RefCell<Option<Connection>> = const { RefCell::new(None) }; }
pub fn with<T>(operation: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
    CLIENT.with(|client| {
        let mut client = client.borrow_mut();
        if client.is_none() {
            *client = Some(Connection::open()?);
        }
        let result = operation(client.as_mut().unwrap());
        if result.is_err() {
            *client = None;
        }
        result
    })
}
pub fn streams_json() -> Result<Value> {
    with(streams_json_connection)
}
fn streams_json_connection(c: &mut Connection) -> Result<Value> {
    let streams = c.streams()?;
    let mut values: Vec<_> = streams.iter().map(stream_json).collect();
    // PipeWire ALSA streams can omit process metadata that is present on
    // their owning client. Keep stream properties authoritative.
    if streams.iter().zip(&values).any(|(stream, value)| {
        stream.client_index.is_some() && value["properties"]["application.process.id"].is_null()
    }) && let Ok(clients) = c.list::<ClientInfo>(&Command::GetClientInfoList)
    {
        for (stream, value) in streams.iter().zip(&mut values) {
            if let Some(client) = clients
                .iter()
                .find(|client| Some(client.index) == stream.client_index)
            {
                inherit_client_properties(value, &properties(&client.props));
            }
        }
    }
    Ok(Value::Array(values))
}
fn inherit_client_properties(stream: &mut Value, client: &Value) {
    if let (Some(stream), Some(client)) = (stream["properties"].as_object_mut(), client.as_object())
    {
        for (key, value) in client {
            stream.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
}
pub fn snapshot_json() -> Result<(String, Value, Value)> {
    with(|c| {
        let bytes = c.request(&Command::GetServerInfo)?;
        let info = ServerInfo::read(
            &mut TagStructReader::new(&mut Cursor::new(bytes), c.version),
            c.version,
        )?;
        let outputs = c
            .list::<SinkInfo>(&Command::GetSinkInfoList)?
            .iter()
            .map(sink_json)
            .collect::<Vec<_>>();
        let streams = streams_json_connection(c)?;
        Ok((
            info.default_sink_name
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Value::Array(outputs),
            streams,
        ))
    })
}
pub fn cards() -> Result<Vec<CardInfo>> {
    with(|c| c.list(&Command::GetCardInfoList))
}

fn properties(props: &Props) -> Value {
    Value::Object(
        props
            .iter()
            .filter_map(|(k, v)| {
                let value = std::ffi::CStr::from_bytes_with_nul(v).ok()?;
                Some((
                    k.to_string_lossy().into_owned(),
                    Value::String(value.to_string_lossy().into_owned()),
                ))
            })
            .collect(),
    )
}
fn channel_name(position: ChannelPosition) -> String {
    let text = format!("{position:?}");
    let mut name = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i > 0 && ch.is_ascii_uppercase() {
            name.push('-');
        }
        name.push(ch.to_ascii_lowercase());
    }
    name
}
fn volume_json(map: &ChannelMap, volume: &ChannelVolume) -> Value {
    Value::Object(
        map.into_iter()
            .zip(volume.channels())
            .map(|(p, v)| (channel_name(p), json!({"value": v.as_u32()})))
            .collect(),
    )
}
fn sink_json(sink: &SinkInfo) -> Value {
    json!({"name": sink.name.to_string_lossy(), "description": sink.description.as_ref().map(|s| s.to_string_lossy()), "channel_map": (&sink.channel_map).into_iter().map(channel_name).collect::<Vec<_>>().join(","), "volume": volume_json(&sink.channel_map, &sink.cvolume), "mute": sink.muted, "card": sink.card_index,
        "ports": sink.ports.iter().map(|p| json!({"name": p.name.to_string_lossy(), "description": p.description.as_ref().map(|d| d.to_string_lossy()), "availability": if p.available == PortAvailable::No { "no" } else { "yes" }})).collect::<Vec<_>>(),
        "active_port": sink.ports.get(sink.active_port).map(|p| p.name.to_string_lossy())})
}
fn stream_json(stream: &SinkInputInfo) -> Value {
    json!({"index": stream.index, "mute": stream.muted, "corked": stream.corked, "volume": volume_json(&stream.channel_map, &stream.cvolume), "properties": properties(&stream.props)})
}
pub fn volumes(values: impl IntoIterator<Item = u32>) -> ChannelVolume {
    let mut volume = ChannelVolume::empty();
    for value in values {
        volume.push(Volume::from_u32_clamped(value));
    }
    volume
}
pub fn name(value: &str) -> Result<CString> {
    Ok(CString::new(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_inherits_client_pid_without_replacing_media_metadata() {
        let mut stream = json!({"properties":{"application.name":"Music","media.name":"Song"}});
        inherit_client_properties(
            &mut stream,
            &json!({"application.process.id":"42","application.name":"Backend"}),
        );
        assert_eq!(stream["properties"]["application.process.id"], "42");
        assert_eq!(stream["properties"]["application.name"], "Music");
        inherit_client_properties(&mut stream, &json!({"application.process.id":"99"}));
        assert_eq!(stream["properties"]["application.process.id"], "42");
    }
    fn connection(socket: UnixStream) -> Connection {
        Connection {
            socket,
            version: pa::MAX_VERSION,
            sequence: 0,
            pending: vec![],
        }
    }
    #[test]
    fn socket_auth_disables_shared_memory_and_negotiates_version() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let fixture = std::thread::spawn(move || {
            let (sequence, command) = pa::read_command_message(
                &mut std::io::BufReader::new(server.try_clone().unwrap()),
                pa::MAX_VERSION,
            )
            .unwrap();
            let Command::Auth(auth) = command else {
                panic!("Missing authentication")
            };
            assert!(!auth.supports_shm && !auth.supports_memfd);
            assert_eq!(auth.cookie.len(), 256);
            pa::write_reply_message(
                &mut server,
                sequence,
                &AuthReply {
                    version: 32,
                    ..Default::default()
                },
                pa::MAX_VERSION,
            )
            .unwrap();
            let (sequence, command) = pa::read_command_message(
                &mut std::io::BufReader::new(server.try_clone().unwrap()),
                32,
            )
            .unwrap();
            assert!(matches!(command, Command::SetClientName(_)));
            pa::write_reply_message(
                &mut server,
                sequence,
                &SetClientNameReply { client_id: 7 },
                32,
            )
            .unwrap();
        });
        let client = Connection::authenticate(client, vec![0; 256]).unwrap();
        assert_eq!(client.version, 32);
        fixture.join().unwrap();
    }
    #[test]
    fn fragmented_subscription_events_are_preserved() {
        use std::io::Write;
        let (client, mut server) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut client = connection(client);
        let mut bytes = vec![];
        pa::write_command_message(
            &mut bytes,
            u32::MAX,
            &Command::SubscribeEvent(SubscriptionEvent {
                event_facility: SubscriptionEventFacility::SinkInput,
                event_type: SubscriptionEventType::Removed,
                index: Some(12),
            }),
            pa::MAX_VERSION,
        )
        .unwrap();
        server.write_all(&bytes[..23]).unwrap();
        assert!(!client.changed().unwrap());
        server.write_all(&bytes[23..]).unwrap();
        assert!(client.changed().unwrap());
        assert!(!client.changed().unwrap());
    }
    #[test]
    fn oversized_packets_fail_before_allocation() {
        let (client, mut server) = UnixStream::pair().unwrap();
        pa::write_descriptor(
            &mut server,
            &Descriptor {
                length: (pa::MAX_MEMBLOCKQ_LENGTH + 1) as u32,
                channel: u32::MAX,
                offset: 0,
                flags: DescriptorFlags::empty(),
            },
        )
        .unwrap();
        assert!(connection(client).packet().is_err());
    }
    #[test]
    fn server_errors_are_reported_without_waiting_for_timeout() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let fixture = std::thread::spawn(move || {
            let (sequence, _) = pa::read_command_message(
                &mut std::io::BufReader::new(server.try_clone().unwrap()),
                pa::MAX_VERSION,
            )
            .unwrap();
            pa::write_error(&mut server, sequence, &PulseError::NoEntity).unwrap();
        });
        let error = connection(client)
            .request(&Command::GetSinkInfoList)
            .unwrap_err();
        assert!(error.to_string().contains("NoEntity"));
        fixture.join().unwrap();
    }
}
