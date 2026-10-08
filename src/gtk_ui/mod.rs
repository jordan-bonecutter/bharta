mod font;
mod menus;
mod preview;
mod services;
mod sound;
mod widgets;
use crate::{Options, media, status, volume};
use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use services::{Event, Services};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};
use widgets::*;

struct Menu {
    pop: gtk::Popover,
    body: gtk::Box,
    kind: String,
    id: u64,
    claim: u128,
    pinned: Rc<Cell<bool>>,
    dismissal: crate::popup_motion::Dismissal,
    opened: Instant,
    closing: Cell<Option<Instant>>,
    hover_opened: Cell<bool>,
}
struct Shell {
    window: gtk::ApplicationWindow,
    services: Services,
    workspaces: gtk::Box,
    names: RefCell<Vec<String>>,
    app: gtk::Label,
    music: gtk::Button,
    track_label: gtk::Button,
    wifi: gtk::Button,
    battery: gtk::Label,
    battery_icon: gtk::DrawingArea,
    clock: gtk::Label,
    menu: RefCell<Option<Menu>>,
    serial: Cell<u64>,
    remapping: Cell<bool>,
    hover_targets: RefCell<Vec<(glib::WeakRef<gtk::Button>, String)>>,
    hover_candidate: RefCell<Option<(String, Instant)>>,
    hover_activated: Cell<bool>,
    sound: RefCell<Option<sound::Sound>>,
    audio_state: RefCell<Option<volume::Snapshot>>,
    audio_busy: Cell<bool>,
    pending: RefCell<std::collections::VecDeque<volume::Control>>,
    extras: RefCell<media::Extras>,
}
fn audio_source_label(streams: &[volume::Stream], track: Option<&media::Track>) -> String {
    let track_name = || {
        track.map(|track| {
            format!(
                "{}{}",
                track.title,
                if track.artist.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", track.artist)
                }
            )
        })
    };
    match streams {
        [stream] => track_name().unwrap_or_else(|| stream.name.clone()),
        [] => track_name().unwrap_or_default(),
        sources => format!("{} sources", sources.len()),
    }
}
pub fn run(options: Options) -> anyhow::Result<()> {
    let font_family = options.font.as_deref().map(font::register).transpose()?;
    gtk::init()?;
    anyhow::ensure!(
        gtk4_layer_shell::is_supported(),
        "GTK layer shell is unavailable; run bharta inside Sway"
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&format!(
        "{}\n{}",
        if options.dark {
            include_str!("dark.css")
        } else {
            include_str!("light.css")
        },
        include_str!("theme.css")
    ));
    if let Some(family) = font_family {
        let font_css = gtk::CssProvider::new();
        font_css.load_from_data(&format!(
            "window.bharta, popover.bharta-menu {{ font-family: {}; }}",
            serde_json::to_string(&family)?
        ));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &font_css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let application = gtk::Application::builder()
        .application_id("org.bharta.Bar")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("bharta")
            .default_height(28)
            .build();
        window.add_css_class("bharta");
        window.init_layer_shell();
        window.set_namespace(Some("bharta"));
        window.set_layer(Layer::Top);
        for edge in [Edge::Top, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(28);
        window.set_keyboard_mode(KeyboardMode::None);
        let monitors = gdk::Display::default().unwrap().monitors();
        for i in 0..monitors.n_items() {
            if let Some(m) = monitors.item(i).and_downcast::<gdk::Monitor>()
                && options
                    .output
                    .as_deref()
                    .is_none_or(|o| m.connector().as_deref() == Some(o))
            {
                window.set_monitor(Some(&m));
                break;
            }
        }
        let bar = gtk::CenterBox::new();
        bar.add_css_class("bar");
        let left = row(3);
        let right = row(6);
        let workspaces = row(2);
        let session = icon_button("view-grid-symbolic", "Session");
        session.set_tooltip_text(Some("Session"));
        let apps = gtk::Button::with_label("Apps");
        left.append(&session);
        left.append(&apps);
        left.append(&workspaces);
        let app = label("");
        app.set_max_width_chars(40);
        app.add_css_class("app-title");
        let music = icon_button("audio-volume-high-symbolic", "Audio sources");
        let track_label = gtk::Button::new();
        let wifi = icon_button("network-wireless-symbolic", "Wi-Fi");
        let battery = label("");
        let battery_icon = gtk::DrawingArea::new();
        battery_icon.set_content_width(26);
        battery_icon.set_content_height(18);
        let clock = label("");
        right.append(&track_label);
        right.append(&music);
        right.append(&wifi);
        right.append(&battery_icon);
        right.append(&battery);
        right.append(&clock);
        bar.set_start_widget(Some(&left));
        bar.set_center_widget(Some(&app));
        bar.set_end_widget(Some(&right));
        window.set_child(Some(&bar));
        let shell = Rc::new(Shell {
            window,
            services: Services::new(options.output.clone()),
            workspaces,
            names: RefCell::new(vec![]),
            app,
            music,
            track_label,
            wifi,
            battery,
            battery_icon,
            clock,
            menu: RefCell::new(None),
            serial: Cell::new(0),
            remapping: Cell::new(false),
            hover_targets: RefCell::new(Vec::new()),
            hover_candidate: RefCell::new(None),
            hover_activated: Cell::new(false),
            sound: RefCell::new(None),
            audio_state: RefCell::new(None),
            audio_busy: Cell::new(false),
            pending: RefCell::new(std::collections::VecDeque::new()),
            extras: RefCell::new(media::Extras::default()),
        });
        for (button, kind) in [
            (&session, "session"),
            (&apps, "apps"),
            (&shell.music, "sound"),
            (&shell.wifi, "network"),
        ] {
            let s = Rc::downgrade(&shell);
            button.connect_clicked(move |b| {
                if let Some(s) = s.upgrade() {
                    s.open(b, kind);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&shell);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(s) = weak.upgrade() {
                    s.close();
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        shell.window.add_controller(keys);
        shell.window.present();
        shell.add_hover_menu(&session, "session");
        shell.add_hover_menu(&apps, "apps");
        shell.add_hover_menu(&shell.music, "sound");
        shell.add_hover_menu(&shell.track_label, "sound");
        shell.add_hover_menu(&shell.wifi, "network");
        let weak = Rc::downgrade(&shell);
        shell.track_label.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.open(&s.music, "sound");
            }
        });
        let s = shell.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            s.tick();
            glib::ControlFlow::Continue
        });
        if options.smoke {
            let app = application.clone();
            glib::timeout_add_local_once(Duration::from_secs(3), move || app.quit());
        }
    });
    application.run_with_args::<&str>(&[]);
    Ok(())
}
impl Shell {
    fn add_hover_menu(&self, button: &gtk::Button, kind: &str) {
        button.set_has_tooltip(false);
        self.hover_targets
            .borrow_mut()
            .push((button.downgrade(), kind.into()));
    }
    fn update_hover(self: &Rc<Self>) {
        let (surface, x, y) = gdk::Display::default()
            .and_then(|d| d.default_seat())
            .and_then(|s| s.pointer())
            .map(|p| p.surface_at_position())
            .unwrap_or((None, 0.0, 0.0));
        // Popover grabs suppress widget enter/PRELIGHT events. Hit-test GTK's
        // actual button allocations using the parent surface's pointer position.
        let mut target = None;
        self.hover_targets.borrow_mut().retain(|(weak, kind)| {
            let Some(button) = weak.upgrade() else {
                return false;
            };
            if surface.is_some()
                && surface == self.window.surface()
                && button.is_visible()
                && button.is_sensitive()
                && button.compute_bounds(&self.window).is_some_and(|r| {
                    r.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                })
            {
                target = Some((button, kind.clone()));
            }
            true
        });
        let Some((button, kind)) = target else {
            self.hover_candidate.borrow_mut().take();
            self.hover_activated.set(false);
            return;
        };
        let now = Instant::now();
        let ready = {
            let mut candidate = self.hover_candidate.borrow_mut();
            match candidate.as_ref() {
                Some((previous, since)) if previous == &kind => {
                    now.duration_since(*since) >= Duration::from_millis(220)
                }
                _ => {
                    self.hover_activated.set(false);
                    *candidate = Some((kind.clone(), now));
                    false
                }
            }
        };
        // A click owns this visit to the button: do not reopen a menu the
        // user just toggled closed while their pointer remains stationary.
        if button.state_flags().contains(gtk::StateFlags::ACTIVE) {
            self.hover_activated.set(true);
        }
        if ready
            && !self.hover_activated.replace(true)
            && self.menu.borrow().as_ref().is_none_or(|m| m.kind != kind)
        {
            self.open(&button, &kind);
            if let Some(m) = self.menu.borrow().as_ref() {
                m.hover_opened.set(true);
                m.pop.set_autohide(false);
            }
            // Apps is a type-to-search surface, including when opened by hover.
            self.window.set_keyboard_mode(if kind == "apps" {
                KeyboardMode::Exclusive
            } else {
                KeyboardMode::None
            });
            self.window.queue_draw();
        }
    }
    fn close(&self) {
        if let Some(m) = self.menu.borrow().as_ref() {
            m.closing.set(Some(Instant::now()));
            m.pop.set_can_target(false);
        }
        let _ = self.services.preview.send(None);
    }
    fn finish_close(&self) {
        let menu = self.menu.borrow_mut().take();
        if let Some(m) = menu {
            m.pop.popdown();
            m.pop.unparent();
        }
        self.sound.borrow_mut().take();
        let _ = self.services.preview.send(None);
        self.window.set_keyboard_mode(KeyboardMode::None);
        self.window.queue_draw();
    }
    fn promote_menu(self: &Rc<Self>, id: u64) -> bool {
        let promoted = self
            .menu
            .borrow()
            .as_ref()
            .is_some_and(|m| m.id == id && m.hover_opened.replace(false));
        if !promoted {
            return false;
        }
        // Keep the existing content, opacity and animation clock. Changing a
        // mapped popover's autohide mode also remaps its Wayland surface, so
        // only enable keyboard focus here; retain hover dismissal and Escape.
        self.window.set_keyboard_mode(KeyboardMode::Exclusive);
        self.window.queue_draw();
        true
    }
    fn open(self: &Rc<Self>, button: &gtk::Button, kind: &str) {
        if self.menu.borrow().as_ref().is_some_and(|m| m.kind == kind) {
            let id = self.menu.borrow().as_ref().map(|m| m.id).unwrap();
            if self.promote_menu(id) {
                return;
            }
            // A click explicitly toggled this menu closed. Keep hover from
            // reopening it until the pointer leaves the trigger.
            self.hover_activated.set(true);
            self.close();
            return;
        }
        self.finish_close();
        let claim = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let _ = self
            .services
            .status
            .send(status::Update::AnnouncePopup(claim));
        let id = self.serial.get() + 1;
        self.serial.set(id);
        let body = column(12);
        body.add_css_class("menu-content");
        body.set_width_request(340);
        let pop = gtk::Popover::new();
        pop.add_css_class("bharta-menu");
        pop.set_has_arrow(false);
        pop.set_autohide(["apps", "network", "sound", "session"].contains(&kind));
        pop.set_position(gtk::PositionType::Bottom);
        pop.set_parent(button);
        pop.set_child(Some(&body));
        let pinned = Rc::new(Cell::new(false));
        self.menu.replace(Some(Menu {
            pop: pop.clone(),
            body: body.clone(),
            kind: kind.into(),
            id,
            claim,
            pinned: pinned.clone(),
            dismissal: crate::popup_motion::Dismissal::opened(Instant::now()),
            opened: Instant::now(),
            closing: Cell::new(None),
            hover_opened: Cell::new(false),
        }));
        if ["apps", "network", "sound", "session"].contains(&kind) {
            self.window.set_keyboard_mode(KeyboardMode::Exclusive);
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(s) = weak.upgrade() {
                    s.close();
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        pop.add_controller(keys);
        let clicks = gtk::EventControllerLegacy::new();
        clicks.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        clicks.connect_event(move |_, event| {
            if event.event_type() == gdk::EventType::ButtonRelease
                && let Some(s) = weak.upgrade()
            {
                s.promote_menu(id);
            }
            glib::Propagation::Proceed
        });
        pop.add_controller(clicks);

        match kind {
            "sound" => {
                let view = sound::Sound::new(self, pinned);
                body.append(&view.root);
                if let Some(state) = self.audio_state.borrow().as_ref() {
                    view.update(state, &self.extras.borrow());
                }
                self.sound.replace(Some(view));
                let _ = self.services.volume.send(volume::Request::Refresh);
            }
            "apps" => {
                self.services.apps(id);
                body.append(&heading("Applications"));
            }
            "network" => {
                body.append(&heading("Wi-Fi"));
                self.services.network(id, services::Job::Scan(false));
            }
            "session" => self.session_menu(&body, pinned),
            _ => {
                body.append(&label("Loading preview…"));
                let _ = self.services.preview.send(Some(kind.to_owned()));
            }
        }
        let weak = Rc::downgrade(self);
        pop.connect_closed(move |pop| {
            if let Some(s) = weak.upgrade() {
                if s.remapping.get() {
                    return;
                }
                let should_fade = s
                    .menu
                    .borrow()
                    .as_ref()
                    .is_some_and(|m| m.id == id && m.closing.get().is_none());
                if should_fade {
                    // GTK releases its outside-click grab before emitting closed.
                    // Remap without a grab for the short visual exit transition.
                    pop.set_autohide(false);
                    s.close();
                    pop.popup();
                }
            }
        });
        pop.set_opacity(0.0);
        // Commit the parent's keyboard mode before the popup requests its grab.
        // Grabbing first leaves a keyboard-less focus target on Sway.
        self.window.add_tick_callback(move |_, _| {
            let pop = pop.clone();
            glib::idle_add_local_once(move || {
                if pop.parent().is_some() {
                    pop.popup();
                }
            });
            glib::ControlFlow::Break
        });
        self.window.queue_draw();
    }
    fn control(&self, control: volume::Control) {
        if let Some(state) = self.audio_state.borrow_mut().as_mut() {
            volume::preview(state, &control);
        }
        if self.audio_busy.replace(true) {
            let mut queue = self.pending.borrow_mut();
            let replace_last = match (queue.back(), &control) {
                (
                    Some(volume::Control::Volume {
                        output: a,
                        channel: ac,
                        ..
                    }),
                    volume::Control::Volume {
                        output: b,
                        channel: bc,
                        ..
                    },
                ) => a == b && ac == bc,
                (
                    Some(volume::Control::StreamVolume(a, _)),
                    volume::Control::StreamVolume(b, _),
                ) => a == b,
                _ => false,
            };
            if replace_last {
                queue.pop_back();
            }
            queue.push_back(control);
        } else {
            let _ = self
                .services
                .volume
                .send(volume::Request::Control(1, control));
        }
    }
    fn tick(self: &Rc<Self>) {
        while let Ok(claim) = self.services.popup_events.try_recv() {
            if self.menu.borrow().as_ref().is_some_and(|m| claim > m.claim) {
                self.finish_close();
            }
        }
        while let Ok(mut state) = self.services.statuses.try_recv() {
            state.extras = self.extras.borrow().clone();
            state.update_audio();
            self.update_status(&state);
        }
        while let Ok(update) = self.services.volume_events.try_recv() {
            if update.completed.is_some() {
                self.audio_busy.set(false);
                if let Some(control) = self.pending.borrow_mut().pop_front() {
                    self.audio_busy.set(true);
                    let _ = self
                        .services
                        .volume
                        .send(volume::Request::Control(1, control));
                }
            }
            if let Err(error) = &update.result
                && let Some(view) = self.sound.borrow().as_ref()
            {
                view.error(error);
            }
            if let Ok(state) = update.result
                && !self.audio_busy.get()
            {
                if let Some(view) = self.sound.borrow().as_ref() {
                    view.update(&state, &self.extras.borrow());
                }
                if let Some(o) = state.active() {
                    self.music.set_icon_name(if o.muted {
                        "audio-volume-muted-symbolic"
                    } else {
                        "audio-volume-high-symbolic"
                    });
                    self.music
                        .set_tooltip_text(Some(&format!("Audio · {}%", o.percent())));
                }
                self.audio_state.replace(Some(state));
                self.update_audio_label();
            }
        }
        while let Ok(event) = self.services.media_events.try_recv() {
            let refresh_music = matches!(
                &event,
                media::Update::Playback(_) | media::Update::Artwork(_)
            );
            match event {
                media::Update::Playback(tracks) => {
                    let mut extras = self.extras.borrow_mut();
                    extras.track = tracks.first().cloned();
                    extras.tracks = tracks;
                    let urls: Vec<_> = extras.tracks.iter().map(|t| t.art_url.clone()).collect();
                    extras.artworks.retain(|url, _| urls.contains(url));
                }
                media::Update::Artwork(art) => {
                    let mut extras = self.extras.borrow_mut();
                    extras.artworks.insert(art.url.clone(), art.clone());
                    extras.artwork = Some(art);
                }
                media::Update::Audio(sources) => self.extras.borrow_mut().audio_sources = sources,
                media::Update::Wifi(result) => {
                    if let Ok(snapshot) = result {
                        let active = snapshot.networks.iter().find(|n| n.active);
                        self.wifi.set_tooltip_text(Some(
                            active.map(|n| n.ssid.as_str()).unwrap_or("Wi-Fi"),
                        ));
                        self.wifi.set_has_tooltip(false);
                        let icon = if !snapshot.enabled {
                            "network-wireless-disabled-symbolic"
                        } else if active.is_some() {
                            "network-wireless-signal-excellent-symbolic"
                        } else {
                            "network-wireless-offline-symbolic"
                        };
                        status_button(
                            &self.wifi,
                            icon,
                            active.map(|n| n.ssid.as_str()).unwrap_or(""),
                        );
                    }
                }
                media::Update::ControlFinished(id, result) => {
                    if let Err(e) = result
                        && let Some(m) = self.menu.borrow().as_ref().filter(|m| m.id == id)
                    {
                        let error = error_label();
                        set_error(&error, &e);
                        m.body.append(&error);
                    }
                }
            }
            if refresh_music {
                self.update_audio_label();
                if let (Some(view), Some(state)) = (
                    self.sound.borrow().as_ref(),
                    self.audio_state.borrow().as_ref(),
                ) {
                    view.update(state, &self.extras.borrow());
                }
            }
        }
        while let Ok(event) = self.services.events.try_recv() {
            match event {
                Event::Network(id, result) => {
                    let body = self
                        .menu
                        .borrow()
                        .as_ref()
                        .filter(|m| m.id == id)
                        .map(|m| m.body.clone());
                    if let Some(body) = body {
                        match result {
                            Ok(snapshot) => self.network_menu(&body, id, snapshot),
                            Err(e) => {
                                clear(&body);
                                body.append(&heading("Wi-Fi"));
                                let error = error_label();
                                set_error(&error, &e);
                                body.append(&error);
                                let retry = text_button("Back to networks");
                                let weak = Rc::downgrade(self);
                                retry.connect_clicked(move |_| {
                                    if let Some(s) = weak.upgrade() {
                                        s.services.network(id, services::Job::Scan(false));
                                    }
                                });
                                body.append(&retry);
                            }
                        }
                    }
                }
                Event::Apps(id, apps) => {
                    let body = self
                        .menu
                        .borrow()
                        .as_ref()
                        .filter(|m| m.id == id)
                        .map(|m| m.body.clone());
                    if let Some(body) = body {
                        self.apps_menu(&body, id, apps);
                    }
                }
                Event::Launched(id, result) => {
                    if self.menu.borrow().as_ref().is_some_and(|m| m.id == id) {
                        match result {
                            Ok(()) => self.close(),
                            Err(e) => {
                                if let Some(m) = self.menu.borrow().as_ref() {
                                    m.body.append(&label(&e));
                                }
                            }
                        }
                    }
                }
            }
        }
        while let Ok(snapshot) = self.services.preview_events.try_recv() {
            if let Some(m) = self.menu.borrow().as_ref()
                && m.kind == snapshot.name
            {
                self.preview_menu(&m.body, snapshot);
            }
        }
        self.update_hover();
        let mut close = false;
        let mut release_grab = None;
        if let Some(m) = self.menu.borrow_mut().as_mut() {
            let now = Instant::now();
            // A modal popover's grab suppresses widget motion on its parent.
            // Query GDK's tracked surface so hovering anywhere on the bar still
            // keeps the popup alive, without querying compositor-global input.
            let (surface, x, y) = gdk::Display::default()
                .and_then(|d| d.default_seat())
                .and_then(|s| s.pointer())
                .map(|p| p.surface_at_position())
                .unwrap_or((None, 0.0, 0.0));
            let over_bar = surface.is_some() && surface == self.window.surface();
            let over_popup = surface.is_some() && surface == m.pop.surface();
            let another_button = over_bar
                && self.hover_targets.borrow().iter().any(|(weak, kind)| {
                    kind != &m.kind
                        && weak.upgrade().is_some_and(|button| {
                            button.is_visible()
                                && button.compute_bounds(&self.window).is_some_and(|r| {
                                    r.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                                })
                        })
                });
            // Release modal pointer grabs when switching bar controls, or when
            // a pinned editor is left behind on another output. Keyboard focus
            // stays with the editor until the next menu takes over.
            if m.pop.is_autohide()
                && (another_button || (m.pinned.get() && !over_bar && !over_popup))
            {
                release_grab = Some(m.pop.clone());
                m.hover_opened.set(true);
            }
            m.dismissal.pointer_inside(over_bar || over_popup, now);
            m.dismissal.set_pinned(m.pinned.get(), now);
            let fade = m
                .closing
                .get()
                .map(|at| (now.duration_since(at).as_secs_f64() / 0.14).clamp(0.0, 1.0))
                .unwrap_or(m.dismissal.progress(now) as f64);
            let appear = (now.duration_since(m.opened).as_secs_f64() / 0.12).clamp(0.0, 1.0);
            m.pop.set_opacity(appear.min(1.0 - fade));
            if appear < 1.0 || fade > 0.0 {
                m.pop.queue_draw();
            }
            close = fade >= 1.0;
        }
        if let Some(pop) = release_grab {
            self.remapping.set(true);
            pop.popdown();
            pop.set_autohide(false);
            pop.popup();
            self.remapping.set(false);
        }
        if close {
            self.finish_close();
        }
    }
    fn update_status(self: &Rc<Self>, state: &status::Status) {
        self.update_audio_label();
        self.clock.set_text(
            &chrono::Local::now()
                .format("%a %b %-d  %-I:%M %p")
                .to_string(),
        );
        self.app.set_text(&state.app);
        let names: Vec<_> = state.workspaces.iter().map(|w| w.name.clone()).collect();
        if *self.names.borrow() != names {
            clear(&self.workspaces);
            for w in &state.workspaces {
                let b = gtk::Button::new();
                let contents = row(5);
                let name = gtk::Label::new(Some(&w.name));
                contents.append(&name);
                let sound = gtk::Image::from_icon_name("audio-volume-high-symbolic");
                sound.add_css_class("workspace-sound");
                sound.set_size_request(13, 13);
                sound.set_pixel_size(13);
                // Keep the indicator's slot in the workspace button even
                // when silent, so becoming audible cannot resize the number.
                sound.set_opacity(if w.audible { 1.0 } else { 0.0 });
                contents.append(&sound);
                b.set_child(Some(&contents));
                b.add_css_class("workspace");
                let weak_button = b.downgrade();
                let animation = Cell::new(if w.audible { 1.0_f64 } else { 0.0 });
                let previous_frame = Cell::new(None);
                sound.add_tick_callback(move |sound, clock| {
                    let Some(button) = weak_button.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let now = clock.frame_time();
                    let elapsed = previous_frame
                        .replace(Some(now))
                        .map(|last| (now - last) as f64 / 1_000_000.0)
                        .unwrap_or(0.0);
                    let target = if button.has_css_class("audible") {
                        1.0
                    } else {
                        0.0
                    };
                    let progress = animation.get();
                    if progress != target {
                        let step = elapsed.min(0.05) / 0.18;
                        let progress = if target > progress {
                            (progress + step).min(target)
                        } else {
                            (progress - step).max(target)
                        };
                        animation.set(progress);
                        let eased = progress * progress * (3.0 - 2.0 * progress);
                        sound.set_opacity(eased);
                        sound.set_pixel_size((13.0 * (0.92 + 0.08 * eased)).round() as i32);
                    }
                    glib::ControlFlow::Continue
                });
                let s = Rc::downgrade(self);
                let name = w.name.clone();
                b.connect_clicked(move |_| {
                    if let Some(s) = s.upgrade() {
                        s.close();
                        let _ = s.services.status.send(status::Update::Focus(name.clone()));
                    }
                });
                self.add_hover_menu(&b, &w.name);
                self.workspaces.append(&b);
            }
            self.names.replace(names);
        }
        let mut child = self.workspaces.first_child();
        for w in &state.workspaces {
            if let Some(c) = child {
                for (class, on) in [
                    ("active", w.focused),
                    ("urgent", w.urgent),
                    ("audible", w.audible),
                ] {
                    if on {
                        c.add_css_class(class)
                    } else {
                        c.remove_css_class(class)
                    }
                }
                child = c.next_sibling();
            }
        }
        self.battery.set_text(
            &state
                .battery
                .map(|(p, _)| format!("{p}%"))
                .unwrap_or_default(),
        );
        self.battery_icon.set_visible(state.battery.is_some());
        if let Some((p, charging)) = state.battery {
            draw_battery(&self.battery_icon, p, charging);
            if charging {
                self.battery_icon.add_css_class("charging");
            } else {
                self.battery_icon.remove_css_class("charging");
            }
        }
        for (class, on) in [
            ("charging", state.battery.is_some_and(|(_, c)| c)),
            ("low", state.battery.is_some_and(|(p, c)| p < 15 && !c)),
        ] {
            if on {
                self.battery.add_css_class(class);
            } else {
                self.battery.remove_css_class(class);
            }
        }
    }
    fn update_audio_label(&self) {
        let state = self.audio_state.borrow();
        let extras = self.extras.borrow();
        let streams = state
            .as_ref()
            .map(|state| state.streams.as_slice())
            .unwrap_or_default();
        let track = extras.track.as_ref();
        self.music
            .set_visible(!streams.is_empty() || extras.track.is_some());
        self.track_label
            .set_visible(!streams.is_empty() || track.is_some());
        let label = audio_source_label(streams, track);
        self.track_label.set_label(&label);
        if let Some(label) = self.track_label.child().and_downcast::<gtk::Label>() {
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(34);
        }
    }
}
#[cfg(test)]
mod audio_label_tests {
    use super::*;

    #[test]
    fn one_audio_source_keeps_its_name_and_multiple_use_a_count() {
        let firefox = volume::Stream {
            icon: "firefox".into(),
            index: 1,
            application: "Firefox".into(),
            name: "Firefox · YouTube".into(),
            percent: 50,
            muted: false,
            corked: false,
        };
        assert_eq!(
            audio_source_label(std::slice::from_ref(&firefox), None),
            "Firefox · YouTube"
        );
        assert_eq!(
            audio_source_label(&[firefox.clone(), firefox.clone()], None),
            "2 sources"
        );
        let track = media::Track {
            art_url: String::new(),
            player: "org.mpris.MediaPlayer2.spotify".into(),
            title: "Track".into(),
            artist: "Artist".into(),
            playing: true,
            can_previous: true,
            can_next: true,
            can_toggle: true,
        };
        assert_eq!(
            audio_source_label(std::slice::from_ref(&firefox), Some(&track)),
            "Track · Artist"
        );
    }
}
