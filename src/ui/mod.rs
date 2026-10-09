mod keyboard;
mod services;
mod wayland;
use crate::{
    audio_meter, icons::Icon, launcher, media, network, popup_motion::Dismissal, status, volume,
    workspace_preview,
};
use egui::{Color32, Pos2, Rect, pos2, vec2};
use services::{Event, Job, Services};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use wayland::run;

#[derive(Clone, PartialEq, Eq)]
enum Menu {
    Apps,
    Sound,
    Wifi,
    Session,
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
    network: Option<network::Snapshot>,
    apps: Vec<launcher::Entry>,
    panel: Option<Panel>,
    preview: Option<workspace_preview::Snapshot>,
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
        style.visuals.widgets.active.bg_fill =
            Color32::from_gray(if options.dark { 90 } else { 210 });
        style.visuals.selection.bg_fill = Color32::from_gray(if options.dark { 190 } else { 110 });
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
        style.spacing.item_spacing = vec2(6., 5.);
        style.spacing.button_padding = vec2(5., 3.);
        style.spacing.interact_size = vec2(24., 22.);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(12.));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(12.));
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
            network: None,
            apps: vec![],
            panel: None,
            preview: None,
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
        let _ = self.services.preview.send(None);
    }
    fn finish_close(&mut self) {
        self.close();
        self.panel = None;
        self.panel_rect = Rect::NOTHING;
        self.preview = None;
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
        self.finish_close();
        self.serial += 1;
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
                let _ = self
                    .services
                    .preview
                    .send(Some((name.clone(), self.ctx.pixels_per_point())));
            }
            Menu::Session => {}
        }
        self.panel = Some(Panel {
            kind,
            x,
            opened: Instant::now(),
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
        while let Ok(claim) = self.services.popup_events.try_recv() {
            if claim > self.claim {
                self.finish_close();
                self.hover = None;
            }
        }
        while let Ok(mut s) = self.services.statuses.try_recv() {
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
                media::Update::Audio(sources) => extras.audio_sources = sources,
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
        self.state.update_audio();
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
        while let Ok(s) = self.services.preview_events.try_recv() {
            if self
                .panel
                .as_ref()
                .is_some_and(|p| p.kind == Menu::Workspace(s.name.clone()))
            {
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
                        meter.retry = Instant::now() + Duration::from_secs(2);
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
                Icon::Wifi(_) => 7,
                Icon::Charging => 8,
            }
        );
        let ctx = &self.ctx;
        self.textures
            .entry(key.clone())
            .or_insert_with(|| {
                let mut p = tiny_skia::Pixmap::new(48, 48).unwrap();
                let c = if self.dark { 0.92 } else { 0.16 };
                crate::icons::draw(
                    &mut p,
                    icon,
                    0.,
                    0.,
                    24.,
                    2.,
                    tiny_skia::Color::from_rgba(c, c, c, 1.).unwrap(),
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
            egui::Button::image(image)
                .frame(false)
                .min_size(vec2(24., 22.)),
        )
        .clicked()
    }
    fn paint_bar(&mut self, ui: &mut egui::Ui, width: f32) -> Vec<(Rect, Menu)> {
        let bg = Color32::from_gray(if self.dark { 30 } else { 242 });
        ui.painter()
            .rect_filled(Rect::from_min_size(Pos2::ZERO, vec2(width, 28.)), 0, bg);
        let mut targets = vec![];
        let button = |targets: &mut Vec<(Rect, Menu)>,
                      this: &mut Self,
                      ui: &mut egui::Ui,
                      rect: Rect,
                      text: &str,
                      icon: Option<Icon>,
                      kind: Menu| {
            let response = ui.interact(
                rect,
                egui::Id::new(("bar", format!("{text}{:?}", rect.min))),
                egui::Sense::click(),
            );
            if response.hovered() {
                ui.painter().rect_filled(
                    rect.shrink2(vec2(0., 2.)),
                    3,
                    Color32::from_gray(if this.dark { 48 } else { 222 }),
                );
            }
            let mut text_rect = rect.shrink2(vec2(4., 0.));
            if let Some(icon) = icon {
                ui.painter().image(
                    this.icon(icon),
                    Rect::from_center_size(pos2(rect.left() + 16., 14.), vec2(15., 15.)),
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
                text_rect.min.x = rect.left() + 30.;
            }
            if !text.is_empty() {
                let mut job = egui::text::LayoutJob::simple(
                    text.into(),
                    egui::FontId::proportional(12.),
                    ui.visuals().text_color(),
                    text_rect.width(),
                );
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                let galley = ui.painter().layout_job(job);
                ui.painter().galley(
                    text_rect.center() - galley.size() / 2.,
                    galley,
                    ui.visuals().text_color(),
                );
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
            Rect::from_min_size(pos2(12., 0.), vec2(30., 28.)),
            "",
            None,
            Menu::Session,
        );
        for y in [11., 15.] {
            for x in [24., 28.] {
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
            Rect::from_min_size(pos2(47., 0.), vec2(44., 28.)),
            "Apps",
            None,
            Menu::Apps,
        );
        let mut x = 99.;
        for w in self.state.workspaces.clone() {
            let r = Rect::from_min_size(pos2(x, 0.), vec2(40., 28.));
            let response = ui.interact(
                r,
                egui::Id::new(("workspace", &w.name)),
                egui::Sense::click(),
            );
            ui.painter().rect_stroke(
                r.shrink2(vec2(1., 3.)),
                3,
                egui::Stroke::new(
                    1.0_f32,
                    Color32::from_gray(if self.dark { 66 } else { 205 }),
                ),
                egui::StrokeKind::Inside,
            );
            if w.focused || response.hovered() {
                ui.painter().rect_filled(
                    r.shrink2(vec2(1., 3.)),
                    3,
                    Color32::from_gray(if self.dark { 52 } else { 220 }),
                );
            }
            let color = if w.urgent {
                Color32::from_rgb(200, 70, 70)
            } else {
                ui.visuals().text_color()
            };
            ui.painter().text(
                pos2(x + 12., 14.),
                egui::Align2::CENTER_CENTER,
                &w.name,
                egui::FontId::proportional(12.),
                color,
            );
            let amount = self.ctx.animate_bool_with_time(
                egui::Id::new(("audible", &w.name)),
                w.audible,
                0.12,
            );
            if amount > 0. {
                let r = Rect::from_center_size(
                    pos2(x + 29., 14.),
                    vec2(12., 12.) * (0.96 + 0.04 * amount),
                );
                ui.painter().image(
                    self.icon(Icon::Volume(false)),
                    r,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::from_rgba_unmultiplied(65, 190, 140, (255. * amount) as u8),
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
            x += 42.;
        }
        let clock = chrono::Local::now()
            .format("%a %b %-d   %-I:%M %p")
            .to_string();
        let clock_width = ui
            .painter()
            .layout_no_wrap(
                clock.clone(),
                egui::FontId::proportional(12.),
                ui.visuals().text_color(),
            )
            .size()
            .x;
        ui.painter().text(
            pos2(width - 12., 14.),
            egui::Align2::RIGHT_CENTER,
            clock,
            egui::FontId::proportional(12.),
            ui.visuals().text_color(),
        );
        let right = width - 12. - clock_width - 8.;
        let battery = self
            .state
            .battery
            .map(|(p, _)| format!("{p}%"))
            .unwrap_or_default();
        ui.painter().text(
            pos2(right, 14.),
            egui::Align2::RIGHT_CENTER,
            battery,
            egui::FontId::proportional(12.),
            ui.visuals().text_color(),
        );
        if let Some((percent, charging)) = self.state.battery {
            let rect = Rect::from_min_size(pos2(right - 59., 9.), vec2(21., 10.));
            let color = ui.visuals().text_color();
            ui.painter().rect_stroke(
                rect,
                2,
                egui::Stroke::new(1.0_f32, color),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(pos2(rect.right() + 1., 12.), vec2(2., 4.)),
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
                        vec2(17. * percent as f32 / 100., 6.),
                    ),
                    1,
                    color,
                );
            }
        }
        let wifi_name = self.state.extras.wifi_name.clone().unwrap_or_default();
        let wifi_width = 32.
            + ui.painter()
                .layout_no_wrap(
                    wifi_name.clone(),
                    egui::FontId::proportional(12.),
                    ui.visuals().text_color(),
                )
                .size()
                .x
                .min(80.);
        let wifi_x = right - 62. - wifi_width;
        button(
            &mut targets,
            self,
            ui,
            Rect::from_min_size(pos2(wifi_x, 0.), vec2(wifi_width, 28.)),
            &wifi_name,
            Some(Icon::Wifi(self.state.extras.wifi_signal)),
            Menu::Wifi,
        );
        let sound_x = wifi_x - 36.;
        let source = match self.audio.streams.as_slice() {
            [s] => s.name.clone(),
            [] => String::new(),
            s => format!("{} sources", s.len()),
        };
        let r = Rect::from_min_max(pos2((sound_x - 160.).max(x), 0.), pos2(sound_x + 32., 28.));
        button(&mut targets, self, ui, r, "", None, Menu::Sound);
        ui.painter().image(
            self.icon(Icon::Volume(self.audio.active().is_some_and(|o| o.muted))),
            Rect::from_center_size(pos2(sound_x + 16., 14.), vec2(15., 15.)),
            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
            Color32::WHITE,
        );
        let text_rect = Rect::from_min_max(r.min + vec2(4., 0.), pos2(sound_x - 4., 28.));
        let mut job = egui::text::LayoutJob::simple(
            source,
            egui::FontId::proportional(12.),
            ui.visuals().text_color(),
            text_rect.width(),
        );
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            text_rect.center() - galley.size() / 2.,
            galley,
            ui.visuals().text_color(),
        );
        let title_bounds = Rect::from_min_max(pos2(x + 8., 0.), pos2(r.min.x - 8., 28.));
        if title_bounds.width() > 60. {
            let painter = ui.painter().with_clip_rect(title_bounds);
            painter.text(
                pos2(
                    (width / 2.).clamp(title_bounds.min.x, title_bounds.max.x),
                    14.,
                ),
                egui::Align2::CENTER_CENTER,
                &self.state.app,
                egui::FontId::proportional(12.),
                ui.visuals().text_color(),
            );
        }
        targets
    }
    fn frame(&mut self, ctx: &egui::Context, width: f32, height: f32) {
        let now = Instant::now();
        let mut targets = vec![];
        egui::Area::new(egui::Id::new("bar"))
            .fixed_pos(Pos2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(vec2(width, 28.));
                targets = self.paint_bar(ui, width);
            });
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let candidate = pointer.and_then(|p| targets.iter().find(|(r, _)| r.contains(p)).cloned());
        if let Some((rect, kind)) = candidate {
            if self.blocked_hover.as_ref() != Some(&kind) {
                if self.hover.as_ref().is_none_or(|(k, _)| *k != kind) {
                    self.hover = Some((kind.clone(), now));
                }
                if self
                    .hover
                    .as_ref()
                    .is_some_and(|(_, t)| now.duration_since(*t) >= Duration::from_millis(220))
                {
                    self.open(kind, rect.center().x, false);
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
        let w = if matches!(kind, Menu::Sound | Menu::Workspace(_)) {
            336.
        } else {
            320.
        };
        let x = (panel.x - w / 2.).clamp(8., (width - w - 8.).max(8.));
        let opacity = panel
            .closing
            .map(|t| 1. - now.duration_since(t).as_secs_f32() / 0.14)
            .unwrap_or_else(|| (now.duration_since(panel.opened).as_secs_f32() / 0.14).min(1.));
        if opacity <= 0. && panel.closing.is_some() {
            self.finish_close();
            return;
        }
        let min_height = match &kind {
            Menu::Apps => {
                let query = self.query.to_lowercase();
                28. + (self
                    .apps
                    .iter()
                    .filter(|a| a.search.contains(&query))
                    .count() as f32
                    * 29.)
                    .min(420.)
            }
            Menu::Workspace(_) => self
                .preview
                .as_ref()
                .map(|s| 28. + (s.rect.h * workspace_preview::preview_scale(s.rect)).max(60.))
                .unwrap_or(0.),
            _ => 0.,
        };
        let shown = egui::Area::new(egui::Id::new(("panel", self.serial)))
            .fixed_pos(pos2(x, 28.))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                ui.set_width(w);
                egui::Frame::new()
                    .fill(Color32::from_gray(if self.dark { 34 } else { 247 }))
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(w - 24.);
                        egui::ScrollArea::vertical()
                            .min_scrolled_height(min_height.min((height - 58.).max(100.)))
                            .max_height((height - 58.).max(100.))
                            .show(ui, |ui| {
                                match kind {
                                    Menu::Sound => self.sound(ui),
                                    Menu::Apps => self.launcher(ui),
                                    Menu::Wifi => self.wifi(ui),
                                    Menu::Session => self.session(ui),
                                    Menu::Workspace(_) => self.workspace(ui),
                                }
                                if !self.error.is_empty() {
                                    ui.add(egui::Label::new(&self.error).wrap());
                                }
                            });
                    });
            });
        self.panel_rect = shown.response.rect;
        let inside = pointer.is_some_and(|p| p.y < 28. || self.panel_rect.contains(p));
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
            if panel.dismissal.progress(now) >= 1. {
                self.finish_close();
            } else if panel.dismissal.progress(now) > 0. {
                let elapsed = panel.dismissal.progress(now) * 0.14;
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
            .min_size(vec2(ui.available_width(), 24.)),
    )
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
            .max_height(420.)
            .show(ui, |ui| {
                for (i, app) in filtered.iter().enumerate() {
                    let response = ui.add(
                        egui::Button::new(&app.name)
                            .frame(i == self.selection)
                            .min_size(vec2(ui.available_width(), 24.)),
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
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 24.), egui::Sense::click());
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
                    egui::FontId::proportional(12.),
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
                let (r, _) = ui.allocate_exact_size(vec2(312., 30.), egui::Sense::hover());
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
                        Rect::from_min_size(r.min, vec2(28., 28.)),
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().image(
                        self.app_icon(&stream.icon),
                        Rect::from_min_size(r.min, vec2(28., 28.)),
                        Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
                let name = track
                    .as_ref()
                    .map(|t| t.title.as_str())
                    .unwrap_or(&stream.name);
                let text_rect =
                    Rect::from_min_max(r.min + vec2(36., 0.), pos2(r.right() - 88., r.bottom()));
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
                    let h = (level * 19.).max(2.) as f32;
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            pos2(r.right() - 80. + i as f32 * 7., r.center().y + 10. - h),
                            vec2(2., h),
                        ),
                        0,
                        ui.visuals().text_color(),
                    );
                }
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        pos2(r.right() - 24., r.top()),
                        vec2(24., 28.),
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
                    if volume_slider(ui, &mut v, 224.).changed() {
                        self.control(volume::Control::StreamVolume(stream.index, v.round() as u8));
                    }
                    ui.add_space(4.);
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
        for output in self.audio.outputs.clone() {
            if flat(
                ui,
                format!(
                    "{}{}",
                    if output.name == self.audio.default {
                        "•  "
                    } else {
                        ""
                    },
                    output.description
                ),
            )
            .clicked()
            {
                self.control(volume::Control::Output(output.name.clone()));
            }
            if output.name == self.audio.default {
                for port in output.ports.iter().filter(|p| p.available) {
                    if flat(
                        ui,
                        format!(
                            "    {}{}",
                            if port.name == output.active_port {
                                "•  "
                            } else {
                                ""
                            },
                            port.description
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
    }
    fn workspace(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.preview else {
            ui.label("Loading…");
            return;
        };
        ui.label(format!("Workspace {}", s.name));
        let scale = workspace_preview::preview_scale(s.rect);
        let (canvas, _) = ui.allocate_exact_size(
            vec2(312., (s.rect.h * scale).max(60.)),
            egui::Sense::hover(),
        );
        let painter = ui.painter().with_clip_rect(canvas);
        painter.rect_filled(canvas, 0, Color32::from_gray(48));
        for (i, w) in s.windows.iter().enumerate() {
            let rect = Rect::from_min_size(
                canvas.min + vec2((w.rect.x - s.rect.x) * scale, (w.rect.y - s.rect.y) * scale),
                vec2(w.rect.w * scale, w.rect.h * scale),
            );
            painter.rect_filled(rect.shrink(1.), 0, Color32::from_gray(72));
            if w.pixels.is_some() {
                let key = format!("preview:{i}");
                let Some(texture) = self.textures.get(&key) else {
                    continue;
                };
                painter.image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
            } else {
                painter.with_clip_rect(rect.intersect(canvas)).text(
                    rect.left_bottom() + vec2(3., -4.),
                    egui::Align2::LEFT_BOTTOM,
                    &w.title,
                    egui::FontId::proportional(10.),
                    Color32::WHITE,
                );
            }
        }
        if !s.message.is_empty() {
            ui.label(&s.message);
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
        ui.spacing_mut().slider_rail_height = 3.;
        ui.spacing_mut().interact_size.y = 18.;
        ui.visuals_mut().widgets.inactive.bg_fill =
            Color32::from_gray(if ui.visuals().dark_mode { 65 } else { 215 });
        let response = ui.add(
            egui::Slider::new(value, 0.0..=150.)
                .show_value(false)
                .trailing_fill(true)
                .handle_shape(egui::style::HandleShape::Circle),
        );
        let radius = response.rect.height() / 2.5;
        let range = response.rect.x_range().shrink(radius);
        let center = pos2(
            egui::lerp(range, (*value / 150.).clamp(0., 1.)),
            response.rect.center().y,
        );
        ui.painter().circle_filled(
            center,
            radius,
            Color32::from_gray(if ui.visuals().dark_mode { 225 } else { 252 }),
        );
        ui.painter().circle_stroke(
            center,
            radius,
            egui::Stroke::new(
                0.7_f32,
                Color32::from_gray(if ui.visuals().dark_mode { 110 } else { 160 }),
            ),
        );
        response
    })
    .inner
}
