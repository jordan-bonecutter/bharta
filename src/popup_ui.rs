use super::*;
use panel::{Action as PanelAction, Kind, Panel};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor as PopupAnchor, ConstraintAdjustment, Gravity,
};

pub enum ResultEvent {
    Network(u64, std::result::Result<network::Snapshot, String>, bool),
    Apps(u64, Vec<launcher::Entry>),
    Launched(u64, std::result::Result<(), String>),
}
enum Job {
    Scan(bool),
    Radio(bool),
    Connect(network::Network, String),
    Disconnect(String),
}
impl App {
    pub fn open_panel(
        &mut self,
        kind: Kind,
        x: i32,
        seat: &wl_seat::WlSeat,
        serial: u32,
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        if self.panel.as_ref().is_some_and(|p| p.kind == kind) {
            self.close_panel();
            return Ok(());
        }
        self.close_panel();
        self.close_preview();
        // Sway routes popup keyboard events through the layer's focus policy.
        // Acquire focus only while a popup is open, restoring normal focus on close.
        if let Some(layer) = &self.layer {
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
        }
        let position = XdgPositioner::new(&self.xdg)?;
        position.set_size(panel::WIDTH as i32, panel::height(kind) as i32);
        position.set_anchor_rect(x.clamp(0, self.width.saturating_sub(1) as i32), 28, 1, 1);
        position.set_anchor(PopupAnchor::BottomRight);
        position.set_gravity(Gravity::BottomLeft);
        position
            .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY);
        let popup = Popup::from_surface(
            None,
            &position,
            qh,
            self.compositor.create_surface(qh),
            &self.xdg,
        )?;
        self.layer
            .as_ref()
            .context("Missing bar")?
            .get_popup(popup.xdg_popup());
        popup.xdg_popup().grab(seat, serial);
        popup.wl_surface().commit();
        self.panel_id += 1;
        let mut panel = Panel::new(popup, kind, self.panel_id, self.scale);
        panel.track = self.status.extras.track.clone();
        panel.artwork = self.status.extras.artwork.clone();
        self.panel = Some(panel);
        match kind {
            Kind::Network => self.network_job(Job::Scan(false)),
            Kind::Launcher => self.load_apps(),
            _ => {}
        }
        Ok(())
    }
    fn close_panel(&mut self) {
        self.panel = None;
        self.keyboard_focus = false;
        if let Some(layer) = &self.layer {
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
        }
    }
    fn load_apps(&mut self) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        p.kind = Kind::Launcher;
        p.busy = true;
        p.message.clear();
        let (id, tx) = (p.id, self.ui_sender.clone());
        std::thread::spawn(move || {
            let _ = tx.send(ResultEvent::Apps(id, launcher::entries()));
        });
    }
    fn network_job(&mut self, job: Job) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        if p.busy {
            return;
        }
        p.busy = true;
        p.message.clear();
        let (id, tx) = (p.id, self.ui_sender.clone());
        std::thread::spawn(move || {
            let connected = matches!(&job, Job::Connect(..));
            let result = match job {
                Job::Scan(rescan) => network::scan(rescan),
                Job::Radio(on) => network::radio(on),
                Job::Connect(n, password) => network::connect(&n, password),
                Job::Disconnect(device) => network::disconnect(&device),
            }
            .map_err(|e| e.to_string());
            let _ = tx.send(ResultEvent::Network(id, result, connected));
        });
    }
    pub fn ui_result(&mut self, result: ResultEvent) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        match result {
            ResultEvent::Network(id, result, connected) if id == p.id => {
                p.busy = false;
                match result {
                    Ok(snapshot) => {
                        p.snapshot = snapshot;
                        p.network_ready = true;
                        p.page = 0;
                        if connected {
                            p.selected = None;
                            p.password.clear();
                        }
                    }
                    Err(e) => p.message = e,
                }
            }
            ResultEvent::Apps(id, apps) if id == p.id => {
                p.apps = apps;
                p.busy = false;
            }
            ResultEvent::Launched(id, result) if id == p.id => {
                p.busy = false;
                match result {
                    Ok(()) => {
                        self.close_panel();
                        return;
                    }
                    Err(e) => p.message = e,
                }
            }
            _ => return,
        }
        self.draw_panel();
    }
    pub fn animate_popup(&mut self) {
        let Some(panel) = &mut self.panel else {
            return;
        };
        panel.update_dismissal();
        let progress = panel.dismissal.progress(std::time::Instant::now());
        if progress >= 1.0 {
            self.close_panel();
        } else if progress > 0.0 {
            self.draw_panel();
        }
    }
    pub fn draw_panel(&mut self) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        if !p.ready {
            return;
        }
        p.update_dismissal();
        let mut pix = p.render(&self.renderer);
        let fade = p.dismissal.progress(std::time::Instant::now());
        if fade > 0.0 {
            let eased = fade * fade * (3.0 - 2.0 * fade);
            let shrink = 1.0 - 0.01 * eased;
            let mut frame = tiny_skia::Pixmap::new(pix.width(), pix.height()).unwrap();
            let transform = tiny_skia::Transform::from_scale(shrink, shrink).post_translate(
                pix.width() as f32 * (1.0 - shrink) / 2.0,
                pix.height() as f32 * (1.0 - shrink) * 0.15,
            );
            frame.draw_pixmap(
                0,
                0,
                pix.as_ref(),
                &tiny_skia::PixmapPaint {
                    opacity: 1.0 - eased,
                    quality: tiny_skia::FilterQuality::Bilinear,
                    ..Default::default()
                },
                transform,
                None,
            );
            pix = frame;
        }
        let (w, h) = (pix.width() as i32, pix.height() as i32);
        let result = (|| -> Result<()> {
            let (buffer, canvas) =
                self.pool
                    .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
            for (src, dst) in pix
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(canvas.as_chunks_mut::<4>().0.iter_mut())
            {
                dst.copy_from_slice(
                    &u32::from_be_bytes([src[3], src[0], src[1], src[2]]).to_ne_bytes(),
                );
            }
            p.popup.xdg_surface().set_window_geometry(
                0,
                0,
                panel::WIDTH as i32,
                panel::height(p.kind) as i32,
            );
            p.popup.wl_surface().set_buffer_scale(p.scale as i32);
            p.popup.wl_surface().damage_buffer(0, 0, w, h);
            buffer.attach_to(p.popup.wl_surface())?;
            p.popup.wl_surface().commit();
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("Popup drawing: {e}");
            self.close_panel();
        }
    }
    pub fn panel_pointer(&mut self, event: &PointerEvent) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        let mut action = None;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                p.dismissal.enter();
                p.hover = Some((event.position.0 as f32, event.position.1 as f32));
            }
            PointerEventKind::Leave { .. } => {
                p.hover = None;
                p.dismissal.leave(std::time::Instant::now());
            }
            PointerEventKind::Press { button: 0x110, .. } => {
                if (18.0..362.0).contains(&event.position.0) {
                    action = p
                        .rows
                        .iter()
                        .find(|r| r.contains(event.position.0 as f32, event.position.1 as f32))
                        .map(|r| r.action.clone());
                }
            }
            PointerEventKind::Axis { vertical, .. } => {
                if p.kind == Kind::Launcher {
                    if vertical.absolute > 0.0 {
                        p.selection += 1;
                    } else if vertical.absolute < 0.0 {
                        p.selection = p.selection.saturating_sub(1);
                    }
                } else if p.kind == Kind::Network && p.selected.is_none() && !p.busy {
                    if vertical.absolute > 0.0 && (p.page + 1) * 6 < p.snapshot.networks.len() {
                        p.page += 1;
                    } else if vertical.absolute < 0.0 {
                        p.page = p.page.saturating_sub(1);
                    }
                    p.message.clear();
                }
            }
            _ => {}
        }
        if let Some(action) = action {
            self.panel_action(action);
        }
        self.draw_panel();
    }
    fn panel_action(&mut self, action: PanelAction) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        match action {
            PanelAction::Media(control) => {
                if p.busy {
                    return;
                }
                if let Some(track) = &p.track
                    && let Some(requests) = &self.playback_requests
                {
                    p.busy = true;
                    p.message.clear();
                    if requests
                        .send(media::Request::Control {
                            panel_id: p.id,
                            player: track.player.clone(),
                            control,
                        })
                        .is_err()
                    {
                        p.busy = false;
                        p.message = "Playback service is unavailable.".into();
                    }
                }
            }
            PanelAction::Scan => self.network_job(Job::Scan(true)),
            PanelAction::Radio => {
                let on = !p.snapshot.enabled;
                self.network_job(Job::Radio(on));
            }
            PanelAction::Choose(n) => {
                p.message.clear();
                if n.active {
                    p.message = "You are connected to this network.".into();
                } else if n.security.contains("802.1X") {
                    p.message = "This enterprise network needs Advanced settings.".into();
                } else if n.security == "--" || n.security.is_empty() {
                    self.network_job(Job::Connect(n, String::new()));
                } else {
                    p.selected = Some(n);
                    p.password.clear();
                }
            }
            PanelAction::Connect => {
                if !p.busy
                    && let Some(n) = p.selected.clone()
                {
                    let secret = std::mem::take(&mut p.password);
                    self.network_job(Job::Connect(n, secret));
                }
            }
            PanelAction::Back => {
                p.selected = None;
                p.password.clear();
                p.confirm = false;
                p.message.clear();
            }
            PanelAction::Disconnect(device) => self.network_job(Job::Disconnect(device)),
            PanelAction::Previous => {
                p.page = p.page.saturating_sub(1);
                p.message.clear();
            }
            PanelAction::Next => {
                if (p.page + 1) * 6 < p.snapshot.networks.len() {
                    p.page += 1;
                }
                p.message.clear();
            }
            PanelAction::Advanced => {
                self.close_panel();
                std::thread::spawn(|| {
                    let _ = status::ipc(0, "exec nm-connection-editor");
                });
            }
            PanelAction::Launch => self.load_apps(),
            PanelAction::Lock => {
                self.close_panel();
                std::thread::spawn(|| {
                    // Match the user's lock shortcut; fall back on other installs.
                    use std::os::unix::fs::PermissionsExt;
                    let has_script = std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".local/bin/lock-session"))
                        .and_then(|path| std::fs::metadata(path).ok())
                        .is_some_and(|meta| {
                            meta.is_file() && meta.permissions().mode() & 0o111 != 0
                        });
                    let command = if has_script {
                        r#"exec "$HOME/.local/bin/lock-session""#
                    } else {
                        "exec swaylock"
                    };
                    if let Err(error) = status::ipc(0, command) {
                        eprintln!("Screen lock: {error}");
                    }
                });
            }
            PanelAction::Logout => p.confirm = true,
            PanelAction::ConfirmLogout => {
                self.close_panel();
                std::thread::spawn(|| {
                    let _ = status::ipc(0, "exit");
                });
            }
            PanelAction::App(path) => {
                if p.busy {
                    return;
                }
                p.busy = true;
                let (id, tx) = (p.id, self.ui_sender.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(ResultEvent::Launched(
                        id,
                        launcher::launch(&path).map_err(|e| e.to_string()),
                    ));
                });
            }
        }
    }
    pub(crate) fn panel_key(&mut self, event: KeyEvent) {
        if !self.keyboard_focus {
            return;
        }
        if event.keysym == Keysym::Escape {
            self.close_panel();
            return;
        }
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        p.dismissal.activity(std::time::Instant::now());
        if p.busy {
            return;
        }
        let mut action = None;
        if p.kind == Kind::Launcher {
            match event.keysym {
                Keysym::Up => p.selection = p.selection.saturating_sub(1),
                Keysym::Down => p.selection += 1,
                Keysym::Return | Keysym::KP_Enter => {
                    action = p
                        .apps
                        .iter()
                        .filter(|a| {
                            p.query
                                .split_whitespace()
                                .all(|w| a.search.contains(&w.to_lowercase()))
                        })
                        .nth(p.selection)
                        .map(|a| PanelAction::App(a.path.clone()));
                }
                Keysym::BackSpace => {
                    p.query.pop();
                    p.selection = 0;
                }
                _ => {
                    if let Some(text) = event.utf8 {
                        p.query.extend(
                            text.chars()
                                .filter(|c| !c.is_control())
                                .take(128usize.saturating_sub(p.query.chars().count())),
                        );
                        p.selection = 0;
                    }
                }
            }
        } else if p.selected.is_some() {
            match event.keysym {
                Keysym::Return | Keysym::KP_Enter => action = Some(PanelAction::Connect),
                Keysym::BackSpace => {
                    p.password.pop();
                }
                _ => {
                    if let Some(text) = event.utf8 {
                        p.password.extend(
                            text.chars()
                                .filter(|c| !c.is_control())
                                .take(128usize.saturating_sub(p.password.chars().count())),
                        );
                    }
                }
            }
        }
        if let Some(action) = action {
            self.panel_action(action);
        }
        self.draw_panel();
    }
}
impl PopupHandler for App {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        popup: &Popup,
        _: PopupConfigure,
    ) {
        if let Some(p) = self.workspace_preview.as_mut()
            && p.popup == *popup
        {
            p.ready = true;
            self.draw_preview();
            return;
        }
        if let Some(p) = self.panel.as_mut()
            && p.popup == *popup
        {
            p.ready = true;
            self.draw_panel();
        }
    }
    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        if self
            .workspace_preview
            .as_ref()
            .is_some_and(|p| p.popup == *popup)
        {
            self.close_preview();
        }
        if self.panel.as_ref().is_some_and(|p| p.popup == *popup) {
            self.close_panel();
        }
    }
}
impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.keyboard_focus = self.panel.is_some()
            && (self
                .panel
                .as_ref()
                .is_some_and(|p| p.popup.wl_surface() == surface)
                || self
                    .layer
                    .as_ref()
                    .is_some_and(|l| l.wl_surface() == surface));
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.keyboard_focus = false;
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.panel_key(event);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.panel_key(event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

// SCTK's shell delegate includes decoration dispatch, even for popup-only clients.
impl smithay_client_toolkit::shell::xdg::window::WindowHandler for App {
    fn request_close(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::shell::xdg::window::Window,
    ) {
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::shell::xdg::window::Window,
        _: smithay_client_toolkit::shell::xdg::window::WindowConfigure,
        _: u32,
    ) {
    }
}
