use super::*;
pub struct Sound {
    pub root: gtk::Box,
    error: gtk::Label,
    master: gtk::Scale,
    subtitle: gtk::Label,
    outputs: gtk::Box,
    streams: gtk::Box,
    stream_signature: RefCell<String>,
    stream_controls: RefCell<Vec<(u32, Option<gtk::Scale>, gtk::Button)>>,
    channels: gtk::Box,
    updating: Rc<Cell<bool>>,
    dragging: Rc<Cell<bool>>,
    signature: RefCell<String>,
    scales: RefCell<Vec<(String, gtk::Scale)>>,
    shell: std::rc::Weak<Shell>,
}
impl Sound {
    pub fn new(shell: &Rc<Shell>, pinned: Rc<Cell<bool>>) -> Self {
        let root = column(12);
        let title = row(12);
        let h = heading("Sound");
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
    pub fn update(&self, state: &volume::Snapshot) {
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
                    let name = label(&stream.name);
                    name.set_hexpand(true);
                    name.set_max_width_chars(34);
                    header.append(&name);
                    let mute = icon_button(
                        if stream.muted {
                            "audio-volume-muted-symbolic"
                        } else {
                            "audio-volume-high-symbolic"
                        },
                        "Toggle app volume",
                    );
                    header.append(&mute);
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
                    self.stream_controls
                        .borrow_mut()
                        .push((stream.index, scale, mute));
                }
            }
        }
        for (index, scale, mute) in self.stream_controls.borrow().iter() {
            if let Some(stream) = state.streams.iter().find(|s| s.index == *index) {
                if let Some(scale) = scale {
                    scale.set_value(stream.percent as f64);
                }
                mute.set_icon_name(if stream.muted {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                });
                mute.set_tooltip_text(Some(if stream.muted {
                    "Unmute app"
                } else {
                    "Mute app"
                }));
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
