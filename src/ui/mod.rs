mod application_icons;
mod audio_sources;
mod bar_button;
#[cfg(not(target_env = "musl"))]
mod gpu;
mod keyboard;
mod motion;
mod services;
mod wayland;
use crate::{
    audio_meter, bluetooth, config, icons::Icon, launcher, media, network, popup_motion::Dismissal,
    status, volume, workspace_preview,
};
use egui::{Color32, Pos2, Rect, pos2, vec2};
use services::{Event, Job, Services, SessionAction};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use wayland::run;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Menu {
    Apps,
    Sound,
    Wifi,
    Bluetooth,
    Session,
    Cpu,
    Workspace(String),
    Tray(String),
}
struct Meter {
    levels: Arc<Mutex<[f64; 7]>>,
    capture: Option<audio_meter::Capture>,
    retry: Instant,
    displayed: [f64; 7],
}
impl Meter {
    fn update(&mut self) -> bool {
        // Ignore sub-pixel noise and repaint only when a visible level changes.
        let levels = self
            .levels
            .lock()
            .unwrap()
            .map(|v| (v * 100.).round() / 100.);
        if levels == self.displayed {
            return false;
        }
        self.displayed = levels;
        true
    }
}
fn plot_color(level: f32) -> Color32 {
    Color32::from(egui::ecolor::Hsva::new(
        (1. - level.clamp(0., 1.)) / 3.,
        0.95,
        0.90,
        1.,
    ))
}
fn paint_equalizer(painter: &egui::Painter, bounds: Rect, levels: &[f64; 7]) {
    let painter = painter.with_clip_rect(bounds);
    let step = bounds.width() / levels.len() as f32;
    for (i, level) in levels.iter().enumerate() {
        let level = (*level as f32).clamp(0., 1.);
        let height = (level * bounds.height()).max(1.);
        painter.add(
            egui::epaint::RectShape::filled(
                Rect::from_min_size(
                    pos2(bounds.left() + i as f32 * step, bounds.bottom() - height),
                    vec2((step - 1.).max(1.).min(step), height),
                ),
                1,
                plot_color(level).gamma_multiply(if level > 0. { 1. } else { 0.2 }),
            )
            .with_round_to_pixels(false),
        );
    }
}
#[derive(Clone, PartialEq)]
enum AppLabel {
    Icon(egui::TextureId),
    Name(String),
}

