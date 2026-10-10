mod keyboard;
mod services;
mod wayland;
use crate::{
    audio_meter, config, icons::Icon, launcher, media, network, popup_motion::Dismissal, status,
    volume, workspace_preview,
};
use egui::{Color32, Pos2, Rect, pos2, vec2};
use services::{Event, Job, Services};
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
    Session,
    Cpu,
    Workspace(String),
}
struct Meter {
    levels: Arc<Mutex<[f64; 7]>>,
    capture: Option<audio_meter::Capture>,
    retry: Instant,
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
    network: Option<network::Snapshot>,
    apps: Vec<launcher::Entry>,
    panel: Option<Panel>,
    preview: Option<workspace_preview::Snapshot>,
    // Ready -> waiting for capture -> crossfading -> ready. Timestamps advance
    // on repaint; the outgoing snapshot stays visible while capture is pending.
    preview_previous: Option<(workspace_preview::Snapshot, Instant)>,
    preview_pending: Option<workspace_preview::Snapshot>,
    textures: HashMap<String, egui::TextureHandle>,
    meters: HashMap<u32, Meter>,
    query: String,
    selection: usize,
    selected_network: Option<network::Network>,
    password: String,
    reveal: bool,
    confirm_logout: bool,
    focus_search: bool,
    error: String,
    busy: bool,
    pending: VecDeque<volume::Control>,
    serial: u64,
    claim: u128,
    hover: Option<(Menu, Instant)>,
    blocked_hover: Option<Menu>,
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
        let font = options
            .font
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(sf_mono);
        if let Some(path) = font {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "custom".into(),
                egui::FontData::from_owned(std::fs::read(&path)?).into(),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .get_mut(&family)
                    .unwrap()
                    .insert(0, "custom".into());
            }
            ctx.set_fonts(fonts);
        }
        Ok(Self {
            ctx,
            services: Services::new(options.output.clone()),
            state: status::Status::default(),
            audio: volume::Snapshot::default(),
            cpu: crate::cpu::Snapshot::default(),
            network: None,
            apps: vec![],
            panel: None,
            preview: None,
            preview_previous: None,
            preview_pending: None,
            textures: HashMap::new(),
            meters: HashMap::new(),
            query: String::new(),
            selection: 0,
            selected_network: None,
            password: String::new(),
            reveal: false,
            confirm_logout: false,
            focus_search: false,
            error: String::new(),
            busy: false,
            pending: VecDeque::new(),
            serial: 0,
            claim: 0,
            hover: None,
            blocked_hover: None,
            preview_size: [312., 220.],
            panel_rect: Rect::NOTHING,
            dark: options.dark,
        })
    }
    fn close(&mut self) {
        if let Some(p) = &mut self.panel {
            self.blocked_hover = Some(p.kind.clone());
            p.closing.get_or_insert(Instant::now());
        }
        self.meters.clear();
        let _ = self.services.cpu.send(false);
        let _ = self.services.preview.send(None);
    }
    fn finish_close(&mut self) {
        self.close();
        self.panel = None;
        self.panel_rect = Rect::NOTHING;
        self.preview = None;
        self.preview_previous = None;
        self.preview_pending = None;
        self.textures.retain(|k, _| k.starts_with("icon:"));
    }
    fn open(&mut self, kind: Menu, x: f32, clicked: bool) {
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
        let opened = self
            .panel
            .as_ref()
            .filter(|p| p.closing.is_none())
            .map(|p| p.opened)
            .unwrap_or_else(Instant::now);
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
        self.confirm_logout = false;
        self.focus_search = kind == Menu::Apps;
        match &kind {
            Menu::Apps => self.services.apps(self.serial),
            Menu::Sound => {
                let _ = self.services.volume.send(volume::Request::Refresh);
                let _ = self.services.media.send(media::Request::Refresh);
            }
            Menu::Wifi => self.services.network(self.serial, Job::Scan(false)),
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
        while let Ok(s) = self.services.cpu_events.try_recv() {
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
                    extras
                        .artworks
                        .retain(|url, _| tracks.iter().any(|track| &track.art_url == url));
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
        while let Ok(event) = self.services.events.try_recv() {
            self.ctx.request_repaint();
            match event {
                Event::Apps(id, apps) if id == self.serial => self.apps = apps,
                Event::Network(id, result) if id == self.serial => match result {
                    Ok(s) => {
                        self.state.extras.wifi_enabled = Some(s.enabled);
                        self.state.extras.wifi_name =
                            s.networks.iter().find(|n| n.active).map(|n| n.ssid.clone());
                        self.state.extras.wifi_signal =
                            s.networks.iter().find(|n| n.active).and_then(|n| n.signal);
                        self.network = Some(s);
                        self.selected_network = None;
                        self.password.clear();
                        self.error.clear();
                    }
                    Err(e) => self.error = e,
                },
                Event::Launched(id, result) if id == self.serial => match result {
                    Ok(()) => self.close(),
                    Err(e) => self.error = e,
                },
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
                }
            }
        }
    }
    fn icon(&mut self, icon: Icon) -> egui::TextureId {
        let key = format!(
            "icon:{}",
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
            }
        );
        let ctx = &self.ctx;
        self.textures
            .entry(key.clone())
            .or_insert_with(|| {
                let mut p = tiny_skia::Pixmap::new(48, 48).unwrap();
                let color_key = if self.dark {
                    "appearance.icon_color_dark"
                } else {
                    "appearance.icon_color_light"
                };
                let c = u32::from_str_radix(&config::get().text(color_key)[1..], 16).unwrap();
                crate::icons::draw(
                    &mut p,
                    icon,
                    0.,
                    0.,
                    24.,
                    2.,
                    tiny_skia::Color::from_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255),
                );
                ctx.load_texture(
                    &key,
                    egui::ColorImage::from_rgba_premultiplied([48, 48], p.data()),
                    egui::TextureOptions::LINEAR,
                )
            })
            .id()
    }
    fn app_icon(&mut self, name: &str) -> egui::TextureId {
        let key = format!("app-icon:{name}");
        if let Some(texture) = self.textures.get(&key) {
            return texture.id();
        }
        if !name.is_empty() && !name.contains('/') {
            for root in std::env::var("XDG_DATA_DIRS")
                .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
                .split(':')
            {
                for size in ["48x48", "32x32", "64x64", "256x256"] {
                    let path = std::path::Path::new(root)
                        .join(format!("icons/hicolor/{size}/apps/{name}.png"));
                    if let Ok(image) = image::open(path) {
                        let image = image.thumbnail(48, 48).into_rgba8();
                        let texture = self.ctx.load_texture(
                            &key,
                            egui::ColorImage::from_rgba_unmultiplied(
                                [image.width() as usize, image.height() as usize],
                                image.as_raw(),
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        let id = texture.id();
                        self.textures.insert(key, texture);
                        return id;
                    }
                }
            }
        }
        let id = self.icon(Icon::Music);
        self.textures.insert(key, self.textures["icon:2"].clone());
        id
    }
    fn icon_button(&mut self, ui: &mut egui::Ui, icon: Icon, enabled: bool) -> bool {
        let image = egui::Image::new((self.icon(icon), vec2(14., 14.)));
        ui.add_enabled(
            enabled,
            egui::Button::image(image).frame(false).min_size(vec2(
                config::get().number("layout.widget_min_width"),
                config::get().number("layout.widget_min_height"),
            )),
        )
        .clicked()
    }
    fn paint_bar(&mut self, ui: &mut egui::Ui, width: f32) -> Vec<(Rect, Menu)> {
        let gap = config::get().number("layout.group_gap");
        let margin = config::get().number("layout.bar_margin");
        let slot = config::get().number("layout.icon_slot_width");
        let cpu_width = config::get().number("layout.cpu_width");
        let session_width = config::get().number("layout.session_width");
        let apps_width = config::get().number("layout.apps_width");
        let workspace_width = config::get().number("layout.workspace_width");
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
                      text: &str,
                      icon: Option<Icon>,
                      kind: Menu| {
            let response = ui.interact(rect, egui::Id::new(("bar", &kind)), egui::Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(0., 2.)),
                    config::get().number("layout.workspace_corner_radius") as u8,
                    config::get().color("hover", this.dark),
                );
            }
            let mut text_rect = rect.shrink2(vec2(4., 0.));
            if let Some(icon) = icon {
                ui.painter().image(
                    this.icon(icon),
                    Rect::from_center_size(
                        pos2(rect.left() + slot / 2., center_y),
                        vec2(
                            config::get().number("appearance.icon_size"),
                            config::get().number("appearance.icon_size"),
                        ),
                    ),
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
                text_rect.min.x = rect.left() + slot;
            }
            if !text.is_empty() {
                let mut job = egui::text::LayoutJob::simple(
                    text.into(),
                    egui::FontId::proportional(config::get().number("appearance.font_size")),
                    ui.visuals().text_color(),
                    text_rect.width(),
                );
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                let galley = ui.painter().layout_job(job);
                let position = if kind == Menu::Cpu {
                    pos2(
                        text_rect.left(),
                        text_rect.center().y - galley.size().y / 2.,
                    )
                } else {
                    text_rect.center() - galley.size() / 2.
                };
                ui.painter()
                    .galley(position, galley, ui.visuals().text_color());
            }
            if response.clicked() {
                this.open(kind.clone(), rect.center().x, true);
            }
            targets.push((rect, kind));
        };
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(margin, 0.),
                vec2(session_width, config::get().number("layout.bar_height")),
            ),
            "",
            None,
            Menu::Session,
        );
        for y in [center_y - 3., center_y + 1.] {
            for x in [
                margin + session_width / 2. - 3.,
                margin + session_width / 2. + 1.,
            ] {
                ui.painter().rect_filled(
                    Rect::from_min_size(pos2(x, y), vec2(3., 3.)),
                    0,
                    ui.visuals().text_color(),
                );
            }
        }
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(margin + session_width + gap, 0.),
                vec2(apps_width, config::get().number("layout.bar_height")),
            ),
            "Apps",
            None,
            Menu::Apps,
        );
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
            let amount = self.ctx.animate_bool_with_time(
                egui::Id::new(("audible", &w.name)),
                w.audible,
                config::get()
                    .duration("animation.speaker_fade_ms")
                    .as_secs_f32(),
            );
            if amount > 0. {
                let r = Rect::from_center_size(
                    pos2(
                        x + config::get().number("layout.workspace_audio_offset"),
                        center_y,
                    ),
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
        let battery = self
            .state
            .battery
            .map(|(p, _)| format!("{p}%"))
            .unwrap_or_default();
        ui.painter().text(
            pos2(
                right - config::get().number("layout.battery_text_offset"),
                center_y,
            ),
            egui::Align2::LEFT_CENTER,
            battery,
            egui::FontId::proportional(config::get().number("appearance.font_size")),
            ui.visuals().text_color(),
        );
        if let Some((percent, charging)) = self.state.battery {
            let rect = Rect::from_min_size(
                pos2(
                    right - config::get().number("layout.battery_width"),
                    center_y - config::get().number("layout.battery_icon_height") / 2.,
                ),
                vec2(
                    config::get().number("layout.battery_icon_width"),
                    config::get().number("layout.battery_icon_height"),
                ),
            );
            let color = ui.visuals().text_color();
            ui.painter().rect_stroke(
                rect,
                2,
                egui::Stroke::new(1.0_f32, color),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(pos2(rect.right() + 1., center_y - 2.), vec2(2., 4.)),
                1,
                color,
            );
            if charging {
                ui.painter().image(
                    self.icon(Icon::Charging),
                    rect.shrink(1.),
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
            } else {
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
        }
        let wifi_x = right - config::get().number("layout.battery_width") - gap - slot;
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(wifi_x, 0.),
                vec2(slot, config::get().number("layout.bar_height")),
            ),
            "",
            Some(Icon::Wifi(self.state.extras.wifi_signal)),
            Menu::Wifi,
        );
        let sound_x = wifi_x - slot - gap;
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(sound_x, 0.),
                vec2(slot, config::get().number("layout.bar_height")),
            ),
            "",
            Some(Icon::Volume(self.audio.active().is_some_and(|o| o.muted))),
            Menu::Sound,
        );
        let cpu_x = sound_x - cpu_width - gap;
        let percent = self
            .cpu
            .percent
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into());
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(
                pos2(cpu_x, 0.),
                vec2(cpu_width, config::get().number("layout.bar_height")),
            ),
            &percent,
            Some(Icon::Cpu),
            Menu::Cpu,
        );
        for separator in [
            cpu_x + cpu_width + gap / 2.,
            sound_x + slot + gap / 2.,
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
        let title_bounds = Rect::from_min_max(
            pos2(x + 8., 0.),
            pos2(cpu_x - gap, config::get().number("layout.bar_height")),
        );
        if title_bounds.width() > 60. {
            let painter = ui.painter().with_clip_rect(title_bounds);
            painter.text(
                pos2(
                    (width / 2.).clamp(title_bounds.min.x, title_bounds.max.x),
                    center_y,
                ),
                egui::Align2::CENTER_CENTER,
                &self.state.app,
                egui::FontId::proportional(config::get().number("appearance.font_size")),
                ui.visuals().text_color(),
            );
        }
        targets
    }
    fn frame(&mut self, ctx: &egui::Context, width: f32, height: f32) {
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
        if opacity <= 0. && panel.closing.is_some() {
            self.finish_close();
            return;
        }
        if opacity < 1. || panel.closing.is_some() || kind == Menu::Sound {
            ctx.request_repaint_after(config::get().duration("animation.frame_ms"));
        }
        let min_height = match &kind {
            Menu::Cpu => {
                config::get().number("layout.row_height")
                    + (self.cpu.processes.len() + 1) as f32
                        * config::get().number("layout.row_height")
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
        } else {
            screen_max_height.max(config::get().number("layout.popup_min_height"))
        };
        let shown = egui::Area::new(egui::Id::new(("panel", self.serial)))
            .fixed_pos(pos2(x, config::get().number("layout.bar_height")))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                ui.set_width(w);
                egui::Frame::new()
                    .fill(config::get().color("popup", self.dark))
                    .inner_margin(config::get().number("layout.popup_padding") as i8)
                    .show(ui, |ui| {
                        ui.set_width(w - 2. * config::get().number("layout.popup_padding"));
                        if kind == Menu::Sound {
                            // Areas remember their previous size. Allow the drawer to grow
                            // when streams/devices arrive, rather than scrolling at that size.
                            ui.set_max_height(max_height);
                        }
                        egui::ScrollArea::vertical()
                            .min_scrolled_height(min_height.min(max_height))
                            .max_height(max_height)
                            .show(ui, |ui| {
                                match kind {
                                    Menu::Sound => self.sound(ui),
                                    Menu::Apps => self.launcher(ui),
                                    Menu::Wifi => self.wifi(ui),
                                    Menu::Session => self.session(ui),
                                    Menu::Cpu => self.processes(ui),
                                    Menu::Workspace(_) => self.workspace(ui),
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
        let pinned = !self.query.is_empty()
            || self.selected_network.is_some()
            || self.confirm_logout
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
    fn keyboard(&self) -> bool {
        self.panel.as_ref().is_some_and(|p| {
            p.closing.is_none()
                && (p.kind == Menu::Apps || self.selected_network.is_some() || p.clicked)
        })
    }
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

fn output_row(ui: &mut egui::Ui, selected: bool, name: &str, device: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(
            ui.available_width(),
            config::get().number("layout.control_height"),
        ),
        egui::Sense::click(),
    );
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(
            rect,
            0,
            config::get().color("hover", ui.visuals().dark_mode),
        );
    }
    // Reserve the selection column even for inactive destinations.
    let text_left = rect.left() + 20.;
    if selected {
        ui.painter().circle_filled(
            pos2(rect.left() + 8., rect.center().y),
            2.5,
            ui.visuals().text_color(),
        );
    }
    let split = text_left + (rect.right() - text_left) * 0.52;
    for (text, left, right) in [
        (
            name,
            text_left,
            if device.is_empty() {
                rect.right()
            } else {
                split - 6.
            },
        ),
        (device, split, rect.right()),
    ] {
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(left, rect.top()),
                    pos2(right, rect.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.add(egui::Label::new(text).truncate().selectable(false));
            },
        );
    }
    response.on_hover_text(if device.is_empty() {
        name.to_owned()
    } else {
        format!("{name} · {device}")
    })
}
impl Ui {
    fn launcher(&mut self, ui: &mut egui::Ui) {
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.query)
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
    fn wifi(&mut self, ui: &mut egui::Ui) {
        if let Some(n) = self.selected_network.clone() {
            ui.label(&n.ssid);
            if flat(ui, "Back to networks").clicked() {
                self.selected_network = None;
                self.password.clear();
                self.error.clear();
                return;
            }
            if !n.active && !n.security.is_empty() {
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(!self.reveal)
                        .hint_text("Password")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_search {
                    input.request_focus();
                    self.focus_search = false;
                }
                ui.checkbox(&mut self.reveal, "Show password");
            }
            if flat(ui, if n.active { "Disconnect" } else { "Connect" }).clicked()
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
        ui.horizontal(|ui| {
            if ui.checkbox(&mut enabled, "Wi-Fi").changed() {
                self.services.network(self.serial, Job::Radio(enabled));
            }
            if ui.button("Scan").clicked() {
                self.services.network(self.serial, Job::Scan(true));
            }
        });
        if let Some(s) = &self.network {
            if s.networks.is_empty() {
                ui.label(if enabled {
                    "No networks found"
                } else {
                    "Wi-Fi is off"
                });
            }
            for n in s.networks.clone() {
                let (rect, response) = ui.allocate_exact_size(
                    vec2(
                        ui.available_width(),
                        config::get().number("layout.control_height"),
                    ),
                    egui::Sense::click(),
                );
                if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, 2, ui.visuals().widgets.hovered.bg_fill);
                }
                let text = format!("{}{}", if n.active { "•  " } else { "" }, n.ssid);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Rect::from_min_max(
                        rect.min + vec2(4., 3.),
                        rect.max - vec2(52., 0.),
                    )),
                    |ui| {
                        ui.add(egui::Label::new(text).truncate());
                    },
                );
                ui.painter().text(
                    pos2(rect.right() - 4., rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    n.signal
                        .map(|signal| format!("{signal}%"))
                        .unwrap_or_else(|| "—".into()),
                    egui::FontId::proportional(config::get().number("appearance.font_size")),
                    ui.visuals().weak_text_color(),
                );
                let clicked = response.clicked();
                if let Some(rssi) = n.rssi_dbm {
                    response.on_hover_text(format!("{rssi} dBm"));
                }
                if clicked {
                    self.selected_network = Some(n);
                    self.focus_search = true;
                }
            }
        } else {
            ui.label("Scanning…");
        }
    }
    fn session(&mut self, ui: &mut egui::Ui) {
        if flat(ui, "Lock screen").clicked() {
            let path = std::env::var("HOME").unwrap_or_default() + "/.local/bin/lock-session";
            let command = if std::path::Path::new(&path).exists() {
                format!("exec {}", serde_json::to_string(&path).unwrap())
            } else {
                "exec swaylock".into()
            };
            std::thread::spawn(move || {
                let _ = status::ipc(0, &command);
            });
            self.close();
        }
        if flat(
            ui,
            if self.confirm_logout {
                "Confirm log out"
            } else {
                "Log out…"
            },
        )
        .clicked()
        {
            if self.confirm_logout {
                let _ = status::ipc(0, "exit");
            } else {
                self.confirm_logout = true;
            }
        }
    }
    fn sound(&mut self, ui: &mut egui::Ui) {
        ui.label("Sound");
        if let Some(output) = self.audio.active().cloned() {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(&output.description).truncate());
                if self.icon_button(ui, Icon::Volume(output.muted), true) {
                    self.control(volume::Control::Mute(output.name.clone()));
                }
            });
            let mut v = output.percent() as f32;
            let width = ui.available_width();
            if volume_slider(ui, &mut v, width).changed() {
                self.control(volume::Control::Volume {
                    output: output.name.clone(),
                    channel: None,
                    percent: v.round() as u8,
                });
            }
            ui.separator();
        }
        if self.audio.streams.is_empty() {
            ui.label("No app audio");
        }
        for stream in self.audio.streams.clone() {
            ui.push_id(stream.index, |ui| {
                let track = self
                    .state
                    .extras
                    .tracks
                    .iter()
                    .find(|t| media::player_matches(&stream.application, &t.player))
                    .cloned();
                let (r, _) = ui.allocate_exact_size(
                    vec2(
                        ui.available_width(),
                        config::get().number("layout.audio_row_height"),
                    ),
                    egui::Sense::hover(),
                );
                if let Some(track) = &track
                    && let Some(art) =
                        self.state.extras.artworks.get(&track.art_url).or_else(|| {
                            self.state
                                .extras
                                .artwork
                                .as_ref()
                                .filter(|art| art.url == track.art_url)
                        })
                {
                    let texture = self
                        .textures
                        .entry(track.art_url.clone())
                        .or_insert_with(|| {
                            self.ctx.load_texture(
                                &track.art_url,
                                egui::ColorImage::from_rgba_premultiplied(
                                    [art.pixels.width() as usize, art.pixels.height() as usize],
                                    art.pixels.data(),
                                ),
                                egui::TextureOptions::LINEAR,
                            )
                        });
                    ui.painter().image(
                        texture.id(),
                        Rect::from_min_size(
                            r.min,
                            vec2(
                                config::get().number("layout.artwork_size"),
                                config::get().number("layout.artwork_size"),
                            ),
                        ),
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().image(
                        self.app_icon(&stream.icon),
                        Rect::from_min_size(
                            r.min,
                            vec2(
                                config::get().number("layout.artwork_size"),
                                config::get().number("layout.artwork_size"),
                            ),
                        ),
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
                let name = track
                    .as_ref()
                    .map(|t| t.title.as_str())
                    .unwrap_or(&stream.name);
                let text_rect = Rect::from_min_max(
                    r.min + vec2(config::get().number("layout.audio_summary_offset"), 0.),
                    pos2(
                        r.right() - config::get().number("layout.audio_controls_width"),
                        r.bottom(),
                    ),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                    ui.add(egui::Label::new(name).truncate())
                        .on_hover_text(format!("{} · {}", stream.application, name));
                });
                let levels = self
                    .meters
                    .get(&stream.index)
                    .and_then(|m| m.levels.lock().ok())
                    .map(|l| *l)
                    .unwrap_or([0.; 7]);
                for (i, level) in levels.iter().enumerate() {
                    let h = (level * config::get().number("layout.meter_height") as f64)
                        .max(config::get().number("layout.meter_min_height") as f64)
                        as f32;
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            pos2(
                                r.right() - config::get().number("layout.meter_offset")
                                    + i as f32 * config::get().number("layout.meter_bar_gap"),
                                r.center().y + 10. - h,
                            ),
                            vec2(config::get().number("layout.meter_bar_width"), h),
                        ),
                        0,
                        ui.visuals().text_color(),
                    );
                }
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        pos2(
                            r.right() - config::get().number("layout.media_button_width"),
                            r.top(),
                        ),
                        vec2(
                            config::get().number("layout.media_button_width"),
                            config::get().number("layout.artwork_size"),
                        ),
                    )),
                    |ui| {
                        if self.icon_button(ui, Icon::Volume(stream.muted), true) {
                            self.control(volume::Control::StreamMute(stream.index));
                        }
                    },
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.;
                    let mut v = stream.percent as f32;
                    if volume_slider(
                        ui,
                        &mut v,
                        (ui.available_width()
                            - config::get().number("layout.audio_controls_width"))
                        .max(1.),
                    )
                    .changed()
                    {
                        self.control(volume::Control::StreamVolume(stream.index, v.round() as u8));
                    }
                    ui.add_space(config::get().number("layout.media_button_gap"));
                    for (icon, control, enabled) in [
                        (
                            Icon::Previous,
                            media::Control::Previous,
                            track.as_ref().is_some_and(|t| t.can_previous),
                        ),
                        (
                            if track.as_ref().is_some_and(|t| t.playing) {
                                Icon::Pause
                            } else {
                                Icon::Play
                            },
                            media::Control::Toggle,
                            track.as_ref().is_some_and(|t| t.can_toggle),
                        ),
                        (
                            Icon::Next,
                            media::Control::Next,
                            track.as_ref().is_some_and(|t| t.can_next),
                        ),
                    ] {
                        if self.icon_button(ui, icon, enabled)
                            && let Some(t) = &track
                        {
                            let _ = self.services.media.send(media::Request::Control {
                                panel_id: self.serial,
                                player: t.player.clone(),
                                control,
                            });
                        }
                    }
                });
                ui.add_space(5.);
            });
        }
        ui.separator();
        ui.label("Output");
        for output in self.audio.outputs.clone() {
            let active = output.name == self.audio.default;
            let ports: Vec<_> = output.ports.iter().filter(|p| p.available).collect();
            if ports.is_empty() {
                if output_row(ui, active, &output.description, "").clicked() {
                    self.control(volume::Control::Output(output.name.clone()));
                }
            } else {
                for port in ports {
                    if output_row(
                        ui,
                        active && port.name == output.active_port,
                        &port.description,
                        &output.description,
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
        if let Some(output) = self.audio.active().cloned() {
            ui.collapsing("Channels", |ui| {
                for c in &output.channels {
                    ui.label(&c.name);
                    let mut v = c.percent() as f32;
                    let width = ui.available_width();
                    if volume_slider(ui, &mut v, width).changed() {
                        self.control(volume::Control::Volume {
                            output: output.name.clone(),
                            channel: Some(c.name.clone()),
                            percent: v.round() as u8,
                        });
                    }
                }
            });
        }
    }
    fn processes(&mut self, ui: &mut egui::Ui) {
        ui.label("CPU");
        let width = ui.available_width();
        for (name, pid, cpu, memory) in
            std::iter::once(("Process".into(), "PID".into(), "CPU".into(), "RAM".into())).chain(
                self.cpu.processes.iter().map(|p| {
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
    }
    fn workspace(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.preview else {
            ui.label("Loading…");
            return;
        };
        let (rect, response) = ui.allocate_exact_size(
            vec2(
                self.preview_size[0],
                self.preview_size[1] + config::get().number("preview.title_height"),
            ),
            egui::Sense::click(),
        );
        let name = match self.panel.as_ref().map(|p| &p.kind) {
            Some(Menu::Workspace(name)) => name,
            _ => &s.name,
        };
        ui.painter().text(
            rect.min,
            egui::Align2::LEFT_TOP,
            format!("Workspace {name}"),
            egui::FontId::proportional(config::get().number("appearance.font_size")),
            ui.visuals().text_color(),
        );
        let canvas = Rect::from_min_max(
            rect.min + vec2(0., config::get().number("preview.title_height")),
            rect.max,
        );
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
        painter.multiply_opacity(opacity);
        let scale = workspace_preview::preview_scale(s.rect, self.preview_size);
        painter.rect_filled(canvas, 0, config::get().color("preview_canvas", self.dark));
        for (i, w) in s.windows.iter().enumerate() {
            let rect = Rect::from_min_size(
                canvas.min + vec2((w.rect.x - s.rect.x) * scale, (w.rect.y - s.rect.y) * scale),
                vec2(w.rect.w * scale, w.rect.h * scale),
            );
            painter.rect_filled(
                rect.shrink(1.),
                0,
                config::get().color("preview_window", self.dark),
            );
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
fn sf_mono() -> Option<std::path::PathBuf> {
    fn find(dir: &std::path::Path, depth: u8) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .replace('-', "");
            if matches!(
                name.as_str(),
                "sfmono regular.otf" | "sfmonoregular.otf" | "sfmonoregular.ttf"
            ) {
                return Some(path);
            }
            if depth > 0
                && entry.file_type().is_ok_and(|t| t.is_dir())
                && let Some(path) = find(&path, depth - 1)
            {
                return Some(path);
            }
        }
        None
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    [
        format!("{data}/fonts"),
        format!("{home}/.fonts"),
        "/usr/local/share/fonts".into(),
        "/usr/share/fonts".into(),
    ]
    .iter()
    .find_map(|dir| find(std::path::Path::new(dir), 4))
}
fn volume_slider(ui: &mut egui::Ui, value: &mut f32, width: f32) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = width;
        ui.spacing_mut().slider_rail_height = config::get().number("layout.slider_rail_height");
        ui.spacing_mut().interact_size.y = config::get().number("layout.slider_height");
        ui.visuals_mut().widgets.inactive.bg_fill =
            config::get().color("slider_rail", ui.visuals().dark_mode);
        let response = ui.add(
            egui::Slider::new(value, 0.0..=150.)
                .show_value(false)
                .trailing_fill(true)
                .handle_shape(egui::style::HandleShape::Circle),
        );
        let radius = response.rect.height() / config::get().number("layout.slider_handle_ratio");
        let range = response.rect.x_range().shrink(radius);
        let center = pos2(
            egui::lerp(range, (*value / 150.).clamp(0., 1.)),
            response.rect.center().y,
        );
        ui.painter().circle_filled(
            center,
            radius,
            config::get().color("slider_handle", ui.visuals().dark_mode),
        );
        ui.painter().circle_stroke(
            center,
            radius,
            egui::Stroke::new(
                config::get().number("layout.slider_border_width"),
                config::get().color("slider_border", ui.visuals().dark_mode),
            ),
        );
        response
    })
    .inner
}
