mod artwork;
mod audio_meter;
mod bluetooth;
mod capture;
mod config;
mod cpu;
mod icons;
mod iwd;
mod launcher;
mod media;
mod network;
mod popup_motion;
mod ui;
// The small headless PNG exporter is independent of the live egui interface.
#[allow(dead_code)]
mod render;
mod status;
mod supervisor;
mod volume;
mod workspace_preview;

use anyhow::{Context, Result, bail};
use render::Renderer;

#[derive(Default, Clone)]
struct Options {
    all_outputs: bool,
    dark: bool,
    output: Option<String>,
    font: Option<String>,
    preview: Option<String>,
    smoke: bool,
    check_network: bool,
    check_bluetooth: bool,
    check_media: bool,
    config: Option<String>,
}
fn options() -> Result<Options> {
    let mut options = Options {
        dark: config::get().dark(),
        font: (!config::get().text("appearance.font").is_empty())
            .then(|| config::get().text("appearance.font").into()),
        ..Options::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all-outputs" => options.all_outputs = true,
            "--dark" => options.dark = true,
            "--light" => options.dark = false,
            "--config" => options.config = Some(args.next().context("--config needs a path")?),
            "--print-config" => {
                print!("{}", config::DEFAULT);
                std::process::exit(0);
            }
            "--output" => {
                options.output = Some(args.next().context("--output needs an output name")?)
            }
            "--font" => options.font = Some(args.next().context("--font needs a font path")?),
            "--preview" => {
                options.preview = Some(args.next().context("--preview needs a PNG path")?)
            }
            "--smoke-test" => options.smoke = true,
            "--check-network" => options.check_network = true,
            "--check-bluetooth" => options.check_bluetooth = true,
            "--check-media" => options.check_media = true,
            "--help" | "-h" => {
                println!(
                    "bharta — native Sway menu bar\n\nUsage: bharta [--dark] [--all-outputs | --output NAME] [--font PATH]\n       bharta [--dark] --preview FILE.png\n       bharta --smoke-test\n\nClick workspaces to switch. Default: all active outputs. Default settings: ~/.config/bharta/config.json (or XDG_CONFIG_HOME).\n--config PATH selects a configuration file; --print-config prints all defaults.\n--dark / --light and --font override configuration.\n--preview renders sample data without connecting to Wayland.\n--smoke-test connects, renders a frame, then exits.\n--check-bluetooth checks BlueZ and lists devices without changing Bluetooth settings."
                );
                std::process::exit(0);
            }
            _ => bail!("Unknown option {arg}; use --help"),
        }
    }
    anyhow::ensure!(
        !(options.all_outputs && options.output.is_some()),
        "Use --all-outputs or --output, not both"
    );
    Ok(options)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|a| a == "--print-config") {
        print!("{}", config::DEFAULT);
        return Ok(());
    }
    let path = args
        .windows(2)
        .rfind(|pair| pair[0] == "--config")
        .map(|pair| std::path::PathBuf::from(&pair[1]));
    config::init(path)?;
    let options = options()?;
    if options.check_bluetooth {
        let snapshot = bluetooth::snapshot()?;
        println!(
            "BlueZ: {}; Bluetooth adapters: {}; Bluetooth devices: {}; wireless receivers: {}",
            snapshot.bluez,
            snapshot.adapters.len(),
            snapshot.devices.len(),
            snapshot.receivers.len()
        );
        if let Some(notice) = &snapshot.notice {
            println!("{notice}");
        }
        for receiver in &snapshot.receivers {
            println!("{} ({}) — USB receiver present", receiver.name, receiver.id);
        }
        for device in snapshot.devices {
            println!(
                "{} ({}) — {}",
                device.name,
                device.address,
                if !snapshot.bluez {
                    "kernel input present"
                } else if device.connected {
                    "connected"
                } else if device.paired {
                    "saved"
                } else {
                    "discovered"
                }
            );
        }
        return Ok(());
    }
    if options.check_network {
        let snapshot = network::scan(false)?;
        println!(
            "Backend: {:?}; Wi-Fi enabled: {}; networks: {}; connected: {}",
            snapshot.backend,
            snapshot.enabled,
            snapshot.networks.len(),
            snapshot.networks.iter().any(|n| n.active)
        );
        return Ok(());
    }
    if options.check_media {
        let mut state = status::Status::read(None);
        state.extras.track = media::track();
        state.extras.audio_sources = media::audio_sources();
        state.update_audio();
        println!(
            "Track available: {}; active audio sources: {}; sounding workspaces: {:?}",
            state.extras.track.is_some(),
            state.extras.audio_sources.len(),
            state
                .workspaces
                .iter()
                .filter(|w| w.audible)
                .map(|w| &w.name)
                .collect::<Vec<_>>()
        );
        return Ok(());
    }
    if let Some(path) = &options.preview {
        Renderer::new(options.font.as_deref(), options.dark)?
            .draw(
                1440,
                2,
                &status::Status::demo(),
                "Tue Oct 6   9:41 AM",
                None,
            )?
            .0
            .save_png(path)?;
        println!("Preview saved to {path}");
        return Ok(());
    }
    if !options.smoke && (options.all_outputs || options.output.is_none()) {
        return supervisor::run(&options);
    }
    ui::run(options)
}