struct Panel {
    kind: Menu,
    x: f32,
    opened: Instant,
    closing: Option<Instant>,
    clicked: bool,
    dismissal: Dismissal,
}
struct Ui {
    ctx: egui::Context,
    services: Services,
    state: status::Status,
    audio: volume::Snapshot,
    cpu: crate::cpu::Snapshot,
    cpu_history: VecDeque<f32>,
    tray_items: Vec<crate::tray::Item>,
    tray_origin: (i32, i32),
    tray_rows: Vec<crate::tray::MenuRow>,
    network: Option<network::Snapshot>,
    bluetooth: Option<bluetooth::Snapshot>,
    bluetooth_pin: String,
    apps: Vec<launcher::Entry>,
    panel: Option<Panel>,
    preview: Option<workspace_preview::Snapshot>,
    // Ready -> waiting for capture -> crossfading -> ready. Timestamps advance
    // on repaint; the outgoing snapshot stays visible while capture is pending.
    preview_previous: Option<(workspace_preview::Snapshot, Instant)>,
    preview_pending: Option<workspace_preview::Snapshot>,
    textures: HashMap<String, egui::TextureHandle>,
    meters: HashMap<u32, Meter>,
    tray_meter: Meter,
    tray_output: Option<String>,
    query: String,
    selection: usize,
    selected_network: Option<network::Network>,
    hidden_ssid: Option<String>,
    password: String,
    reveal: bool,
    confirm_session: Option<SessionAction>,
    focus_search: bool,
    error: String,
    busy: bool,
    pending: VecDeque<volume::Control>,
    serial: u64,
    claim: u128,
    hover: Option<(Menu, Instant)>,
    blocked_hover: Option<Menu>,
    pending_menu: Option<(Menu, f32, bool)>,
    missing_icons: std::collections::HashSet<String>,
    app_icon_pixels: HashMap<String, crate::tray::Icon>,
    app_icon_requests: std::sync::mpsc::Sender<String>,
    app_icon_results: std::sync::mpsc::Receiver<application_icons::Result>,
    drawer_motion: motion::DrawerMotion,
    workspace_icon_fades: HashMap<String, motion::Crossfade<Vec<egui::TextureId>>>,
    app_icon_fade: motion::Crossfade<AppLabel>,
    preview_size: [f32; 2],
    panel_rect: Rect,
    dark: bool,
}
impl Ui {
    fn new(options: &crate::Options) -> anyhow::Result<Self> {
        let ctx = egui::Context::default();
        // Keep clicks and worker commands exactly once; sizing settles on the next frame.
        ctx.options_mut(|options| options.max_passes = std::num::NonZeroUsize::new(1).unwrap());
        let mut style = (*ctx.global_style()).clone();
        style.visuals = if options.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        style.visuals.widgets.active.bg_fill = config::get().color("active_widget", options.dark);
        style.visuals.selection.bg_fill = config::get().color("selection", options.dark);
        style.visuals.override_text_color = Some(config::get().color("text", options.dark));
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
        style.spacing.item_spacing = vec2(
            config::get().number("layout.item_gap_x"),
            config::get().number("layout.item_gap_y"),
        );
        style.spacing.button_padding = vec2(
            config::get().number("layout.button_padding_x"),
            config::get().number("layout.button_padding_y"),
        );
        style.spacing.interact_size = vec2(
            config::get().number("layout.widget_min_width"),
            config::get().number("layout.widget_min_height"),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::proportional(config::get().number("appearance.font_size")),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::proportional(config::get().number("appearance.font_size")),
        );
        ctx.set_global_style(style);
        let mut fonts = egui::FontDefinitions::default();
        for (name, family, path) in [
            (
                "normal",
                egui::FontFamily::Proportional,
                options
                    .font
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .or_else(|| installed_font(false)),
            ),
            (
                "monospace",
                egui::FontFamily::Monospace,
                (!config::get().text("appearance.monospace_font").is_empty())
                    .then(|| {
                        std::path::PathBuf::from(config::get().text("appearance.monospace_font"))
                    })
                    .or_else(|| installed_font(true)),
            ),
        ] {
            if let Some(path) = path {
                let data = std::fs::read(&path).map_err(|error| {
                    anyhow::anyhow!("Read {name} font {}: {error}", path.display())
                })?;
                ab_glyph::FontRef::try_from_slice(&data).map_err(|error| {
                    anyhow::anyhow!("Invalid {name} font {}: {error}", path.display())
                })?;
                fonts
                    .font_data
                    .insert(name.into(), egui::FontData::from_owned(data).into());
                fonts
                    .families
                    .get_mut(&family)
                    .unwrap()
                    .insert(0, name.into());
            }
        }
        ctx.set_fonts(fonts);
        let (app_icon_requests, app_icon_results) = application_icons::worker();
        Ok(Self {
            ctx,
            services: Services::new(options.output.clone()),
            state: status::Status::default(),
            audio: volume::Snapshot::default(),
            cpu: crate::cpu::Snapshot::default(),
            cpu_history: VecDeque::with_capacity(32),
            tray_items: vec![],
            tray_origin: (0, 0),
            tray_rows: vec![],
            network: None,
            bluetooth: None,
            bluetooth_pin: String::new(),
            apps: vec![],
            panel: None,
            preview: None,
            preview_previous: None,
            preview_pending: None,
            textures: HashMap::new(),
            meters: HashMap::new(),
            tray_meter: Meter {
                levels: Arc::new(Mutex::new([0.; 7])),
                capture: None,
                retry: Instant::now(),
                displayed: [0.; 7],
            },
            tray_output: None,
            query: String::new(),
            selection: 0,
            selected_network: None,
            hidden_ssid: None,
            password: String::new(),
            reveal: false,
            confirm_session: None,
            focus_search: false,
            error: String::new(),
            busy: false,
            pending: VecDeque::new(),
            serial: 0,
            claim: 0,
            hover: None,
            blocked_hover: None,
            pending_menu: None,
            missing_icons: Default::default(),
            app_icon_pixels: Default::default(),
            app_icon_requests,
            app_icon_results,
            drawer_motion: Default::default(),
            workspace_icon_fades: Default::default(),
            app_icon_fade: Default::default(),
            preview_size: [312., 220.],
            panel_rect: Rect::NOTHING,
            dark: options.dark,
        })
    }
    fn close(&mut self) {
        self.pending_menu = None;
        let _ = self
            .services
            .tray
            .send(crate::tray::Request::OpenMenu(None));
        if let Some(p) = &mut self.panel {
            self.blocked_hover = Some(p.kind.clone());
            p.closing.get_or_insert(Instant::now());
        }
        let _ = self.services.bluetooth.send(bluetooth::Request::Close);
        self.meters.clear();
        let _ = self.services.cpu.send(false);
        let _ = self.services.preview.send(None);
    }
    fn finish_close(&mut self) {
        self.close();
        self.panel = None;
        self.panel_rect = Rect::NOTHING;
        self.drawer_motion = Default::default();
        self.selected_network = None;
        self.hidden_ssid = None;
        self.password.clear();
        self.bluetooth_pin.clear();
        self.preview = None;
        self.preview_previous = None;
        self.preview_pending = None;
        self.textures
            .retain(|k, _| k.starts_with("icon:") || k.starts_with("app-icon:"));
    }
    fn open(&mut self, kind: Menu, x: f32, clicked: bool) {
        let executable = config::get().text("launcher.executable");
        if kind == Menu::Apps && !executable.is_empty() {
            if !clicked {
                return;
            }
            match launcher::launch_executable(executable, &config::get().strings("launcher.args")) {
                Ok(()) => {
                    self.finish_close();
                    self.blocked_hover = Some(Menu::Apps);
                    return;
                }
                Err(error) => eprintln!("Could not start launcher {executable:?}: {error}"),
            }
        }
        if let Some(panel) = &mut self.panel
            && panel.kind == kind
            && panel.closing.is_none()
        {
            if clicked {
                if panel.clicked {
                    self.blocked_hover = Some(kind);
                    self.close();
                } else {
                    panel.clicked = true;
                }
            }
            return;
        }
        if self.panel.as_ref().is_some_and(|panel| panel.kind != kind) {
            if self.pending_menu.is_none() {
                self.close();
            }
            self.pending_menu = Some((kind, x, clicked));
            self.ctx.request_repaint();
            return;
        }
        let opened = Instant::now();
        let switching_preview = matches!(kind, Menu::Workspace(_))
            && self
                .panel
                .as_ref()
                .is_some_and(|p| matches!(p.kind, Menu::Workspace(_)) && p.closing.is_none());
        let x = if switching_preview {
            self.panel.as_ref().unwrap().x
        } else {
            x
        };
        if switching_preview {
            self.preview_pending = None;
        } else {
            self.finish_close();
        }
        self.blocked_hover = None;
        if !switching_preview {
            self.serial += 1;
        }
        self.claim = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let _ = self
            .services
            .status
            .send(status::Update::AnnouncePopup(self.claim));
        self.error.clear();
        self.query.clear();
        self.selection = 0;
        self.password.clear();
        self.selected_network = None;
        self.hidden_ssid = None;
        self.confirm_session = None;
        self.focus_search = matches!(kind, Menu::Apps | Menu::Cpu);
        match &kind {
            Menu::Tray(id) => {
                self.tray_rows.clear();
                let _ = self
                    .services
                    .tray
                    .send(crate::tray::Request::OpenMenu(Some(id.clone())));
            }
            Menu::Apps => self.services.apps(self.serial),
            Menu::Sound => {
                let _ = self.services.volume.send(volume::Request::Refresh);
                let _ = self.services.media.send(media::Request::Refresh);
            }
            Menu::Wifi => self.services.network(self.serial, Job::Scan(true)),
            Menu::Bluetooth => {
                self.bluetooth = None;
                let _ = self
                    .services
                    .bluetooth
                    .send(bluetooth::Request::Open(self.serial));
                let _ = self.services.bluetooth.send(bluetooth::Request::Scan);
            }
            Menu::Workspace(name) => {
                let _ = self.services.preview.send(Some((
                    name.clone(),
                    self.ctx.pixels_per_point(),
                    self.preview_size,
                )));
            }
            Menu::Session => {}
            Menu::Cpu => {
                let _ = self.services.cpu.send(true);
            }
        }
        self.panel = Some(Panel {
            kind,
            x,
            opened,
            closing: None,
            clicked,
            dismissal: Dismissal::opened(Instant::now()),
        });
    }
    fn control(&mut self, control: volume::Control) {
        volume::preview(&mut self.audio, &control);
        if let volume::Control::StreamVolume(index, percent) = &control
            && let Some(stream) = self.audio.streams.iter_mut().find(|s| s.index == *index)
        {
            stream.percent = *percent as u32;
        }
        if self.busy {
            // Collapse successive updates for the same slider; retain ordered mute/output actions.
            if self
                .pending
                .back()
                .is_some_and(|last| match (last, &control) {
                    (
                        volume::Control::Volume {
                            output: a,
                            channel: ac,
                            ..
                        },
                        volume::Control::Volume {
                            output: b,
                            channel: bc,
                            ..
                        },
                    ) => a == b && ac == bc,
                    (volume::Control::StreamVolume(a, _), volume::Control::StreamVolume(b, _)) => {
                        a == b
                    }
                    _ => false,
                })
            {
                self.pending.pop_back();
            }
            self.pending.push_back(control);
        } else {
            self.busy = true;
            let _ = self
                .services
                .volume
                .send(volume::Request::Control(1, control));
        }
    }
    fn poll(&mut self) {
        while let Ok((name, icon)) = self.app_icon_results.try_recv() {
            if let Some(icon) = icon {
                self.app_icon_pixels.insert(name, icon);
                self.ctx.request_repaint();
            }
        }
        while let Ok(update) = self.services.tray_events.try_recv() {
            match update {
                crate::tray::Update::Items(items) => {
                    self.textures
                        .retain(|key, _| !key.starts_with("tray-icon:"));
                    if self.panel.as_ref().is_some_and(|panel| {
                        matches!(&panel.kind, Menu::Tray(id) if !items.iter().any(|item| &item.id == id))
                    }) { self.finish_close(); }
                    self.tray_items = items;
                }
                crate::tray::Update::Menu(id, rows) => {
                    if self
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.kind == Menu::Tray(id.clone()))
                    {
                        match rows {
                            Ok(rows) => {
                                self.tray_rows = rows;
                                self.error.clear();
                            }
                            Err(error) => self.error = error,
                        }
                    }
                }
            }
            self.ctx.request_repaint();
        }
        while let Ok(s) = self.services.cpu_events.try_recv() {
            if let Some(percent) = s.percent {
                if self.cpu_history.len() == 32 {
                    self.cpu_history.pop_front();
                }
                self.cpu_history.push_back(percent);
            }
            self.cpu = s;
            self.ctx.request_repaint();
        }
        let mut audio_changed = false;
        while let Ok(claim) = self.services.popup_events.try_recv() {
            if claim > self.claim {
                self.finish_close();
                self.hover = None;
            }
        }
        while let Ok(mut s) = self.services.statuses.try_recv() {
            audio_changed = true;
            self.ctx.request_repaint();
            s.extras = self.state.extras.clone();
            self.state = s;
        }
        while let Ok(update) = self.services.volume_events.try_recv() {
            self.ctx.request_repaint();
            if update.completed.is_some() {
                self.busy = false;
                if let Some(c) = self.pending.pop_front() {
                    self.control(c);
                }
            }
            match update.result {
                Ok(s) if !self.busy && !self.ctx.egui_is_using_pointer() => self.audio = s,
                Err(e) => self.error = e,
                _ => {}
            }
        }
        while let Ok(event) = self.services.media_events.try_recv() {
            self.ctx.request_repaint();
            let extras = &mut self.state.extras;
            match event {
                media::Update::Playback(tracks) => {
                    audio_changed = true;
                    extras.track = tracks.first().cloned();
                    // Keep loaded artwork through temporary MPRIS outages. The
                    // worker may coalesce the empty snapshot with reconnection.
                    if !tracks.is_empty() {
                        extras
                            .artworks
                            .retain(|url, _| tracks.iter().any(|track| &track.art_url == url));
                    }
                    extras.tracks = tracks;
                }
                media::Update::Artwork(art) => {
                    extras.artwork = Some(art.clone());
                    extras.artworks.insert(art.url.clone(), art);
                }
                media::Update::Audio(sources) => {
                    audio_changed = true;
                    extras.audio_sources = sources;
                }
                media::Update::Wifi(Ok(s)) => {
                    extras.wifi_enabled = Some(s.enabled);
                    extras.wifi_name = s.networks.iter().find(|n| n.active).map(|n| n.ssid.clone());
                    extras.wifi_signal =
                        s.networks.iter().find(|n| n.active).and_then(|n| n.signal);
                    self.network = Some(s);
                }
                media::Update::ControlFinished(id, Err(e)) if id == self.serial => self.error = e,
                _ => {}
            }
        }
        if audio_changed {
            self.state.update_audio();
        }
        while let Ok((id, result)) = self.services.bluetooth_events.try_recv() {
            if id == self.serial
                && self
                    .panel
                    .as_ref()
                    .is_some_and(|p| p.kind == Menu::Bluetooth && p.closing.is_none())
            {
                match result {
                    Ok(s) => {
                        if self.bluetooth.as_ref().and_then(|old| old.prompt.as_ref())
                            != s.prompt.as_ref()
                            && matches!(
                                s.prompt,
                                Some(bluetooth::Prompt::Pin | bluetooth::Prompt::Passkey)
                            )
                        {
                            self.bluetooth_pin.clear();
                            self.reveal = false;
                            self.ctx
                                .memory_mut(|m| m.request_focus(egui::Id::new("bluetooth-pin")));
                        }
                        if self.bluetooth.is_none()
                            && let Some(panel) = &mut self.panel
                        {
                            panel.opened = Instant::now();
                        }
                        self.bluetooth = Some(s);
                        self.error.clear();
                    }
                    Err(e) => self.error = e,
                }
                self.ctx.request_repaint();
            }
        }
        while let Ok(event) = self.services.events.try_recv() {
            self.ctx.request_repaint();
            match event {
                Event::Apps(id, apps) if id == self.serial => self.apps = apps,
                Event::Network(id, completed_action, result) if id == self.serial => match result {
                    Ok(s) => {
                        self.state.extras.wifi_enabled = Some(s.enabled);
                        self.state.extras.wifi_name =
                            s.networks.iter().find(|n| n.active).map(|n| n.ssid.clone());
                        self.state.extras.wifi_signal =
                            s.networks.iter().find(|n| n.active).and_then(|n| n.signal);
                        self.network = Some(s);
                        if completed_action {
                            self.selected_network = None;
                            self.hidden_ssid = None;
                            self.password.clear();
                        }
                        self.error.clear();
                    }
                    Err(e) => self.error = e,
                },
                Event::Launched(id, result) if id == self.serial => match result {
                    Ok(()) => self.close(),
                    Err(e) => self.error = e,
                },
                Event::Session(result) => {
                    self.busy = false;
                    self.confirm_session = None;
                    match result {
                        Ok(()) => self.close(),
                        Err(e) => self.error = e,
                    }
                }
                _ => {}
            }
        }
        if self.preview_previous.as_ref().is_some_and(|(_, at)| {
            at.elapsed() >= config::get().duration("animation.preview_fade_ms")
        }) {
            self.preview_previous = None;
            self.textures
                .retain(|key, _| !key.starts_with("preview-old:"));
        }
        let mut incoming = self.preview_pending.take();
        while let Ok(s) = self.services.preview_events.try_recv() {
            if self
                .panel
                .as_ref()
                .is_some_and(|p| p.kind == Menu::Workspace(s.name.clone()))
            {
                incoming = Some(s);
            }
        }
        if let Some(s) = incoming {
            let switching = self.preview.as_ref().is_some_and(|old| old.name != s.name);
            if !s.ready() || switching && self.preview_previous.is_some() {
                self.preview_pending = Some(s);
            } else {
                if self.preview.is_none()
                    && let Some(panel) = &mut self.panel
                {
                    panel.opened = Instant::now();
                }
                if switching {
                    let old = self.preview.take().unwrap();
                    for i in 0..old.windows.len() {
                        if let Some(texture) = self.textures.remove(&format!("preview:{i}")) {
                            self.textures.insert(format!("preview-old:{i}"), texture);
                        }
                    }
                    self.preview_previous = Some((old, Instant::now()));
                }
                for (i, w) in s.windows.iter().enumerate() {
                    if let Some(pixels) = &w.pixels {
                        if self
                            .preview
                            .as_ref()
                            .and_then(|s| s.windows.get(i))
                            .and_then(|w| w.pixels.as_ref())
                            .is_some_and(|old| Arc::ptr_eq(old, pixels))
                        {
                            continue;
                        }
                        let key = format!("preview:{i}");
                        let image = egui::ColorImage::from_rgba_premultiplied(
                            [pixels.width() as usize, pixels.height() as usize],
                            pixels.data(),
                        );
                        // The raster cache keys meshes by texture ID, not changed pixels.
                        // A fresh ID invalidates just this image; other UI meshes stay cached.
                        self.textures.insert(
                            key.clone(),
                            self.ctx
                                .load_texture(&key, image, egui::TextureOptions::LINEAR),
                        );
                    }
                }
                self.ctx.request_repaint();
                self.preview = Some(s);
            }
        }
        let output = self
            .audio
            .active()
            .filter(|output| {
                !output.muted && self.audio.streams.iter().any(|s| !s.corked && !s.muted)
            })
            .map(|output| output.name.clone());
        if output != self.tray_output {
            self.tray_meter.capture = None;
            *self.tray_meter.levels.lock().unwrap() = [0.; 7];
            self.tray_meter.retry = Instant::now();
            self.tray_output = output;
        }
        if self
            .tray_meter
            .capture
            .as_mut()
            .is_some_and(|c| !c.is_running())
        {
            self.tray_meter.capture = None;
        }
        if self.tray_output.is_some()
            && self.tray_meter.capture.is_none()
            && Instant::now() >= self.tray_meter.retry
        {
            self.tray_meter.retry =
                Instant::now() + config::get().duration("intervals.meter_retry_ms");
            self.tray_meter.capture =
                audio_meter::Capture::start_output(self.tray_meter.levels.clone()).ok();
        }
        if self.tray_meter.update() {
            self.ctx.request_repaint();
        }
        if self
            .panel
            .as_ref()
            .is_some_and(|p| p.kind == Menu::Sound && p.closing.is_none())
        {
            self.meters.retain(|id, _| {
                self.audio
                    .streams
                    .iter()
                    .any(|s| s.index == *id && !s.corked)
            });
            for stream in &self.audio.streams {
                if !stream.corked {
                    let meter = self.meters.entry(stream.index).or_insert_with(|| Meter {
                        levels: Arc::new(Mutex::new([0.; 7])),
                        capture: None,
                        retry: Instant::now(),
                        displayed: [0.; 7],
                    });
                    if meter.capture.as_mut().is_some_and(|c| !c.is_running()) {
                        meter.capture = None;
                    }
                    if meter.capture.is_none() && Instant::now() >= meter.retry {
                        meter.retry =
                            Instant::now() + config::get().duration("intervals.meter_retry_ms");
                        meter.capture =
                            audio_meter::Capture::start(meter.levels.clone(), stream.index).ok();
                    }
                    if meter.update() {
                        self.ctx.request_repaint();
                    }
                }
            }
        }
    }
    fn icon(&mut self, icon: Icon) -> egui::TextureId {
        self.icon_at_size(icon, config::get().number("appearance.icon_size"))
    }
    fn icon_at_size(&mut self, icon: Icon, size: f32) -> egui::TextureId {
        let pixels = (size * self.ctx.pixels_per_point() * 2.).round().max(1.) as u32;
        let key = format!(
            "icon:{}:{pixels}",
            match icon {
                Icon::Volume(false) => 0,
                Icon::Volume(true) => 1,
                Icon::Music => 2,
                Icon::Play => 3,
                Icon::Pause => 4,
                Icon::Previous => 5,
                Icon::Next => 6,
                Icon::Wifi(signal) =>
                    10 + signal
                        .map(|v| [1, 33, 66].into_iter().filter(|n| v >= *n).count())
                        .unwrap_or(0),
                Icon::Charging => 8,
                Icon::Cpu => 9,
                Icon::Bluetooth => 14,
                Icon::Apps => 15,
                Icon::Logout => 16,
                Icon::Lock => 22,
                Icon::Reboot => 23,
                Icon::Power => 24,
                Icon::Ethernet => 25,
                Icon::Link => 17,
                Icon::Unlink => 18,
                Icon::Back => 19,
                Icon::Eye(false) => 20,
                Icon::Eye(true) => 21,
            }
        );
        let ctx = &self.ctx;
        self.textures
            .entry(key.clone())
            .or_insert_with(|| {
                let color_key = if self.dark {
                    "appearance.icon_color_dark"
                } else {
                    "appearance.icon_color_light"
                };
                let c = u32::from_str_radix(&config::get().text(color_key)[1..], 16).unwrap();
                let p = crate::icons::rasterize(
                    icon,
                    pixels,
                    tiny_skia::Color::from_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255),
                );
                ctx.load_texture(
                    &key,
                    egui::ColorImage::from_rgba_premultiplied(
                        [pixels as usize, pixels as usize],
                        p.data(),
                    ),
                    egui::TextureOptions::LINEAR,
                )
            })
            .id()
    }
    fn application_icon(&mut self, name: &str, size: f32) -> Option<egui::TextureId> {
        let pixels = (size * self.ctx.pixels_per_point()).round().max(1.) as u32;
        let key = format!("app-icon:{name}:{pixels}");
        if let Some(texture) = self.textures.get(&key) {
            return Some(texture.id());
        }
        if let Some(icon) = self.app_icon_pixels.get(name) {
            let texture = self.ctx.load_texture(
                &key,
                application_icons::raster(icon, pixels),
                egui::TextureOptions::LINEAR,
            );
            let id = texture.id();
            self.textures.insert(key, texture);
            return Some(id);
        }
        if !name.is_empty() && self.missing_icons.insert(name.into()) {
            let _ = self.app_icon_requests.send(name.into());
        }
        None
    }
    fn app_icon(&mut self, name: &str, size: f32) -> egui::TextureId {
        self.application_icon(name, size)
            .unwrap_or_else(|| self.icon_at_size(Icon::Music, size))
    }
    fn workspace_apps(&self, name: &str) -> Vec<String> {
        self.state
            .window_details
            .values()
            .flatten()
            .filter(|window| window.workspace == name && !window.application.is_empty())
            .map(|window| window.application.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn playback_button(&mut self, ui: &mut egui::Ui, icon: Icon, enabled: bool) -> bool {
        ui.add_enabled_ui(enabled, |ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(28., 28.), egui::Sense::click());
            let highlighted = enabled && (response.hovered() || response.has_focus());
            let hover = ui
                .ctx()
                .animate_bool_with_time(response.id, highlighted, 0.1);
            ui.painter().rect_filled(
                rect.shrink(1.),
                6,
                config::get()
                    .color("button_hover", self.dark)
                    .gamma_multiply(if response.is_pointer_button_down_on() {
                        1.
                    } else {
                        hover * 0.75
                    }),
            );
            ui.painter().image(
                self.icon_at_size(icon, 16.),
                Rect::from_center_size(rect.center(), vec2(16., 16.)),
                Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                Color32::WHITE,
            );
            let label = match icon {
                Icon::Previous => "Previous",
                Icon::Next => "Next",
                Icon::Pause => "Pause",
                _ => "Play",
            };
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
            });
            response.on_hover_text(label).clicked()
        })
        .inner
    }
    fn icon_button(&mut self, ui: &mut egui::Ui, icon: Icon, enabled: bool) -> bool {
        let size = config::get()
            .number("layout.widget_min_width")
            .max(config::get().number("layout.widget_min_height"));
        ui.add_enabled_ui(enabled, |ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(size, size), egui::Sense::click());
            let hover = ui.ctx().animate_bool_with_time(
                response.id,
                response.hovered() || response.has_focus(),
                0.12,
            );
            let base = config::get().color("button", self.dark);
            let highlight = config::get().color("button_hover", self.dark);
            let fill = egui::Color32::from(
                egui::Rgba::from(base) * (1. - hover) + egui::Rgba::from(highlight) * hover,
            );
            ui.painter().circle_filled(
                rect.center(),
                size / 2.,
                if response.is_pointer_button_down_on() {
                    fill.gamma_multiply(0.9)
                } else {
                    fill
                },
            );
            ui.painter().image(
                self.icon_at_size(icon, 14.),
                Rect::from_center_size(rect.center(), vec2(14., 14.)),
                Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                Color32::WHITE,
            );
            response.clicked()
        })
        .inner
    }
    fn paint_tray(
        &mut self,
        ui: &mut egui::Ui,
        targets: &mut Vec<(Rect, Menu)>,
        right: f32,
        minimum: f32,
        slot: f32,
    ) -> f32 {
        let gap = config::get().number("layout.group_gap");
        let stride = slot + gap;
        let available = ((right - minimum + gap).max(0.) / stride) as usize;
        let items: Vec<_> = self.tray_items.iter().take(available).cloned().collect();
        let left = right - items.len() as f32 * stride + if items.is_empty() { 0. } else { gap };
        for (index, item) in items.iter().enumerate() {
            let rect = Rect::from_min_size(
                pos2(left + index as f32 * stride, 0.),
                vec2(slot, config::get().number("layout.bar_height")),
            );
            let response = ui.interact(
                rect,
                egui::Id::new(("tray", &item.id)),
                egui::Sense::click(),
            );
            let opened = self
                .panel
                .as_ref()
                .is_some_and(|p| p.kind == Menu::Tray(item.id.clone()) && p.closing.is_none());
            let fill = config::get().color(
                if response.hovered() || response.has_focus() || opened {
                    "button_hover"
                } else {
                    "button"
                },
                self.dark,
            );
            let button = rect.shrink2(vec2(2., 3.));
            ui.painter().rect_filled(
                button,
                3,
                if response.is_pointer_button_down_on() {
                    fill.gamma_multiply(0.9)
                } else {
                    fill
                },
            );
            ui.painter().rect_stroke(
                button,
                3,
                egui::Stroke::new(1_f32, config::get().color("separator", self.dark)),
                egui::StrokeKind::Inside,
            );
            let separator = rect.right() + gap / 2.;
            let half_height = config::get().number("layout.separator_height") / 2.;
            ui.painter().line_segment(
                [
                    pos2(separator, rect.center().y - half_height),
                    pos2(separator, rect.center().y + half_height),
                ],
                egui::Stroke::new(1_f32, config::get().color("separator", self.dark)),
            );
            let texture = if let Some(icon) = &item.icon {
                let key = format!("tray-icon:{}", item.id);
                self.textures
                    .entry(key.clone())
                    .or_insert_with(|| {
                        self.ctx.load_texture(
                            key,
                            egui::ColorImage::from_rgba_unmultiplied(icon.size, &icon.rgba),
                            egui::TextureOptions::LINEAR,
                        )
                    })
                    .id()
            } else {
                self.icon(Icon::Apps)
            };
            let size = config::get().number("appearance.icon_size");
            ui.painter().image(
                texture,
                Rect::from_center_size(rect.center(), vec2(size, size)),
                Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                Color32::WHITE,
            );
            if item.menu.is_some() {
                targets.push((rect, Menu::Tray(item.id.clone())));
            }
            if response.secondary_clicked() || (response.clicked() && item.menu_only) {
                if item.menu.is_some() {
                    self.open(Menu::Tray(item.id.clone()), rect.center().x, true);
                } else {
                    self.finish_close();
                    let _ = self.services.tray.send(crate::tray::Request::Activate(
                        item.id.clone(),
                        "ContextMenu",
                        rect.center().x as i32 + self.tray_origin.0,
                        rect.center().y as i32 + self.tray_origin.1,
                    ));
                }
            } else if response.clicked() || response.middle_clicked() {
                self.finish_close();
                self.blocked_hover = Some(Menu::Tray(item.id.clone()));
                let method = if response.middle_clicked() {
                    "SecondaryActivate"
                } else {
                    "Activate"
                };
                let _ = self.services.tray.send(crate::tray::Request::Activate(
                    item.id.clone(),
                    method,
                    rect.center().x as i32 + self.tray_origin.0,
                    rect.center().y as i32 + self.tray_origin.1,
                ));
            }
            if response.hovered() {
                let scroll = ui.input(|input| {
                    input
                        .raw
                        .events
                        .iter()
                        .filter_map(|event| {
                            if let egui::Event::MouseWheel { delta, .. } = event {
                                Some(*delta)
                            } else {
                                None
                            }
                        })
                        .fold(egui::Vec2::ZERO, |total, delta| total + delta)
                });
                let (delta, orientation) = if scroll.y != 0. {
                    (scroll.y, "vertical")
                } else {
                    (scroll.x, "horizontal")
                };
                if delta != 0. {
                    let _ = self.services.tray.send(crate::tray::Request::Scroll(
                        item.id.clone(),
                        delta as i32,
                        orientation,
                    ));
                }
            }
        }
        left
    }
    fn tray_menu(&mut self, ui: &mut egui::Ui, id: &str) {
        let rows = self.tray_rows.clone();
        if let Some(row) = tray_menu_rows(ui, &rows) {
            let _ = self
                .services
                .tray
                .send(crate::tray::Request::Click(id.into(), row));
            self.close();
        }
    }
    fn paint_bar(&mut self, ui: &mut egui::Ui, width: f32) -> Vec<(Rect, Menu)> {
        let gap = config::get().number("layout.group_gap");
        let margin = config::get().number("layout.bar_margin");
        let slot = config::get().number("layout.icon_slot_width");
        let cpu_width = config::get().number("layout.cpu_width");
        let sound_width = config::get().number("layout.sound_tray_width");
        let session_width = config::get().number("layout.session_width");
        let apps_width = config::get().number("layout.apps_width");
        let number_offset = config::get().number("layout.workspace_number_offset");
        let stack_icon_size = 14.;
        let stack_stride = 6.;
        let icon_start = number_offset + 8.;
        let speaker_size = config::get().number("layout.workspace_audio_size");
        let speaker_offset =
            icon_start + stack_icon_size + 2. * stack_stride + 4. + speaker_size / 2.;
        let workspace_width =
            speaker_offset + speaker_size / 2. + config::get().number("layout.button_padding_x");
        let workspace_gap = config::get().number("layout.workspace_gap");
        let center_y = config::get().number("layout.bar_height") / 2.;
        let bg = config::get().color("bar", self.dark);
        ui.painter().rect_filled(
            Rect::from_min_size(
                Pos2::ZERO,
                vec2(width, config::get().number("layout.bar_height")),
            ),
            0,
            bg,
        );
        let mut targets = vec![];
        let button = |targets: &mut Vec<(Rect, Menu)>,
                      this: &mut Self,
                      ui: &mut egui::Ui,
                      rect: Rect,
                      icon: Icon,
                      content: bar_button::Content,
                      kind: Menu| {
            let response = ui.interact(rect, egui::Id::new(("bar", &kind)), egui::Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(0., 2.)),
                    config::get().number("layout.workspace_corner_radius") as u8,
                    config::get().color("hover", this.dark),
                );
            }
            let layout = bar_button::Layout::new(
                rect,
                content,
                slot,
                config::get().number("appearance.icon_size"),
            );
            layout.paint_icon(ui.painter(), this.icon(icon));
            if response.clicked() {
                this.open(kind.clone(), rect.center().x, true);
            }
            targets.push((rect, kind));
            layout
        };
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(margin, 0.),
                vec2(session_width, config::get().number("layout.bar_height")),
            ),
            Icon::Logout,
            bar_button::Content::Icon,
            Menu::Session,
        );
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(margin + session_width + gap, 0.),
                vec2(apps_width, config::get().number("layout.bar_height")),
            ),
            Icon::Apps,
            bar_button::Content::Icon,
            Menu::Apps,
        );
        self.workspace_icon_fades.retain(|name, _| {
            self.state
                .workspaces
                .iter()
                .any(|workspace| workspace.name == *name)
        });
        let mut x = margin + session_width + gap + apps_width + gap;
        for w in self.state.workspaces.clone() {
            let r = Rect::from_min_size(
                pos2(x, 0.),
                vec2(workspace_width, config::get().number("layout.bar_height")),
            );
            let response = ui.interact(
                r,
                egui::Id::new(("workspace", &w.name)),
                egui::Sense::click(),
            );
            ui.painter().rect_stroke(
                r.shrink2(vec2(1., 3.)),
                config::get().number("layout.workspace_corner_radius") as u8,
                egui::Stroke::new(
                    config::get().number("layout.workspace_outline_width"),
                    config::get().color("workspace_border", self.dark),
                ),
                egui::StrokeKind::Inside,
            );
            if w.focused || response.hovered() {
                ui.painter().rect_filled(
                    r.shrink2(vec2(1., 3.)),
                    config::get().number("layout.workspace_corner_radius") as u8,
                    config::get().color("workspace_active", self.dark),
                );
            }
            let color = if w.urgent {
                config::get().color("urgent", self.dark)
            } else {
                ui.visuals().text_color()
            };
            let apps = self.workspace_apps(&w.name);
            let icons: Vec<_> = apps
                .iter()
                .filter_map(|app| self.application_icon(app, stack_icon_size))
                .take(3)
                .collect();
            ui.painter().text(
                pos2(
                    x + config::get().number("layout.workspace_number_offset"),
                    center_y,
                ),
                egui::Align2::CENTER_CENTER,
                &w.name,
                egui::FontId::proportional(config::get().number("appearance.font_size")),
                color,
            );
            let fade = self.workspace_icon_fades.entry(w.name.clone()).or_default();
            let layers = fade.update(
                icons,
                Instant::now(),
                config::get().duration("animation.icon_fade_ms"),
            );
            if fade.animating() {
                self.ctx
                    .request_repaint_after(config::get().duration("animation.frame_ms"));
            }
            for (icons, opacity) in layers {
                for (index, icon) in icons.into_iter().enumerate().rev() {
                    ui.painter().image(
                        icon,
                        Rect::from_center_size(
                            pos2(
                                x + icon_start + stack_icon_size / 2. + index as f32 * stack_stride,
                                center_y,
                            ),
                            vec2(stack_icon_size, stack_icon_size),
                        ),
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE.gamma_multiply(opacity),
                    );
                }
            }
            let amount = self.ctx.animate_bool_with_time(
                egui::Id::new(("audible", &w.name)),
                w.audible,
                config::get()
                    .duration("animation.speaker_fade_ms")
                    .as_secs_f32(),
            );
            if amount > 0. {
                let r = Rect::from_center_size(
                    pos2(x + speaker_offset, center_y),
                    vec2(
                        config::get().number("layout.workspace_audio_size"),
                        config::get().number("layout.workspace_audio_size"),
                    ) * (config::get().number("animation.speaker_min_scale")
                        + (1. - config::get().number("animation.speaker_min_scale")) * amount),
                );
                ui.painter().image(
                    self.icon(Icon::Volume(false)),
                    r,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    config::get()
                        .color("audio", self.dark)
                        .gamma_multiply(amount),
                );
            }
            if response.clicked() {
                let _ = self
                    .services
                    .status
                    .send(status::Update::Focus(w.name.clone()));
                self.close();
            }
            targets.push((r, Menu::Workspace(w.name)));
            x += workspace_width + workspace_gap;
        }
        let clock = chrono::Local::now()
            .format(config::get().text("appearance.clock_format"))
            .to_string();
        let clock_width = ui
            .painter()
            .layout_no_wrap(
                clock.clone(),
                egui::FontId::proportional(config::get().number("appearance.font_size")),
                ui.visuals().text_color(),
            )
            .size()
            .x;
        ui.painter().text(
            pos2(width - margin, center_y),
            egui::Align2::RIGHT_CENTER,
            clock,
            egui::FontId::proportional(config::get().number("appearance.font_size")),
            ui.visuals().text_color(),
        );
        let right = width - margin - clock_width - gap;
        let battery_icon_extent = config::get().number("layout.battery_icon_width") + 3.;
        let icon_padding = ((slot - config::get().number("appearance.icon_size")) / 2.).max(0.);
        let battery_width = if self.state.battery.is_some() {
            battery_icon_extent + 2. * icon_padding
        } else {
            0.
        };
        let battery_left = right - battery_width + icon_padding;
        if let Some((percent, charging)) = self.state.battery {
            let rect = Rect::from_min_size(
                pos2(
                    battery_left,
                    center_y - config::get().number("layout.battery_icon_height") / 2.,
                ),
                vec2(
                    config::get().number("layout.battery_icon_width"),
                    config::get().number("layout.battery_icon_height"),
                ),
            );
            paint_battery(ui, rect, if charging { 0 } else { percent });
            if charging {
                ui.painter().image(
                    self.icon(Icon::Charging),
                    rect.shrink(1.),
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
            }
        }
        let wifi_x = right - battery_width - gap - slot;
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(wifi_x, 0.),
                vec2(slot, config::get().number("layout.bar_height")),
            ),
            if self.state.ethernet {
                Icon::Ethernet
            } else {
                Icon::Wifi(self.state.extras.wifi_signal)
            },
            bar_button::Content::Icon,
            Menu::Wifi,
        );
        let bluetooth_x = wifi_x - slot - gap;
        let sound_x = bluetooth_x - sound_width - gap;
        let sound_layout = button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(sound_x, 0.),
                vec2(sound_width, config::get().number("layout.bar_height")),
            ),
            Icon::Volume(self.audio.active().is_some_and(|o| o.muted)),
            bar_button::Content::IconAndPlot,
            Menu::Sound,
        );
        if let Some((painter, spectrum)) = sound_layout.plot(ui.painter()) {
            paint_equalizer(&painter, spectrum, &self.tray_meter.displayed);
        }
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(bluetooth_x, 0.),
                vec2(slot, config::get().number("layout.bar_height")),
            ),
            Icon::Bluetooth,
            bar_button::Content::Icon,
            Menu::Bluetooth,
        );
        let cpu_x = sound_x - cpu_width - gap;
        let cpu_layout = button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(cpu_x, 0.),
                vec2(cpu_width, config::get().number("layout.bar_height")),
            ),
            Icon::Cpu,
            bar_button::Content::IconAndPlot,
            Menu::Cpu,
        );
        if let Some((painter, chart)) = cpu_layout.plot(ui.painter()) {
            let color = ui.visuals().text_color();
            painter.line_segment(
                [chart.left_bottom(), chart.right_bottom()],
                egui::Stroke::new(1_f32, color.gamma_multiply(0.2)),
            );
            let points: Vec<_> = self
                .cpu_history
                .iter()
                .enumerate()
                .map(|(i, percent)| {
                    let usage = percent.clamp(0., 100.) / 100.;
                    let point = pos2(
                        chart.right()
                            - (self.cpu_history.len() - 1 - i) as f32 * chart.width() / 31.,
                        chart.bottom() - usage * chart.height(),
                    );
                    (point, usage)
                })
                .collect();
            // Sample the hue ramp at physical-pixel intervals so GPU RGB
            // interpolation cannot desaturate a long green-to-red segment.
            let mut colored_points = Vec::new();
            for pair in points.windows(2) {
                let steps = ((pair[1].0.x - pair[0].0.x).abs() * painter.ctx().pixels_per_point())
                    .ceil()
                    .max(1.) as usize;
                for step in 0..steps {
                    let t = step as f32 / steps as f32;
                    colored_points.push((
                        pair[0].0 + (pair[1].0 - pair[0].0) * t,
                        plot_color(pair[0].1 + (pair[1].1 - pair[0].1) * t),
                    ));
                }
            }
            if let Some((point, usage)) = points.last() {
                colored_points.push((*point, plot_color(*usage)));
            }
            let points = colored_points;
            if points.len() > 1 {
                let mut fill = egui::Mesh::default();
                for (point, color) in &points {
                    let color = color.gamma_multiply(0.55);
                    fill.colored_vertex(*point, color);
                    fill.colored_vertex(pos2(point.x, chart.bottom()), color);
                }
                for i in 0..points.len() - 1 {
                    let index = i as u32 * 2;
                    fill.add_triangle(index, index + 1, index + 2);
                    fill.add_triangle(index + 1, index + 3, index + 2);
                }
                painter.add(egui::Shape::mesh(fill));
                for pair in points.windows(2) {
                    painter
                        .line_segment([pair[0].0, pair[1].0], egui::Stroke::new(1_f32, pair[1].1));
                }
            } else if let Some((point, color)) = points.first() {
                painter.line_segment(
                    [*point, pos2(point.x, chart.bottom())],
                    egui::Stroke::new(1_f32, color.gamma_multiply(0.55)),
                );
                painter.circle_filled(*point, 1., *color);
            }
        }
        for separator in [
            cpu_x + cpu_width + gap / 2.,
            bluetooth_x + slot + gap / 2.,
            sound_x + sound_width + gap / 2.,
            wifi_x + slot + gap / 2.,
            right + gap / 2.,
        ] {
            ui.painter().line_segment(
                [
                    pos2(
                        separator,
                        center_y - config::get().number("layout.separator_height") / 2.,
                    ),
                    pos2(
                        separator,
                        center_y + config::get().number("layout.separator_height") / 2.,
                    ),
                ],
                egui::Stroke::new(1_f32, config::get().color("separator", self.dark)),
            );
        }
        let tray_left = self.paint_tray(ui, &mut targets, cpu_x - gap, x + 68., slot);
        let title_bounds = Rect::from_min_max(
            pos2(x + 8., 0.),
            pos2(tray_left, config::get().number("layout.bar_height")),
        );
        if title_bounds.width() > 60. {
            let painter = ui.painter().with_clip_rect(title_bounds);
            let center = pos2(
                (width / 2.).clamp(title_bounds.min.x, title_bounds.max.x),
                center_y,
            );
            let app = self.state.app.clone();
            let target = self
                .application_icon(&app, 18.)
                .map(AppLabel::Icon)
                .unwrap_or(AppLabel::Name(app));
            let layers = self.app_icon_fade.update(
                target,
                Instant::now(),
                config::get().duration("animation.icon_fade_ms"),
            );
            if self.app_icon_fade.animating() {
                self.ctx
                    .request_repaint_after(config::get().duration("animation.frame_ms"));
            }
            for (label, opacity) in layers {
                match label {
                    AppLabel::Icon(icon) => {
                        painter.image(
                            icon,
                            Rect::from_center_size(center, vec2(18., 18.)),
                            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                            Color32::WHITE.gamma_multiply(opacity),
                        );
                    }
                    AppLabel::Name(name) => {
                        painter.text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            name,
                            egui::FontId::proportional(
                                config::get().number("appearance.font_size"),
                            ),
                            ui.visuals().text_color().gamma_multiply(opacity),
                        );
                    }
                }
            }
        }
        targets
    }
    fn frame(&mut self, ctx: &egui::Context, width: f32, height: f32) {
        if self.pending_menu.is_some()
            && self.panel.as_ref().is_none_or(|panel| {
                panel.closing.is_some_and(|at| {
                    at.elapsed() >= config::get().duration("animation.popup_fade_ms")
                })
            })
        {
            let (kind, x, clicked) = self.pending_menu.take().unwrap();
            self.finish_close();
            self.open(kind, x, clicked);
        }
        let bounds = workspace_preview::preview_bounds([width, height]);
        if self.preview_size != bounds {
            self.preview_size = bounds;
            if let Some(panel) = &self.panel
                && panel.closing.is_none()
                && let Menu::Workspace(name) = &panel.kind
            {
                let _ = self.services.preview.send(Some((
                    name.clone(),
                    ctx.pixels_per_point(),
                    bounds,
                )));
            }
        }
        let now = Instant::now();
        let mut targets = vec![];
        egui::Area::new(egui::Id::new("bar"))
            .fixed_pos(Pos2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(vec2(width, config::get().number("layout.bar_height")));
                targets = self.paint_bar(ui, width);
            });
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let candidate = pointer.and_then(|p| targets.iter().find(|(r, _)| r.contains(p)).cloned());
        if let Some((rect, kind)) = candidate {
            if self.blocked_hover.as_ref() != Some(&kind) {
                self.blocked_hover = None;
                if self.hover.as_ref().is_none_or(|(k, _)| *k != kind) {
                    self.hover = Some((kind.clone(), now));
                }
                if self.panel.as_ref().is_some_and(|p| p.kind != kind)
                    || self.hover.as_ref().is_some_and(|(_, t)| {
                        now.duration_since(*t) >= config::get().duration("animation.hover_ms")
                    })
                {
                    self.open(kind, rect.center().x, false);
                } else if let Some((_, since)) = &self.hover {
                    ctx.request_repaint_after(
                        config::get()
                            .duration("animation.hover_ms")
                            .saturating_sub(now.duration_since(*since)),
                    );
                }
            }
        } else {
            self.hover = None;
            self.blocked_hover = None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close();
        }
        let Some(panel) = &self.panel else {
            return;
        };
        let kind = panel.kind.clone();
        if kind == Menu::Bluetooth && self.bluetooth.is_none() && self.error.is_empty() {
            return;
        }
        if matches!(kind, Menu::Workspace(_)) && self.preview.is_none() {
            let panel = self.panel.as_mut().unwrap();
            panel.dismissal.pointer_inside(
                pointer.is_some_and(|p| p.y < config::get().number("layout.bar_height")),
                now,
            );
            if panel.dismissal.progress(now) >= 1. {
                self.finish_close();
            } else if let Some(delay) = panel.dismissal.repaint_after(now) {
                ctx.request_repaint_after(delay);
            }
            return;
        }
        let w = match kind {
            Menu::Workspace(_) => {
                self.preview_size[0] + 2. * config::get().number("layout.popup_padding")
            }
            Menu::Sound => config::get().number("layout.sound_width"),
            _ => config::get().number("layout.popup_width"),
        };
        let x = (panel.x - w / 2.).clamp(
            config::get().number("layout.popup_screen_margin"),
            (width - w - config::get().number("layout.popup_screen_margin"))
                .max(config::get().number("layout.popup_screen_margin")),
        );
        let opacity = panel
            .closing
            .map(|t| {
                1. - now.duration_since(t).as_secs_f32()
                    / config::get()
                        .duration("animation.popup_fade_ms")
                        .as_secs_f32()
            })
            .unwrap_or_else(|| {
                (now.duration_since(panel.opened).as_secs_f32()
                    / if matches!(kind, Menu::Workspace(_)) {
                        config::get()
                            .duration("animation.preview_fade_ms")
                            .as_secs_f32()
                    } else {
                        config::get()
                            .duration("animation.popup_fade_ms")
                            .as_secs_f32()
                    })
                .min(1.)
            });
        let opacity = opacity.clamp(0., 1.);
        let opacity = opacity * opacity * (3. - 2. * opacity);
        if opacity <= 0. && panel.closing.is_some() {
            self.finish_close();
            return;
        }
        if opacity < 1. || panel.closing.is_some() {
            ctx.request_repaint_after(config::get().duration("animation.frame_ms"));
        }
        let min_height = match &kind {
            Menu::Tray(_) => {
                self.tray_rows.iter().filter(|row| row.visible).count() as f32
                    * (config::get().number("layout.row_height")
                        + config::get().number("layout.item_gap_y"))
            }
            Menu::Wifi => {
                if self.hidden_ssid.is_some() {
                    5. * config::get().number("layout.control_height")
                } else if self.selected_network.is_some() {
                    4. * config::get().number("layout.control_height")
                } else {
                    config::get().number("layout.control_height")
                        + self.network.as_ref().map_or(1, |s| s.networks.len().max(1)) as f32
                            * (config::get().number("layout.control_height")
                                + 12.
                                + config::get().number("layout.item_gap_y"))
                }
            }
            Menu::Bluetooth => {
                let row = config::get().number("layout.control_height") + 12.;
                self.bluetooth.as_ref().map_or(3. * row, |s| {
                    if s.prompt.is_some() {
                        return 4. * row;
                    }
                    let groups = [
                        s.devices.iter().any(|d| d.connected || d.paired),
                        s.devices.iter().any(|d| !d.connected && !d.paired),
                    ]
                    .into_iter()
                    .filter(|present| *present)
                    .count();
                    (1 + groups
                        + s.devices.len()
                        + 3 * usize::from(s.prompt.is_some())
                        + 2 * usize::from(s.action_error.is_some())
                        + usize::from(s.busy.is_some())) as f32
                        * row
                })
            }
            Menu::Cpu => {
                let query = self.query.trim().to_lowercase();
                2. * config::get().number("layout.row_height")
                    + (self
                        .cpu
                        .processes
                        .iter()
                        .filter(|p| p.matches_query(&query))
                        .count()
                        .min(config::get().number("processes.rows") as usize)
                        + 1) as f32
                        * (config::get().number("layout.process_row_height")
                            + config::get().number("layout.item_gap_y"))
            }
            Menu::Apps => {
                let query = self.query.to_lowercase();
                config::get().number("layout.bar_height")
                    + (self
                        .apps
                        .iter()
                        .filter(|a| a.search.contains(&query))
                        .count() as f32
                        * config::get().number("layout.launcher_row_height"))
                    .min(config::get().number("layout.launcher_max_height"))
            }
            Menu::Workspace(_) => {
                config::get().number("preview.title_height") + self.preview_size[1]
            }
            _ => 0.,
        };
        let screen_max_height = (height
            - config::get().number("layout.bar_height")
            - config::get().number("layout.popup_bottom_margin")
            - 2. * config::get().number("layout.popup_padding"))
        .max(1.);
        let max_height = if kind == Menu::Sound {
            config::get()
                .number("layout.sound_max_height")
                .min(screen_max_height)
        } else if kind == Menu::Bluetooth {
            config::get()
                .number("layout.bluetooth_max_height")
                .min(screen_max_height)
        } else {
            screen_max_height.max(config::get().number("layout.popup_min_height"))
        };
        let shown =
            egui::Area::new(egui::Id::new(("panel", self.serial)))
                .fixed_pos(pos2(x, config::get().number("layout.bar_height")))
                .order(egui::Order::Foreground)
                .fade_in(false)
                .show(ctx, |ui| {
                    ui.set_opacity(opacity);
                    ui.visuals_mut().override_text_color =
                        Some(config::get().color("popup_text", self.dark));
                    ui.visuals_mut().widgets.hovered.bg_stroke = egui::Stroke::NONE;
                    ui.visuals_mut().widgets.active.bg_stroke = egui::Stroke::NONE;
                    ui.set_width(w);
                    egui::Frame::new()
                        .fill(config::get().color("popup", self.dark).gamma_multiply(
                            config::get().number("appearance.popup_opacity_fraction"),
                        ))
                        .corner_radius(config::get().number("layout.popup_corner_radius") as u8)
                        .inner_margin(config::get().number("layout.popup_padding") as i8)
                        .show(ui, |ui| {
                            ui.set_width(w - 2. * config::get().number("layout.popup_padding"));
                            if matches!(
                                kind,
                                Menu::Sound
                                    | Menu::Wifi
                                    | Menu::Bluetooth
                                    | Menu::Session
                                    | Menu::Tray(_)
                            ) {
                                // Areas remember their previous size. Allow the drawer to grow
                                // when streams/devices arrive, rather than scrolling at that size.
                                ui.set_max_height(max_height);
                            }
                            let content_max_height = max_height;
                            // Wi-Fi scrolls its network list independently so its
                            // radio switch and hidden-network action remain visible.
                            if matches!(kind, Menu::Wifi | Menu::Bluetooth) {
                                self.connection_drawer(ui, &kind, max_height, now);
                                return;
                            }
                            egui::ScrollArea::vertical()
                                .min_scrolled_height(min_height.min(content_max_height))
                                .max_height(content_max_height)
                                .show(ui, |ui| {
                                    match kind {
                                        Menu::Sound => self.sound(ui),
                                        Menu::Apps => self.launcher(ui),
                                        Menu::Wifi => self.wifi(ui),
                                        Menu::Bluetooth => self.bluetooth(ui),
                                        Menu::Session => self.session(ui),
                                        Menu::Cpu => self.processes(ui),
                                        Menu::Workspace(_) => self.workspace(ui),
                                        Menu::Tray(ref id) => self.tray_menu(ui, id),
                                    }
                                    if !self.error.is_empty() {
                                        ui.add(egui::Label::new(&self.error).wrap());
                                    }
                                });
                        });
                });
        self.panel_rect = shown.response.rect;
        let inside = pointer.is_some_and(|p| {
            p.y < config::get().number("layout.bar_height") || self.panel_rect.contains(p)
        });
        let pinned = (!self.query.is_empty() && kind != Menu::Cpu)
            || self.selected_network.is_some()
            || self.hidden_ssid.is_some()
            || (kind == Menu::Bluetooth
                && self.bluetooth.as_ref().is_some_and(|s| s.busy.is_some()))
            || self.confirm_session.is_some()
            || self.busy
            || ctx.egui_is_using_pointer();
        if !inside && ctx.input(|i| i.pointer.any_pressed()) {
            self.close();
        }
        if let Some(panel) = &mut self.panel {
            panel.dismissal.set_pinned(pinned, now);
            panel.dismissal.pointer_inside(inside, now);
            if let Some(delay) = panel.dismissal.repaint_after(now) {
                ctx.request_repaint_after(delay);
            }
            if panel.dismissal.progress(now) >= 1. {
                self.finish_close();
            } else if panel.dismissal.progress(now) > 0. {
                let elapsed = panel.dismissal.progress(now)
                    * config::get()
                        .duration("animation.popup_fade_ms")
                        .as_secs_f32();
                panel
                    .closing
                    .get_or_insert(now - Duration::from_secs_f32(elapsed));
            }
        }
    }
    fn credential_entry(&self) -> bool {
        self.panel.as_ref().is_some_and(|p| {
            p.closing.is_none()
                && match p.kind {
                    Menu::Wifi => self.selected_network.is_some() || self.hidden_ssid.is_some(),
                    Menu::Bluetooth => self.bluetooth.as_ref().is_some_and(|s| s.prompt.is_some()),
                    _ => false,
                }
        })
    }
    fn keyboard(&self) -> bool {
        self.panel.as_ref().is_some_and(|p| {
            p.closing.is_none()
                && (matches!(p.kind, Menu::Apps | Menu::Cpu)
                    || self.selected_network.is_some()
                    || self.hidden_ssid.is_some()
                    || (p.kind == Menu::Bluetooth
                        && self.bluetooth.as_ref().is_some_and(|s| s.busy.is_some()))
                    || p.clicked)
        })
    }
}

