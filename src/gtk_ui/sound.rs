use super::*;
use crate::audio_meter;
use std::sync::{Arc, Mutex};
struct StreamControl {
    index: u32,
    scale: Option<gtk::Scale>,
    mute: gtk::Button,
    previous: gtk::Button,
    toggle: gtk::Button,
    next: gtk::Button,
    cover: gtk::Picture,
    art_url: String,
    levels: Arc<Mutex<[f64; 7]>>,
    meter: gtk::DrawingArea,
    meter_active: Rc<Cell<bool>>,
    capture: Option<audio_meter::Capture>,
}
pub struct Sound {
    pub root: gtk::Box,
    error: gtk::Label,
    master: gtk::Scale,
    subtitle: gtk::Label,
    outputs: gtk::Box,
    streams: gtk::Box,
    stream_signature: RefCell<String>,
    stream_controls: RefCell<Vec<StreamControl>>,
    channels: gtk::Box,
    updating: Rc<Cell<bool>>,
    dragging: Rc<Cell<bool>>,
    signature: RefCell<String>,
    scales: RefCell<Vec<(String, gtk::Scale)>>,
    shell: std::rc::Weak<Shell>,
}
impl Sound {
    fn meter(levels: Arc<Mutex<[f64; 7]>>) -> (gtk::DrawingArea, Rc<Cell<bool>>) {
        let area = gtk::DrawingArea::new();
        area.set_content_width(48);
        area.set_content_height(18);
        area.set_valign(gtk::Align::Center);
        area.set_can_target(false);
        let measured = levels;
        area.set_draw_func(move |area, cr, _, height| {
            #[allow(deprecated)]
            let color = area.style_context().color();
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.85,
            );
            cr.set_line_width(2.5);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            let levels = measured.lock().map(|v| *v).unwrap_or([0.0; 7]);
            for (index, level) in levels.iter().enumerate() {
                let x = 2.0 + index as f64 * 6.5;
                let bottom = height as f64 - 2.0;
                cr.move_to(x, bottom);
                cr.line_to(x, bottom - (1.0 + level * 12.0));
                let _ = cr.stroke();
            }
        });
        let active = Rc::new(Cell::new(false));
        let tick_active = active.clone();
        area.add_tick_callback(move |area, _| {
            if area.is_mapped() && tick_active.get() {
                area.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        (area, active)
    }
    fn normalized(value: &str) -> String {
        value
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    fn stream_matches_player(application: &str, player: &str) -> bool {
        let player = Self::normalized(player);
        let application = Self::normalized(application);
        let application = application
            .strip_prefix("google")
            .or_else(|| application.strip_prefix("mozilla"))
            .unwrap_or(&application);
        !application.is_empty() && player.contains(application)
    }
    fn media_button(
        shell: &std::rc::Weak<Shell>,
        application: &str,
        icon: &str,
        control: media::Control,
    ) -> gtk::Button {
        let button = icon_button(icon, "Playback");
        button.set_visible(false);
        let weak = shell.clone();
        let application = application.to_string();
        button.connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade()
                && let Some(track) = shell.extras.borrow().track.as_ref()
                && Self::stream_matches_player(&application, &track.player)
            {
                let _ = shell.services.media.send(media::Request::Control {
                    panel_id: shell.serial.get(),
                    player: track.player.clone(),
                    control,
                });
            }
        });
        button
    }
    pub fn new(shell: &Rc<Shell>, pinned: Rc<Cell<bool>>) -> Self {
        let root = column(12);
        let title = row(12);
        let h = heading("Audio");
        h.set_hexpand(true);
        title.append(&h);
        let mute = icon_button("audio-volume-muted-symbolic", "Toggle mute");
        title.append(&mute);
        root.append(&title);
        let weak = Rc::downgrade(shell);
        mute.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                let name = s
                    .audio_state
                    .borrow()
                    .as_ref()
                    .and_then(|v| v.active())
                    .map(|o| o.name.clone());
                if let Some(name) = name {
                    s.control(volume::Control::Mute(name));
                }
            }
        });
        let updating = Rc::new(Cell::new(false));
        let dragging = pinned;
        let master = Self::scale(shell, None, None, &updating, &dragging);
        root.append(&master);
        let subtitle = label("");
        root.append(&subtitle);
        root.append(&section("OUTPUT"));
        let outputs = column(4);
        root.append(&scroll(&outputs, 220));
        root.append(&section("APPLICATIONS"));
        let streams = column(7);
        root.append(&scroll(&streams, 260));
        let channels = column(8);
        let expand = gtk::Expander::new(Some("Channels"));
        expand.set_child(Some(&channels));
        root.append(&expand);
        let error = error_label();
        root.append(&error);
        Self {
            error,
            root,
            master,
            subtitle,
            outputs,
            streams,
            stream_signature: RefCell::new("unloaded".into()),
            stream_controls: RefCell::new(vec![]),
            channels,
            updating,
            dragging,
            signature: RefCell::new(String::new()),
            scales: RefCell::new(vec![]),
            shell: Rc::downgrade(shell),
        }
    }
    fn scale(
        shell: &Rc<Shell>,
        channel: Option<String>,
        stream: Option<u32>,
        updating: &Rc<Cell<bool>>,
        dragging: &Rc<Cell<bool>>,
    ) -> gtk::Scale {
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_draw_value(true);
        scale.set_value_pos(gtk::PositionType::Right);
        scale.set_digits(0);
        scale.set_hexpand(true);
        scale.set_increments(1.0, 5.0);
        let weak = Rc::downgrade(shell);
        let updating = updating.clone();
        scale.connect_value_changed(move |scale| {
            if updating.get() {
                return;
            }
            if let Some(s) = weak.upgrade() {
                let name = s
                    .audio_state
                    .borrow()
                    .as_ref()
                    .and_then(|v| v.active())
                    .map(|o| o.name.clone());
                if let Some(output) = name {
                    if let Some(index) = stream {
                        s.control(volume::Control::StreamVolume(
                            index,
                            scale.value().round() as u8,
                        ));
                    } else {
                        s.control(volume::Control::Volume {
                            output,
                            channel: channel.clone(),
                            percent: scale.value().round() as u8,
                        });
                    }
                }
            }
        });
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let d = dragging.clone();
        events.connect_event(move |_, event| {
            match event.event_type() {
                gdk::EventType::ButtonPress => d.set(true),
                gdk::EventType::ButtonRelease => d.set(false),
                _ => {}
            }
            glib::Propagation::Proceed
        });
        scale.add_controller(events);
        scale
    }
    pub fn error(&self, error: &str) {
        set_error(&self.error, error);
    }
    pub fn update(&self, state: &volume::Snapshot, extras: &media::Extras) {
        if self.dragging.get() {
            return;
        }
        set_error(&self.error, "");
        self.updating.set(true);
        if let Some(o) = state.active() {
            self.master.set_value(o.percent() as f64);
            self.subtitle.set_text(&format!(
                "{}{}",
                o.description,
                if o.muted { " · Muted" } else { "" }
            ));
            for (name, scale) in self.scales.borrow().iter() {
                if let Some(c) = o.channels.iter().find(|c| c.name == *name) {
                    scale.set_value(c.percent() as f64);
                }
            }
        }
        let stream_signature = state
            .streams
            .iter()
            .map(|s| format!("{}:{}:{}", s.index, s.name, s.corked))
            .collect::<Vec<_>>()
            .join("|");
        if *self.stream_signature.borrow() != stream_signature {
            self.stream_signature.replace(stream_signature);
            clear(&self.streams);
            self.stream_controls.borrow_mut().clear();
            if let Some(shell) = self.shell.upgrade() {
                if state.streams.is_empty() {
                    let empty = label("No app audio");
                    empty.add_css_class("dim-label");
                    self.streams.append(&empty);
                }
                for stream in &state.streams {
                    let header = row(8);
                    let cover = gtk::Picture::new();
                    cover.set_size_request(28, 28);
                    cover.set_can_shrink(true);
                    cover.set_visible(false);
                    header.append(&cover);
                    let name = label(&stream.name);
                    name.set_hexpand(true);
                    name.set_max_width_chars(34);
                    header.append(&name);
                    let meter_levels = Arc::new(Mutex::new([0.0; 7]));
                    let (meter, meter_active) = Self::meter(meter_levels.clone());
                    header.append(&meter);
                    let mute = icon_button(
                        if stream.muted {
                            "audio-volume-muted-symbolic"
                        } else {
                            "audio-volume-high-symbolic"
                        },
                        "Toggle app volume",
                    );
                    header.append(&mute);
                    let previous = Self::media_button(
                        &self.shell,
                        &stream.application,
                        "media-skip-backward-symbolic",
                        media::Control::Previous,
                    );
                    let toggle = Self::media_button(
                        &self.shell,
                        &stream.application,
                        "media-playback-start-symbolic",
                        media::Control::Toggle,
                    );
                    let next = Self::media_button(
                        &self.shell,
                        &stream.application,
                        "media-skip-forward-symbolic",
                        media::Control::Next,
                    );
                    for button in [&previous, &toggle, &next] {
                        header.append(button);
                    }
                    let weak = self.shell.clone();
                    let index = stream.index;
                    mute.connect_clicked(move |_| {
                        if let Some(shell) = weak.upgrade() {
                            shell.control(volume::Control::StreamMute(index));
                        }
                    });
                    self.streams.append(&header);
                    let scale = if stream.corked {
                        let paused = label("Paused");
                        paused.add_css_class("dim-label");
                        self.streams.append(&paused);
                        None
                    } else {
                        let scale = Self::scale(
                            &shell,
                            None,
                            Some(stream.index),
                            &self.updating,
                            &self.dragging,
                        );
                        scale.set_range(0.0, 150.0);
                        scale.set_value(stream.percent as f64);
                        self.streams.append(&scale);
                        Some(scale)
                    };
                    self.stream_controls.borrow_mut().push(StreamControl {
                        index: stream.index,
                        scale,
                        mute,
                        previous,
                        toggle,
                        next,
                        cover,
                        art_url: String::new(),
                        levels: meter_levels,
                        meter,
                        meter_active,
                        capture: None,
                    });
                }
            }
        }
        for control in self.stream_controls.borrow_mut().iter_mut() {
            if let Some(stream) = state.streams.iter().find(|s| s.index == control.index) {
                if let Some(scale) = &control.scale {
                    scale.set_value(stream.percent as f64);
                }
                control.mute.set_icon_name(if stream.muted {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                });
                control.mute.set_tooltip_text(Some(if stream.muted {
                    "Unmute app"
                } else {
                    "Mute app"
                }));
                if stream.corked {
                    control.capture.take();
                    control.meter_active.set(false);
                    if let Ok(mut levels) = control.levels.lock() {
                        *levels = [0.0; 7];
                    }
                } else if control.capture.is_none() {
                    control.capture =
                        audio_meter::Capture::start(control.levels.clone(), stream.index).ok();
                    control.meter_active.set(control.capture.is_some());
                } else if control
                    .capture
                    .as_mut()
                    .is_some_and(|capture| !capture.is_running())
                {
                    control.capture.take();
                    control.meter_active.set(false);
                    if let Ok(mut levels) = control.levels.lock() {
                        *levels = [0.0; 7];
                    }
                }
                let track = extras.track.as_ref().filter(|track| {
                    Self::stream_matches_player(&stream.application, &track.player)
                });
                if let Some(track) = track {
                    control.previous.set_visible(true);
                    control.next.set_visible(true);
                    control.previous.set_sensitive(track.can_previous);
                    control.next.set_sensitive(track.can_next);
                    control.toggle.set_visible(true);
                    control.toggle.set_icon_name(if track.playing {
                        "media-playback-pause-symbolic"
                    } else {
                        "media-playback-start-symbolic"
                    });
                    control.toggle.set_sensitive(track.can_toggle);
                    if let Some(art) = extras
                        .artwork
                        .as_ref()
                        .filter(|art| art.url == track.art_url)
                        && control.art_url != art.url
                    {
                        control.cover.set_paintable(Some(&texture(&art.pixels)));
                        control.cover.set_visible(true);
                        control.art_url = art.url.clone();
                    }
                } else {
                    control.previous.set_visible(false);
                    control.toggle.set_visible(false);
                    control.next.set_visible(false);
                    control.cover.set_visible(false);
                    control.art_url.clear();
                }
                control.meter.queue_draw();
            }
        }
        let signature = format!(
            "{} {:?}",
            state.default,
            state
                .outputs
                .iter()
                .map(|o| (
                    &o.name,
                    &o.active_port,
                    o.channels.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    o.ports
                        .iter()
                        .map(|p| (&p.name, p.available))
                        .collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
        );
        if *self.signature.borrow() != signature {
            self.signature.replace(signature);
            clear(&self.outputs);
            clear(&self.channels);
            self.scales.borrow_mut().clear();
            if let Some(shell) = self.shell.upgrade() {
                for o in &state.outputs {
                    let b = gtk::Button::with_label(&format!(
                        "{}{}",
                        if o.name == state.default {
                            "✓  "
                        } else {
                            "    "
                        },
                        o.description
                    ));
                    if let Some(label) = b.child().and_downcast::<gtk::Label>() {
                        label.set_xalign(0.0);
                        label.set_max_width_chars(40);
                        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    }
                    if o.name == state.default {
                        b.add_css_class("selected");
                    }
                    let s = self.shell.clone();
                    let name = o.name.clone();
                    b.connect_clicked(move |_| {
                        if let Some(s) = s.upgrade() {
                            s.control(volume::Control::Output(name.clone()));
                        }
                    });
                    self.outputs.append(&b);
                    if o.name == state.default {
                        for p in &o.ports {
                            if !p.available {
                                continue;
                            }
                            let b = gtk::Button::with_label(&format!(
                                "    {}{}",
                                if p.name == o.active_port {
                                    "✓  "
                                } else {
                                    "    "
                                },
                                p.description
                            ));
                            if let Some(label) = b.child().and_downcast::<gtk::Label>() {
                                label.set_xalign(0.0);
                                label.set_max_width_chars(40);
                            }
                            if p.name == o.active_port {
                                b.add_css_class("selected");
                            }
                            let s = self.shell.clone();
                            let name = o.name.clone();
                            let port = p.name.clone();
                            b.connect_clicked(move |_| {
                                if let Some(s) = s.upgrade() {
                                    s.control(volume::Control::Port(name.clone(), port.clone()));
                                }
                            });
                            self.outputs.append(&b);
                        }
                        for c in &o.channels {
                            self.channels.append(&label(&c.name));
                            let scale = Self::scale(
                                &shell,
                                Some(c.name.clone()),
                                None,
                                &self.updating,
                                &self.dragging,
                            );
                            scale.set_value(c.percent() as f64);
                            self.channels.append(&scale);
                            self.scales.borrow_mut().push((c.name.clone(), scale));
                        }
                    }
                }
            }
        }
        self.updating.set(false);
    }
}
