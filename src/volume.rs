use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    time::Instant,
};
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
    pub index: u32,
    pub application: String,
    pub icon: String,
    pub name: String,
    pub percent: u32,
    pub muted: bool,
    pub corked: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub default: String,
    pub outputs: Vec<Output>,
    pub streams: Vec<Stream>,
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
}
pub enum Request {
    Refresh,
    Control(u64, Control),
}
pub struct Update {
    pub completed: Option<u64>,
    pub result: Result<Snapshot, String>,
}
fn pactl(args: &[String]) -> Result<Vec<u8>> {
    let mut child = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context(
            "Audio controls require pactl and a running PulseAudio or PipeWire-Pulse server",
        )?;
    let stdout = child.stdout.take().context("Missing pactl output")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = vec![];
        stdout
            .take(2 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + crate::config::get().duration("timeouts.command_ms");
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("Audio reader failed"))??;
                ensure!(
                    status.success(),
                    "Audio server rejected the request; the output may have disconnected"
                );
                return Ok(bytes);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(crate::config::get().duration("timeouts.command_poll_ms"))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                bail!("Audio server did not respond");
            }
        }
    }
}
fn run(args: &[&str]) -> Result<Vec<u8>> {
    pactl(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
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
    let default = String::from_utf8(run(&["get-default-sink"])?)?
        .trim()
        .to_owned();
    let mut snapshot = parse(
        &serde_json::from_slice(&run(&["--format=json", "list", "sinks"])?)?,
        default,
    )?;
    snapshot.streams = parse_streams(&serde_json::from_slice(&run(&[
        "--format=json",
        "list",
        "sink-inputs",
    ])?)?);
    Ok(snapshot)
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
fn volumes(output: &Output, channel: Option<&str>, percent: u8) -> Result<Vec<String>> {
    Ok(volume_values(output, channel, percent)?
        .into_iter()
        .map(|v| v.to_string())
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
    match control {
        Control::StreamVolume(index, percent) => {
            ensure!(percent <= 150, "Stream volume must be at most 150 percent");
            ensure!(
                current.streams.iter().any(|stream| stream.index == index),
                "Audio stream ended"
            );
            run(&[
                "set-sink-input-volume",
                &index.to_string(),
                &format!("{percent}%"),
            ])?;
            return Ok(());
        }
        Control::StreamMute(index) => {
            let stream = current
                .streams
                .iter()
                .find(|stream| stream.index == index)
                .context("Audio stream ended")?;
            run(&[
                "set-sink-input-mute",
                &index.to_string(),
                if stream.muted { "0" } else { "1" },
            ])?;
            return Ok(());
        }
        _ => {}
    }
    let name = match &control {
        Control::Volume { output, .. }
        | Control::Mute(output)
        | Control::Output(output)
        | Control::Port(output, _) => output,
        Control::StreamVolume(_, _) | Control::StreamMute(_) => unreachable!(),
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
        } => {
            let mut args = vec!["set-sink-volume".into(), name];
            args.extend(volumes(output, channel.as_deref(), percent)?);
            pactl(&args)?;
        }
        Control::Mute(name) => {
            run(&["set-sink-mute", &name, if output.muted { "0" } else { "1" }])?;
        }
        Control::Port(name, port) => {
            ensure!(
                output.ports.iter().any(|p| p.name == port && p.available),
                "Audio port is unavailable"
            );
            run(&["set-sink-port", &name, &port])?;
        }
        Control::Output(name) => {
            run(&["set-default-sink", &name])?;
            // Move current playback too, so choosing an output is audible immediately.
            let inputs: Value =
                serde_json::from_slice(&run(&["--format=json", "list", "sink-inputs"])?)?;
            for input in inputs.as_array().into_iter().flatten() {
                if let Some(id) = input["index"].as_u64() {
                    // A stream can disappear between enumeration and move.
                    let _ = run(&["move-sink-input", &id.to_string(), &name]);
                }
            }
        }
        Control::StreamVolume(_, _) | Control::StreamMute(_) => unreachable!(),
    }
    Ok(())
}
pub fn watch(sender: std::sync::mpsc::Sender<Update>) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
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
            request = match rx.recv_timeout(crate::config::get().duration("intervals.volume_ms")) {
                Ok(r) => r,
                Err(mpsc::RecvTimeoutError::Timeout) => Request::Refresh,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
        }
    });
    tx
}
#[cfg(test)]
mod tests {
    use super::*;
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
            volumes(o, Some("front-left"), 25).unwrap(),
            ["65536", "16384"]
        );
        assert_eq!(volumes(o, None, 50).unwrap(), ["32768", "16384"]);
        assert!(volumes(o, Some("missing"), 50).is_err());
        assert!(volumes(o, None, 101).is_err());
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
        assert_eq!(streams[0].percent, 50);
        assert!(!streams[0].corked && !streams[0].muted);
        assert_eq!(streams[1].name, "Music");
        assert!(streams[1].corked && streams[1].muted);
        let raw = parse_streams(&serde_json::json!([
            {"index":9,"properties":{"application.name":"ALSA plug-in [fruisic]", "media.name":"ALSA Playback", "application.icon_name":"fruisic"}},
            {"index":10,"properties":{"application.name":"Firefox", "media.name":"AudioStream"}}
        ]));
        assert!(
            raw.iter()
                .any(|s| s.name == "fruisic" && s.icon == "fruisic")
        );
        assert!(raw.iter().any(|s| s.name == "Firefox"));
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
        assert_eq!(volumes(o, None, 50).unwrap(), ["32768", "32768"]);
        s.default = "removed".into();
        assert!(s.active().is_none());
        assert!(parse(&Value::Null, String::new()).is_err());
    }
    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire-Pulse server; creates a temporary null sink"]
    fn live_volume_and_mute_roundtrip() {
        struct Module(String);
        impl Drop for Module {
            fn drop(&mut self) {
                let _ = run(&["unload-module", &self.0]);
            }
        }
        let name = format!("bharta-test-{}", std::process::id());
        let args = format!("sink_name={name}");
        let id =
            String::from_utf8(run(&["load-module", "module-null-sink", &args]).unwrap()).unwrap();
        let _module = Module(id.trim().into());
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