fn connection_separator(ui: &mut egui::Ui, dark: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1_f32, config::get().color("separator", dark)),
    );
}
fn connection_row(ui: &mut egui::Ui, key: impl std::hash::Hash) -> (Rect, egui::Response) {
    let (_, rect) = ui.allocate_space(vec2(
        ui.available_width(),
        config::get().number("layout.control_height") + 12.,
    ));
    let name_rect = Rect::from_min_max(pos2(rect.left() + 34., rect.top()), rect.max);
    let response = ui.interact(
        name_rect,
        ui.id().with(("connection-row", key)),
        egui::Sense::click(),
    );
    (rect, response)
}
fn paint_row_name(ui: &egui::Ui, rect: Rect, name: &str, reserved: f32) {
    let color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::simple(
        name.to_owned(),
        egui::FontId::proportional(config::get().number("appearance.font_size")),
        color,
        (rect.width() - reserved - 38.).max(1.),
    );
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left() + 38., rect.center().y - galley.size().y / 2.),
        galley,
        color,
    );
}
fn radio_header(ui: &mut egui::Ui, enabled: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            toggle(ui, enabled, "")
        })
        .inner
    })
    .inner
}
fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, mut response) = ui.allocate_exact_size(vec2(30., 18.), egui::Sense::click());
        if !label.is_empty() {
            response = response.union(ui.add(egui::Label::new(label).sense(egui::Sense::click())));
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
        });
        if response.clicked() {
            *value = !*value;
            response.mark_changed();
        }
        let visuals = ui.style().interact(&response);
        let color = if *value {
            ui.visuals().selection.bg_fill
        } else {
            visuals.bg_fill
        };
        ui.painter().rect_filled(rect, 9, color);
        ui.painter().rect_stroke(
            rect,
            9,
            egui::Stroke::new(1_f32, ui.visuals().weak_text_color()),
            egui::StrokeKind::Inside,
        );
        let t = ui.ctx().animate_bool(response.id, *value);
        ui.painter().circle_filled(
            pos2(rect.left() + 9. + t * 12., rect.center().y),
            6.,
            ui.visuals().text_color(),
        );
        response
    })
    .inner
}
fn paint_signal(ui: &egui::Ui, rect: Rect, strength: Option<u8>) {
    for i in 0..4 {
        let active = strength.is_some_and(|s| s > 0 && usize::from(s) * 4 / 101 >= i);
        let color = ui
            .visuals()
            .text_color()
            .gamma_multiply(if active { 1. } else { 0.2 });
        let height = rect.height() * (i + 1) as f32 / 4.;
        ui.painter().rect_filled(
            Rect::from_min_size(
                pos2(
                    rect.left() + i as f32 * rect.width() / 4.,
                    rect.bottom() - height,
                ),
                vec2(rect.width() / 4. - 1.5, height),
            ),
            1,
            color,
        );
    }
}
fn paint_battery(ui: &egui::Ui, rect: Rect, percent: u8) {
    let color = ui.visuals().text_color();
    ui.painter().rect_stroke(
        rect,
        2,
        egui::Stroke::new(1_f32, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().rect_filled(
        Rect::from_min_size(pos2(rect.right() + 1., rect.center().y - 2.), vec2(2., 4.)),
        1,
        color,
    );
    ui.painter().rect_filled(
        Rect::from_min_size(
            rect.min + vec2(2., 2.),
            vec2(
                (rect.width() - 4.) * percent as f32 / 100.,
                rect.height() - 4.,
            ),
        ),
        1,
        color,
    );
}
fn flat(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(
        egui::Button::new("")
            .left_text(text)
            .frame(false)
            .min_size(vec2(
                ui.available_width(),
                config::get().number("layout.control_height"),
            )),
    )
}

impl Ui {
    fn launcher(&mut self, ui: &mut egui::Ui) {
        let search = add_text_field(
            ui,
            text_field(&mut self.query)
                .hint_text("Search applications")
                .desired_width(f32::INFINITY),
        );
        if self.focus_search {
            search.request_focus();
            self.focus_search = false;
        }
        if search.changed() {
            self.selection = 0;
        }
        let query = self.query.to_lowercase();
        let filtered: Vec<_> = self
            .apps
            .iter()
            .filter(|a| a.search.contains(&query))
            .collect();
        if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.selection = (self.selection + 1).min(filtered.len().saturating_sub(1));
        }
        if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            self.selection = self.selection.saturating_sub(1);
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        egui::ScrollArea::vertical()
            .max_height(config::get().number("layout.launcher_max_height"))
            .show(ui, |ui| {
                for (i, app) in filtered.iter().enumerate() {
                    let response = ui.add(
                        egui::Button::new(&app.name)
                            .frame(i == self.selection)
                            .min_size(vec2(
                                ui.available_width(),
                                config::get().number("layout.control_height"),
                            )),
                    );
                    if response.clicked() || (enter && i == self.selection) {
                        self.services.launch(self.serial, app.path.clone());
                    }
                    if i == self.selection
                        && ui.input(|i| {
                            i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowUp)
                        })
                    {
                        response.scroll_to_me(None);
                    }
                }
            });
    }
    fn connection_view(&self, kind: &Menu) -> motion::View {
        if *kind == Menu::Wifi {
            if self.hidden_ssid.is_some() {
                motion::View::HiddenWifi
            } else if let Some(network) = &self.selected_network {
                motion::View::Wifi(network.bssid.clone())
            } else {
                motion::View::List
            }
        } else if let Some(snapshot) = &self.bluetooth {
            if let Some(prompt) = &snapshot.prompt {
                motion::View::Bluetooth(format!("{:?}:{prompt:?}", snapshot.busy))
            } else {
                motion::View::List
            }
        } else {
            motion::View::List
        }
    }
    fn connection_drawer(&mut self, ui: &mut egui::Ui, kind: &Menu, max_height: f32, now: Instant) {
        let view = self.connection_view(kind);
        let duration = config::get().duration("animation.drawer_transition_ms");
        let (offset, opacity) = self.drawer_motion.navigate(view.clone(), now, duration);
        let opening = *kind == Menu::Bluetooth
            && self.panel.as_ref().is_some_and(|panel| {
                now.saturating_duration_since(panel.opened)
                    < config::get().duration("animation.popup_fade_ms")
            });
        let current_height = if opening {
            None
        } else {
            self.drawer_motion.height(now, duration)
        };
        let origin = ui.cursor().min;
        let width = ui.available_width();
        let visible =
            Rect::from_min_size(origin, vec2(width, current_height.unwrap_or(max_height)));
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            origin + vec2(offset, 0.),
            vec2(width, max_height),
        )));
        content.spacing_mut().scroll.fade.strength = 0.;
        content.set_clip_rect(ui.clip_rect().intersect(visible));
        content.multiply_opacity(opacity);
        if *kind == Menu::Wifi {
            self.wifi(&mut content);
        } else {
            self.bluetooth(&mut content);
        }
        if !self.error.is_empty() {
            content.add(egui::Label::new(&self.error).wrap());
        }
        let measured = content.min_rect().height().min(max_height).max(1.);
        if self.connection_view(kind) == view {
            if opening {
                self.drawer_motion.settle_size(measured);
            } else {
                self.drawer_motion.measure(measured, now, duration);
            }
        } else {
            self.ctx.request_repaint();
        }
        ui.allocate_exact_size(
            vec2(width, current_height.unwrap_or(measured)),
            egui::Sense::hover(),
        );
        if self.drawer_motion.animating() {
            self.ctx
                .request_repaint_after(config::get().duration("animation.frame_ms"));
        }
    }
    fn connection_back(&mut self, ui: &mut egui::Ui, name: &str) -> bool {
        let mut back = false;
        ui.horizontal(|ui| {
            back = self.icon_button(ui, Icon::Back, true);
            ui.add(egui::Label::new(name).truncate());
        });
        back || ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Extra1))
    }
    fn connection_button(&mut self, ui: &mut egui::Ui, connected: bool, enabled: bool) -> bool {
        self.icon_button(
            ui,
            if connected { Icon::Unlink } else { Icon::Link },
            enabled,
        )
    }
    fn secret_field(&mut self, ui: &mut egui::Ui, pairing: bool) {
        // Reserve both slots before drawing either widget. Masked and visible
        // glyphs must not change the field, eye button, or following rows.
        let (row, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 32.), egui::Sense::hover());
        ui.scope_builder(egui::UiBuilder::new().max_rect(row), |ui| {
            let width = (row.width() - 32. - ui.spacing().item_spacing.x).max(1.);
            let (value, hint, id) = if pairing {
                (&mut self.bluetooth_pin, "Pairing code", "bluetooth-pin")
            } else {
                (&mut self.password, "Password", "wifi-password")
            };
            let input = put_text_field(
                ui,
                Rect::from_min_size(row.min, vec2(width, row.height())),
                text_field(value)
                    .password(!self.reveal)
                    .hint_text(hint)
                    .id(egui::Id::new(id))
                    .desired_width(width),
            );
            if self.focus_search {
                input.request_focus();
                self.focus_search = false;
            }
            let label = match (pairing, self.reveal) {
                (true, true) => "Hide pairing code",
                (true, false) => "Show pairing code",
                (false, true) => "Hide password",
                (false, false) => "Show password",
            };
            let button = ui.scope_builder(
                egui::UiBuilder::new().max_rect(Rect::from_center_size(
                    pos2(row.right() - 16., row.center().y),
                    vec2(24., 24.),
                )),
                |ui| self.icon_button(ui, Icon::Eye(!self.reveal), true),
            );
            button.response.on_hover_text(label);
            if button.inner {
                self.reveal = !self.reveal;
                input.request_focus();
            }
        });
    }
    fn wifi(&mut self, ui: &mut egui::Ui) {
        if self.hidden_ssid.is_some() {
            if self.connection_back(ui, "Hidden network") {
                self.hidden_ssid = None;
                self.password.clear();
                self.error.clear();
                return;
            }
            ui.add_space(8.);
            let ssid = self.hidden_ssid.as_mut().unwrap();
            let input = add_text_field(
                ui,
                text_field(ssid)
                    .hint_text("Network name")
                    .desired_width(f32::INFINITY),
            );
            if self.focus_search {
                input.request_focus();
                self.focus_search = false;
            }
            ui.add_space(8.);
            self.secret_field(ui, false);
            ui.add_space(8.);
            let valid = self
                .hidden_ssid
                .as_ref()
                .is_some_and(|s| !s.is_empty() && s.len() <= 32 && !s.contains('\0'));
            if ui
                .add_enabled_ui(valid, |ui| action_button(ui, "Connect"))
                .inner
                .clicked()
                || (valid && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                self.services.network(
                    self.serial,
                    Job::ConnectHidden(
                        self.hidden_ssid.clone().unwrap(),
                        std::mem::take(&mut self.password),
                    ),
                );
            }
            return;
        }
        if let Some(n) = self.selected_network.clone() {
            if self.connection_back(ui, &n.ssid) {
                self.selected_network = None;
                self.password.clear();
                self.error.clear();
                return;
            }
            if !n.active && !n.security.is_empty() {
                ui.add_space(8.);
                self.secret_field(ui, false);
                ui.add_space(8.);
            }
            if action_button(ui, if n.active { "Disconnect" } else { "Connect" }).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                self.services.network(
                    self.serial,
                    if n.active {
                        Job::Disconnect(n.device)
                    } else {
                        Job::Connect(n, std::mem::take(&mut self.password))
                    },
                );
            }
            return;
        }
        let mut enabled = self.network.as_ref().is_none_or(|s| s.enabled);
        if radio_header(ui, &mut enabled, "Wi-Fi").changed() {
            self.services.network(self.serial, Job::Radio(enabled));
        }
        if let Some(snapshot) = self.network.clone() {
            if snapshot.networks.iter().any(|network| network.active) {
                ui.weak("Connected networks");
                for network in snapshot
                    .networks
                    .iter()
                    .filter(|network| network.active)
                    .cloned()
                {
                    self.wifi_network_row(ui, network);
                }
                if snapshot.networks.iter().any(|network| !network.active) {
                    connection_separator(ui, self.dark);
                }
            }
            let list_height = config::get()
                .number("layout.wifi_list_max_height")
                .min((ui.available_height() - 48.).max(1.));
            egui::ScrollArea::vertical()
                .id_salt("wifi-networks")
                .max_height(list_height)
                .show(ui, |ui| {
                    if snapshot.networks.is_empty() {
                        ui.weak(if enabled {
                            "No networks found"
                        } else {
                            "Wi-Fi is off"
                        });
                    }
                    if snapshot.networks.iter().any(|network| !network.active) {
                        ui.weak("Available networks");
                    }
                    for network in snapshot
                        .networks
                        .into_iter()
                        .filter(|network| !network.active)
                    {
                        self.wifi_network_row(ui, network);
                    }
                });
        } else {
            ui.weak("Scanning…");
        }
        if enabled {
            if self
                .network
                .as_ref()
                .is_some_and(|snapshot| !snapshot.networks.is_empty())
            {
                connection_separator(ui, self.dark);
            }
            if action_button(ui, "Join hidden network…").clicked() {
                self.hidden_ssid = Some(String::new());
                self.password.clear();
                self.reveal = false;
                self.focus_search = true;
                self.error.clear();
            }
        }
    }
    fn wifi_network_row(&mut self, ui: &mut egui::Ui, n: network::Network) {
        let (rect, response) = connection_row(ui, (&n.device, &n.bssid));
        paint_row_name(ui, rect, &n.ssid, 24.);
        paint_signal(
            ui,
            Rect::from_center_size(pos2(rect.right() - 10., rect.center().y), vec2(16., 12.)),
            n.signal,
        );
        let mut controls = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(
            pos2(rect.left() + 14., rect.center().y),
            vec2(28., 28.),
        )));
        let action = self.connection_button(&mut controls, n.active, true);
        let row_clicked = response.clicked();
        response.on_hover_text(
            n.rssi_dbm
                .map(|r| format!("{}\n{r} dBm", n.ssid))
                .unwrap_or_else(|| n.ssid.clone()),
        );
        if action || row_clicked {
            if n.active {
                self.services
                    .network(self.serial, Job::Disconnect(n.device));
            } else if n.security.is_empty() || n.security == "--" {
                self.services
                    .network(self.serial, Job::Connect(n, String::new()));
            } else {
                self.selected_network = Some(n);
                self.password.clear();
                self.reveal = false;
                self.focus_search = true;
            }
        }
    }
    fn bluetooth(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.bluetooth.clone();
        let Some(s) = snapshot else {
            if self.error.is_empty() {
                ui.weak("Loading devices…");
            }
            return;
        };
        if s.adapters.is_empty() {
            ui.weak("No Bluetooth adapter");
            return;
        }
        let mut enabled = s.enabled();
        if s.prompt.is_none() && radio_header(ui, &mut enabled, "Bluetooth").changed() {
            let _ = self
                .services
                .bluetooth
                .send(bluetooth::Request::Radio(enabled));
        }
        if !s.enabled() {
            ui.weak("Bluetooth is off");
            return;
        }
        if let Some(prompt) = &s.prompt {
            let name = s
                .devices
                .iter()
                .find(|d| Some(&d.path) == s.busy.as_ref())
                .map(|d| d.name.as_str())
                .unwrap_or("Pair device");
            if self.connection_back(ui, name) {
                self.bluetooth_pin.clear();
                let _ = self.services.bluetooth.send(bluetooth::Request::Cancel);
                return;
            }
            ui.add_space(8.);
            match prompt {
                bluetooth::Prompt::Confirm(message) => {
                    ui.label(message);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Confirm").frame(false)).clicked() {
                            let _ = self
                                .services
                                .bluetooth
                                .send(bluetooth::Request::Answer(Some(String::new())));
                        }
                        if ui.add(egui::Button::new("Cancel").frame(false)).clicked() {
                            let _ = self.services.bluetooth.send(bluetooth::Request::Cancel);
                        }
                    });
                }
                bluetooth::Prompt::Pin | bluetooth::Prompt::Passkey => {
                    ui.label("Enter the device’s pairing code");
                    ui.add_space(4.);
                    self.secret_field(ui, true);
                    ui.add_space(8.);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Pair").frame(false)).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            let _ = self
                                .services
                                .bluetooth
                                .send(bluetooth::Request::Answer(Some(std::mem::take(
                                    &mut self.bluetooth_pin,
                                ))));
                        }
                        if ui.add(egui::Button::new("Cancel").frame(false)).clicked() {
                            let _ = self.services.bluetooth.send(bluetooth::Request::Cancel);
                        }
                    });
                }
                bluetooth::Prompt::Display(message) => {
                    ui.label(message);
                }
            }
            return;
        }
        if let Some(path) = &s.busy {
            ui.horizontal(|ui| {
                let name = s
                    .devices
                    .iter()
                    .find(|d| &d.path == path)
                    .map(|d| d.name.as_str())
                    .unwrap_or("device");
                ui.add_sized(
                    vec2(
                        (ui.available_width() - 65.).max(1.),
                        config::get().number("layout.control_height"),
                    ),
                    egui::Label::new(format!("Connecting {name}…")).truncate(),
                );
                if ui.add(egui::Button::new("Cancel").frame(false)).clicked() {
                    let _ = self.services.bluetooth.send(bluetooth::Request::Cancel);
                }
            });
        }
        if let Some(error) = &s.action_error {
            ui.label(error);
        }
        if s.devices.iter().any(|device| device.connected) {
            ui.weak("Connected devices");
            for device in s.devices.iter().filter(|device| device.connected) {
                self.bluetooth_device_row(ui, device, s.busy.is_none());
            }
            connection_separator(ui, self.dark);
        }
        egui::ScrollArea::vertical()
            .id_salt("bluetooth-devices")
            .max_height((ui.available_height() - 4.).max(1.))
            .show(ui, |ui| {
                for (title, paired) in [("Paired devices", true), ("Nearby devices", false)] {
                    let devices: Vec<_> = s
                        .devices
                        .iter()
                        .filter(|device| !device.connected && device.paired == paired)
                        .collect();
                    if devices.is_empty() {
                        continue;
                    }
                    ui.weak(title);
                    for device in devices {
                        self.bluetooth_device_row(ui, device, s.busy.is_none());
                    }
                }
            });
        if s.devices.is_empty() {
            ui.weak("No devices found");
        }
    }
    fn bluetooth_device_row(
        &mut self,
        ui: &mut egui::Ui,
        device: &bluetooth::Device,
        enabled: bool,
    ) {
        let (rect, response) = connection_row(ui, &device.path);
        let row_clicked = response.clicked();
        response.on_hover_text(format!("{}\n{}", device.name, device.address));
        paint_row_name(
            ui,
            rect,
            &device.name,
            if device.connected && device.battery.is_some() {
                50.
            } else {
                24.
            },
        );
        if device.connected
            && let Some(battery) = device.battery
        {
            let battery_rect =
                Rect::from_center_size(pos2(rect.right() - 36., rect.center().y), vec2(16., 8.));
            paint_battery(ui, battery_rect, battery);
        }
        paint_signal(
            ui,
            Rect::from_center_size(pos2(rect.right() - 10., rect.center().y), vec2(16., 12.)),
            device
                .rssi
                .map(|r| ((i32::from(r).clamp(-100, -40) + 100) * 100 / 60) as u8),
        );
        let mut controls = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(
            pos2(rect.left() + 14., rect.center().y),
            vec2(28., 28.),
        )));
        let activate = self.connection_button(&mut controls, device.connected, enabled);
        let action = if device.connected {
            bluetooth::Action::Disconnect
        } else if device.paired {
            bluetooth::Action::Connect
        } else {
            bluetooth::Action::Pair
        };
        if activate || (enabled && row_clicked) {
            let _ = self
                .services
                .bluetooth
                .send(bluetooth::Request::Action(device.path.clone(), action));
        }
    }
    fn session(&mut self, ui: &mut egui::Ui) {
        for (action, icon, label) in [
            (SessionAction::Lock, Icon::Lock, "Lock"),
            (SessionAction::Logout, Icon::Logout, "Log out"),
            (SessionAction::Reboot, Icon::Reboot, "Reboot"),
            (SessionAction::Shutdown, Icon::Power, "Shut down"),
        ] {
            let confirming = self.confirm_session == Some(action);
            let text = if confirming {
                format!("Confirm {}", label.to_lowercase())
            } else {
                label.into()
            };
            let clicked = ui
                .horizontal(|ui| {
                    let clicked = self.icon_button(ui, icon, !self.busy);
                    let label = ui.add_enabled(
                        !self.busy,
                        egui::Label::new(text).sense(egui::Sense::click()),
                    );
                    clicked || label.clicked()
                })
                .inner;
            if clicked {
                if action == SessionAction::Lock || confirming {
                    self.busy = true;
                    self.error.clear();
                    self.services.session(action);
                } else {
                    self.confirm_session = Some(action);
                }
            }
        }
        if self.confirm_session.is_some() && !self.busy && flat(ui, "Cancel").clicked() {
            self.confirm_session = None;
        }
    }
    fn sound(&mut self, ui: &mut egui::Ui) {
        let outputs = self.audio.outputs.clone();
        let active = self.audio.active().cloned();
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = 32.;
            ui.spacing_mut().button_padding = vec2(8., 6.);
            let dark = ui.visuals().dark_mode;
            let widgets = &mut ui.visuals_mut().widgets;
            for widget in [
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
            ] {
                widget.corner_radius = egui::CornerRadius::same(10);
                widget.bg_stroke = egui::Stroke::NONE;
            }
            widgets.inactive.weak_bg_fill =
                config::get().color("button", dark).gamma_multiply(0.55);
            widgets.hovered.weak_bg_fill = config::get().color("button_hover", dark);
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 30.).max(1.);
                egui::ComboBox::from_id_salt("sound-device")
                    .popup_style(egui::style::StyleModifier::from(
                        move |style: &mut egui::Style| {
                            style.visuals.override_text_color =
                                Some(config::get().color("popup_text", dark));
                            style.visuals.window_fill = config::get().color("popup", dark);
                            style.visuals.window_stroke = egui::Stroke::NONE;
                            style.visuals.popup_shadow = egui::Shadow::NONE;
                            style.visuals.menu_corner_radius = egui::CornerRadius::same(10);
                            style.visuals.selection.bg_fill =
                                config::get().color("button_hover", dark);
                            style.spacing.interact_size.y = 30.;
                        },
                    ))
                    .width(width)
                    .selected_text(
                        active
                            .as_ref()
                            .map(|output| output.description.as_str())
                            .unwrap_or("No audio device"),
                    )
                    .show_ui(ui, |ui| {
                        for output in outputs {
                            let ports: Vec<_> =
                                output.ports.iter().filter(|port| port.available).collect();
                            if ports.is_empty() {
                                if ui
                                    .selectable_label(
                                        output.name == self.audio.default,
                                        &output.description,
                                    )
                                    .clicked()
                                {
                                    self.control(volume::Control::Output(output.name.clone()));
                                }
                            } else {
                                for port in ports {
                                    if ui
                                        .selectable_label(
                                            output.name == self.audio.default
                                                && port.name == output.active_port,
                                            format!(
                                                "{} · {}",
                                                output.description, port.description
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.control(volume::Control::Port(
                                            output.name.clone(),
                                            port.name.clone(),
                                        ));
                                    }
                                }
                            }
                        }
                    });
                if let Some(output) = &active
                    && self.icon_button(ui, Icon::Volume(output.muted), true)
                {
                    self.control(volume::Control::Mute(output.name.clone()));
                }
            });
        });
        if let Some(output) = &active {
            ui.add_space(6.);
            let mut value = output.percent() as f32;
            let width = ui.available_width();
            if volume_slider(ui, &mut value, width).changed() {
                self.control(volume::Control::Volume {
                    output: output.name.clone(),
                    channel: None,
                    percent: value.round() as u8,
                });
            }
        }
        ui.add_space(6.);
        if self.audio.streams.is_empty() {
            ui.weak("No app audio");
        }
        let players = audio_sources::player_rows(&self.audio.streams, &self.state.extras.tracks)
            .into_iter()
            .map(|(index, track)| (index, track.clone()))
            .collect::<HashMap<_, _>>();
        for stream in self.audio.streams.clone() {
            ui.push_id(stream.index, |ui| {
                let track = players.get(&stream.index).cloned();
                let (row, _) = ui.allocate_exact_size(
                    vec2(
                        ui.available_width(),
                        config::get().number("layout.audio_row_height").max(96.),
                    ),
                    egui::Sense::hover(),
                );
                let artwork_size = row.height() - 20.;
                let artwork =
                    Rect::from_min_size(row.min + vec2(0., 10.), vec2(artwork_size, artwork_size));
                let texture = track
                    .as_ref()
                    .and_then(|track| {
                        self.state.extras.artworks.get(&track.art_url).or_else(|| {
                            self.state
                                .extras
                                .artwork
                                .as_ref()
                                .filter(|art| art.url == track.art_url)
                        })
                    })
                    .map(|art| {
                        let pixels =
                            (artwork_size * self.ctx.pixels_per_point()).round().max(1.) as u32;
                        let key = format!("artwork:{}:{pixels}", art.url);
                        self.textures
                            .entry(key.clone())
                            .or_insert_with(|| {
                                self.ctx.load_texture(
                                    &key,
                                    application_icons::raster_premultiplied(
                                        [art.pixels.width() as usize, art.pixels.height() as usize],
                                        art.pixels.data(),
                                        pixels,
                                    ),
                                    egui::TextureOptions::LINEAR,
                                )
                            })
                            .id()
                    })
                    .unwrap_or_else(|| self.app_icon(&stream.icon, artwork_size));
                ui.painter().image(
                    texture,
                    artwork,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
                let middle = Rect::from_min_max(
                    row.min + vec2(artwork_size + 8., 10.),
                    pos2(row.right() - 32., row.bottom() - 10.),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(middle), |ui| {
                    ui.spacing_mut().item_spacing.y = 6.;
                    let name = track
                        .as_ref()
                        .map(|track| track.title.as_str())
                        .unwrap_or(&stream.name);
                    ui.add_sized(
                        vec2(ui.available_width(), 16.),
                        egui::Label::new(name)
                            .truncate()
                            .halign(egui::Align::Center),
                    )
                    .on_hover_text(format!("{} · {name}", stream.application));
                    ui.horizontal(|ui| {
                        let row_width = ui.available_width();
                        let button_group_width = 3. * 28. + 2. * ui.spacing().item_spacing.x;
                        ui.add_space(((row_width - button_group_width) / 2.).max(0.));
                        for (icon, control, enabled) in [
                            (
                                Icon::Previous,
                                media::Control::Previous,
                                track.as_ref().is_some_and(|track| track.can_previous),
                            ),
                            (
                                if track.as_ref().is_some_and(|track| track.playing) {
                                    Icon::Pause
                                } else {
                                    Icon::Play
                                },
                                media::Control::Toggle,
                                track.as_ref().is_some_and(|track| track.can_toggle),
                            ),
                            (
                                Icon::Next,
                                media::Control::Next,
                                track.as_ref().is_some_and(|track| track.can_next),
                            ),
                        ] {
                            if self.playback_button(ui, icon, enabled)
                                && let Some(track) = &track
                            {
                                let _ = self.services.media.send(media::Request::Control {
                                    panel_id: self.serial,
                                    player: track.player.clone(),
                                    control,
                                });
                            }
                        }
                    });
                    let mut value = stream.percent as f32;
                    if volume_slider(ui, &mut value, ui.available_width()).changed() {
                        self.control(volume::Control::StreamVolume(
                            stream.index,
                            value.round() as u8,
                        ));
                    }
                });
                let levels = self
                    .meters
                    .get(&stream.index)
                    .map(|meter| meter.displayed)
                    .unwrap_or([0.; 7]);
                let right =
                    Rect::from_min_size(pos2(row.right() - 28., row.top() + 36.), vec2(28., 52.));
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(right)
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 8.;
                        let (meter, _) =
                            ui.allocate_exact_size(vec2(28., 20.), egui::Sense::hover());
                        paint_equalizer(ui.painter(), meter, &levels);
                        if self.icon_button(ui, Icon::Volume(stream.muted), true) {
                            self.control(volume::Control::StreamMute(stream.index));
                        }
                    },
                );
            });
        }
    }
    fn processes(&mut self, ui: &mut egui::Ui) {
        ui.label(
            self.cpu
                .percent
                .map(|v| format!("CPU  {v:.0}%"))
                .unwrap_or_else(|| "CPU".into()),
        );
        let search = add_text_field(
            ui,
            text_field(&mut self.query)
                .hint_text("Search processes")
                .desired_width(f32::INFINITY),
        );
        if self.focus_search {
            search.request_focus();
            self.focus_search = false;
        }
        let query = self.query.trim().to_lowercase();
        let width = ui.available_width();
        for (name, pid, cpu, memory) in
            std::iter::once(("Process".into(), "PID".into(), "CPU".into(), "RAM".into())).chain(
                self.cpu
                    .processes
                    .iter()
                    .filter(|p| p.matches_query(&query))
                    .take(config::get().number("processes.rows") as usize)
                    .map(|p| {
                        (
                            p.name.clone(),
                            p.pid.to_string(),
                            format!("{:.1}%", p.percent),
                            format!("{}M", p.memory / 1_048_576),
                        )
                    }),
            )
        {
            let (r, _) = ui.allocate_exact_size(
                vec2(width, config::get().number("layout.process_row_height")),
                egui::Sense::hover(),
            );
            let mut job = egui::text::LayoutJob::simple(
                name,
                egui::FontId::proportional(config::get().number("appearance.font_size")),
                ui.visuals().text_color(),
                (width - config::get().number("layout.process_columns_width")).max(1.),
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let text = ui.painter().layout_job(job);
            ui.painter().galley(
                pos2(r.left(), r.center().y - text.size().y / 2.),
                text,
                ui.visuals().text_color(),
            );
            for (value, offset) in [
                (pid, config::get().number("layout.pid_column_offset")),
                (cpu, config::get().number("layout.cpu_column_offset")),
                (memory, 0.),
            ] {
                ui.painter().text(
                    pos2(r.right() - offset, r.center().y),
                    egui::Align2::RIGHT_CENTER,
                    value,
                    egui::FontId::monospace(config::get().number("appearance.small_font_size")),
                    ui.visuals().text_color(),
                );
            }
        }
        if !self.cpu.processes.iter().any(|p| p.matches_query(&query)) {
            ui.label(if query.is_empty() {
                "Loading…"
            } else {
                "No matching processes"
            });
        }
    }
    fn workspace(&mut self, ui: &mut egui::Ui) {
        let Some(snapshot) = self.preview.clone() else {
            ui.label("Loading…");
            return;
        };
        let name = match self.panel.as_ref().map(|panel| &panel.kind) {
            Some(Menu::Workspace(name)) => name.clone(),
            _ => snapshot.name.clone(),
        };
        let apps = self.workspace_apps(&name);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Workspace {name}"));
            for app in apps {
                if let Some(icon) = self.application_icon(&app, 20.) {
                    ui.add(egui::Image::new((icon, vec2(20., 20.))))
                        .on_hover_text(app);
                } else {
                    ui.add(egui::Label::new(app).truncate());
                }
            }
        });
        let (canvas, response) = ui.allocate_exact_size(
            vec2(self.preview_size[0], self.preview_size[1]),
            egui::Sense::click(),
        );
        let s = &snapshot;
        let amount = self
            .preview_previous
            .as_ref()
            .map(|(_, at)| {
                (at.elapsed().as_secs_f32()
                    / config::get()
                        .duration("animation.preview_fade_ms")
                        .as_secs_f32())
                .min(1.)
            })
            .unwrap_or(1.);
        if let Some((old, _)) = &self.preview_previous {
            self.paint_workspace(ui, canvas, old, "preview-old:", 1.);
            if amount < 1. {
                self.ctx
                    .request_repaint_after(config::get().duration("animation.frame_ms"));
            }
        }
        self.paint_workspace(ui, canvas, s, "preview:", amount);
        if response.clicked() {
            let _ = self
                .services
                .status
                .send(status::Update::Focus(name.clone()));
            self.close();
        }
    }
    fn paint_workspace(
        &self,
        ui: &egui::Ui,
        canvas: Rect,
        s: &workspace_preview::Snapshot,
        prefix: &str,
        opacity: f32,
    ) {
        let mut painter = ui.painter().with_clip_rect(canvas);
        painter.multiply_opacity(opacity * 0.9);
        let scale = workspace_preview::preview_scale(s.rect, self.preview_size);
        painter.rect_filled(
            canvas,
            8,
            config::get()
                .color("preview_canvas", self.dark)
                .gamma_multiply(0.18),
        );
        for (i, w) in s.windows.iter().enumerate() {
            let map = |r: workspace_preview::Geometry| {
                Rect::from_min_size(
                    canvas.min + vec2((r.x - s.rect.x) * scale, (r.y - s.rect.y) * scale),
                    vec2(r.w * scale, r.h * scale),
                )
            };
            let rect = map(w.content);
            painter.rect_filled(rect, 0, config::get().color("preview_window", self.dark));
            if w.pixels.is_some() {
                if let Some(texture) = self.textures.get(&format!("{prefix}{i}")) {
                    painter.image(
                        texture.id(),
                        rect,
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
            } else {
                painter.with_clip_rect(rect.intersect(canvas)).text(
                    rect.left_bottom() + vec2(3., -4.),
                    egui::Align2::LEFT_BOTTOM,
                    &w.title,
                    egui::FontId::proportional(
                        config::get().number("appearance.small_font_size") - 1.,
                    ),
                    Color32::WHITE,
                );
            }
        }
        if !s.message.is_empty() {
            painter.text(
                canvas.left_bottom() + vec2(3., -3.),
                egui::Align2::LEFT_BOTTOM,
                &s.message,
                egui::FontId::proportional(config::get().number("appearance.small_font_size") - 1.),
                Color32::WHITE,
            );
        }
    }
}

// Optional installed font; no fontconfig library or bundled proprietary font.
fn installed_font(monospace: bool) -> Option<std::path::PathBuf> {
    fn find(dir: &std::path::Path, depth: u8, names: &[&str]) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .replace('-', "");
            if names.contains(&name.as_str()) {
                return Some(path);
            }
            if depth > 0
                && entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(path) = find(&path, depth - 1, names)
            {
                return Some(path);
            }
        }
        None
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let names: &[&str] = if monospace {
        &[
            "sfmono regular.otf",
            "sfmonoregular.otf",
            "sfmonoregular.ttf",
            "dejavusansmono.ttf",
            "liberationmonoregular.ttf",
        ]
    } else {
        &[
            "sfprotextregular.otf",
            "interregular.ttf",
            "adwaitasansregular.ttf",
            "liberationsansregular.ttf",
            "dejavusans.ttf",
        ]
    };
    [
        format!("{data}/fonts"),
        format!("{home}/.fonts"),
        "/usr/local/share/fonts".into(),
        "/usr/share/fonts".into(),
    ]
    .iter()
    .find_map(|dir| {
        names
            .iter()
            .find_map(|name| find(std::path::Path::new(dir), 4, &[*name]))
    })
}
fn volume_slider(ui: &mut egui::Ui, value: &mut f32, width: f32) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = width;
        ui.spacing_mut().interact_size.y = config::get().number("layout.slider_height");
        ui.spacing_mut().slider_rail_height = 0.;
        let dark = ui.visuals().dark_mode;
        let widgets = &mut ui.visuals_mut().widgets;
        for widget in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            widget.bg_fill = Color32::TRANSPARENT;
            widget.fg_stroke = egui::Stroke::NONE;
            widget.bg_stroke = egui::Stroke::NONE;
        }
        let response = ui.add(
            egui::Slider::new(value, 0.0..=150.)
                .show_value(false)
                .trailing_fill(false),
        );
        let radius = response.rect.height() / 2.;
        let range = response.rect.x_range().shrink(response.rect.height() / 2.5);
        let center = pos2(
            egui::lerp(range, (*value / 150.).clamp(0., 1.)),
            response.rect.center().y,
        );
        ui.painter().rect_filled(
            response.rect,
            radius as u8,
            config::get().color("slider_rail", dark),
        );
        ui.painter().rect_filled(
            Rect::from_min_max(
                response.rect.min,
                pos2(center.x + radius, response.rect.bottom()),
            ),
            radius as u8,
            config::get()
                .color("slider_handle", dark)
                .gamma_multiply(0.94),
        );
        let painter = ui.painter().with_clip_rect(response.rect);
        painter.circle_filled(
            center + vec2(0., 0.5),
            radius,
            Color32::from_black_alpha(24),
        );
        painter.circle_filled(
            center,
            radius - 0.5,
            config::get().color("slider_handle", dark),
        );
        response
    })
    .inner
}

