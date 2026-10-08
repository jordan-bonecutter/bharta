use super::*;
pub struct Sound {
    pub root: gtk::Box,
    error: gtk::Label,
    master: gtk::Scale,
    subtitle: gtk::Label,
    outputs: gtk::Box,
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
        let master = Self::scale(shell, None, &updating, &dragging);
        root.append(&master);
        let subtitle = label("");
        root.append(&subtitle);
        root.append(&section("OUTPUT"));
        let outputs = column(4);
        root.append(&scroll(&outputs, 220));
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
                    s.control(volume::Control::Volume {
                        output,
                        channel: channel.clone(),
                        percent: scale.value().round() as u8,
                    });
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