// Give every text input the same comfortable vertical padding.
fn text_field(value: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(value).margin(egui::Margin::symmetric(6, 7))
}

fn add_text_field(ui: &mut egui::Ui, edit: egui::TextEdit<'_>) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.), egui::Sense::hover());
    put_text_field(ui, rect, edit)
}

fn put_text_field(ui: &mut egui::Ui, rect: Rect, edit: egui::TextEdit<'_>) -> egui::Response {
    // egui's default text-edit frame expands with hover/focus visuals. Use a
    // fixed frame and paint its outline without changing the content bounds.
    let frame = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 7))
        .fill(ui.visuals().text_edit_bg_color())
        .corner_radius(3);
    let response = ui.put(rect, edit.frame(frame).vertical_align(egui::Align::Center));
    let color = if response.has_focus() {
        ui.visuals().selection.stroke.color
    } else {
        config::get().color("slider_border", ui.visuals().dark_mode)
    };
    ui.painter().rect_stroke(
        rect,
        3,
        egui::Stroke::new(1_f32, color),
        egui::StrokeKind::Inside,
    );
    response
}

fn action_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    // Keep the hit target and text independent of egui's stateful frame margins.
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 28.), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let dark = ui.visuals().dark_mode;
    let highlighted = ui.is_enabled() && (response.hovered() || response.has_focus());
    let mut fill = config::get().color(
        if highlighted {
            "button_hover"
        } else {
            "button"
        },
        dark,
    );
    if response.is_pointer_button_down_on() {
        fill = fill.gamma_multiply(0.9);
    }
    ui.painter().rect_filled(rect, 7, fill);
    ui.painter().with_clip_rect(rect.shrink(2.)).text(
        rect.left_center() + vec2(8., 0.),
        egui::Align2::LEFT_CENTER,
        label,
        egui::TextStyle::Button.resolve(ui.style()),
        if ui.is_enabled() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        },
    );
    response
}

fn tray_menu_rows(ui: &mut egui::Ui, rows: &[crate::tray::MenuRow]) -> Option<i32> {
    let mut clicked = None;
    for row in rows.iter().filter(|row| row.visible) {
        if row.separator {
            ui.separator();
            continue;
        }
        let label = match row.checked {
            Some(true) => format!("✓  {}", row.label),
            Some(false) => format!("    {}", row.label),
            None => row.label.clone(),
        };
        if row.children.is_empty() {
            if ui
                .add_enabled_ui(row.enabled, |ui| action_button(ui, &label))
                .inner
                .clicked()
            {
                clicked = Some(row.id);
            }
        } else {
            ui.add_enabled_ui(row.enabled, |ui| {
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    ui.make_persistent_id(row.id),
                    false,
                );
                let arrow = if state.is_open() { "▾" } else { "▸" };
                let response = action_button(ui, &format!("{arrow}  {label}"));
                if response.clicked() {
                    state.toggle(ui);
                }
                state.show_body_indented(&response, ui, |ui| {
                    clicked = clicked.or_else(|| tray_menu_rows(ui, &row.children));
                });
            });
        }
    }
    clicked
}
