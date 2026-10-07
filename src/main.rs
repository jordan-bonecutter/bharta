mod artwork;
mod capture;
mod icons;
mod iwd;
mod launcher;
mod media;
mod network;
mod panel;
mod popup_motion;
mod popup_ui;
mod render;
mod status;
mod supervisor;
mod workspace_preview;

use anyhow::{Context, Result, bail};
use render::{Action, Hit, Renderer};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_popup, delegate_xdg_shell,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, LoopHandle, channel},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            XdgPositioner, XdgShell,
            popup::{Popup, PopupConfigure, PopupHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::time::Duration;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};

#[derive(Default)]
struct Options {
    all_outputs: bool,
    dark: bool,
    output: Option<String>,
    font: Option<String>,
    preview: Option<String>,
    smoke: bool,
    check_network: bool,
    check_media: bool,
}
fn options() -> Result<Options> {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all-outputs" => options.all_outputs = true,
            "--dark" => options.dark = true,
            "--output" => {
                options.output = Some(args.next().context("--output needs an output name")?)
            }
            "--font" => options.font = Some(args.next().context("--font needs a font path")?),
            "--preview" => {
                options.preview = Some(args.next().context("--preview needs a PNG path")?)
            }
            "--smoke-test" => options.smoke = true,
            "--check-network" => options.check_network = true,
            "--check-media" => options.check_media = true,
            "--help" | "-h" => {
                println!(
                    "bharta — native Sway menu bar\n\nUsage: bharta [--dark] [--all-outputs | --output NAME] [--font PATH]\n       bharta [--dark] --preview FILE.png\n       bharta --smoke-test\n\nClick workspaces to switch. Default: all active outputs. Default theme: light. Height: 28 logical pixels.\n--preview renders sample data without connecting to Wayland.\n--smoke-test connects, renders a frame, then exits."
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
    let options = options()?;
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
        state.extras.audio_pids = media::audio_pids();
        state.update_audio();
        println!(
            "Track available: {}; active audio processes: {}; sounding workspaces: {:?}",
            state.extras.track.is_some(),
            state.extras.audio_pids.len(),
            state
                .workspaces
                .iter()
                .filter(|w| w.audible)
                .map(|w| &w.name)
                .collect::<Vec<_>>()
        );
        return Ok(());
    }
    let renderer = Renderer::new(options.font.as_deref(), options.dark)?;
    if let Some(path) = options.preview {
        renderer
            .draw(
                1440,
                2,
                &status::Status::demo(),
                "Tue Oct 6   9:41 AM",
                None,
            )?
            .0
            .save_png(&path)?;
        println!("Preview saved to {path}");
        return Ok(());
    }
    if !options.smoke && (options.all_outputs || options.output.is_none()) {
        return supervisor::run(&options);
    }
    let conn =
        Connection::connect_to_env().context("Connect to Wayland; run bharta inside Sway")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell =
        LayerShell::bind(&globals, &qh).context("Compositor must support wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(1920 * 28 * 4, &shm)?;
    let (updates, requests) = std::sync::mpsc::channel();
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let (ui_sender, ui_receiver) = channel::channel();
    let (preview_sender, preview_receiver) = channel::channel();
    let mut app = App {
        preview_requests: workspace_preview::watch(preview_sender),
        preview_target: None,
        workspace_preview: None,
        playback_requests: None,
        loop_handle: event_loop.handle(),
        compositor,
        xdg: XdgShell::bind(&globals, &qh)?,
        panel: None,
        panel_id: 0,
        ui_sender,
        keyboards: Vec::new(),
        keyboard_focus: false,
        updates: updates.clone(),
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer: None,
        renderer,
        width: 0,
        scale: 1,
        configured: false,
        exit: false,
        pointers: Vec::new(),
        hits: Vec::new(),
        hover: None,
        status: status::Status::default(),
        smoke: options.smoke,
        error: None,
    };
    queue.roundtrip(&mut app)?;
    let output = if let Some(name) = &options.output {
        Some(
            app.outputs
                .outputs()
                .find(|o| {
                    app.outputs
                        .info(o)
                        .is_some_and(|i| i.name.as_ref() == Some(name))
                })
                .with_context(|| format!("Output {name:?} not found"))?,
        )
    } else {
        None
    };
    let layer = shell.create_layer_surface(
        &qh,
        app.compositor.create_surface(&qh),
        Layer::Top,
        Some("bharta"),
        output.as_ref(),
    );
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 28);
    layer.set_exclusive_zone(28);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();
    app.layer = Some(layer);
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Wayland event source: {e}"))?;
    event_loop
        .handle()
        .insert_source(ui_receiver, |event, _, app| {
            if let channel::Event::Msg(result) = event {
                app.ui_result(result);
            }
        })
        .map_err(|e| anyhow::anyhow!("UI result source: {e}"))?;
    event_loop
        .handle()
        .insert_source(preview_receiver, |event, _, app| {
            if let channel::Event::Msg(snapshot) = event {
                app.preview_result(snapshot);
            }
        })
        .map_err(|e| anyhow::anyhow!("Preview event source: {e}"))?;
    let (sender, receiver) = channel::channel();
    event_loop
        .handle()
        .insert_source(receiver, |event, _, app| {
            if let channel::Event::Msg(status) = event {
                let extras = std::mem::take(&mut app.status.extras);
                app.status = status;
                app.status.extras = extras;
                app.status.update_audio();
                app.draw();
            }
        })
        .map_err(|e| anyhow::anyhow!("Status event source: {e}"))?;
    let (extra_sender, extra_receiver) = channel::channel();
    event_loop
        .handle()
        .insert_source(extra_receiver, |event, _, app| {
            if let channel::Event::Msg(update) = event {
                match update {
                    media::Update::Artwork(art) => {
                        if app
                            .status
                            .extras
                            .track
                            .as_ref()
                            .is_some_and(|t| t.art_url == art.url)
                        {
                            app.status.extras.artwork = Some(art.clone());
                            if let Some(p) = app.panel.as_mut()
                                && p.kind == panel::Kind::Music
                            {
                                p.artwork = Some(art);
                                app.draw_panel();
                            }
                        }
                    }
                    media::Update::Playback(track) => {
                        app.status.extras.track = track.clone();
                        if let Some(p) = app.panel.as_mut()
                            && p.kind == panel::Kind::Music
                        {
                            p.track = track;
                            app.draw_panel();
                        }
                    }
                    media::Update::ControlFinished(id, result) => {
                        if let Some(p) = app.panel.as_mut()
                            && p.id == id
                        {
                            p.busy = false;
                            if let Err(error) = result {
                                p.message = error;
                            }
                            app.draw_panel();
                        }
                    }
                    media::Update::Audio(pids) => {
                        app.status.extras.audio_pids = pids;
                        app.status.update_audio();
                    }
                    media::Update::Wifi(Ok(snapshot)) => {
                        app.status.extras.wifi_signal = snapshot
                            .networks
                            .iter()
                            .find(|n| n.active)
                            .map(|n| n.signal);
                        app.status.extras.wifi_name = snapshot
                            .networks
                            .iter()
                            .find(|n| n.active)
                            .map(|n| n.ssid.clone());
                        app.status.extras.wifi_enabled = Some(snapshot.enabled);
                        if let Some(p) = app.panel.as_mut()
                            && p.kind == panel::Kind::Network
                            && !p.busy
                        {
                            p.snapshot = snapshot;
                            p.network_ready = true;
                            app.draw_panel();
                        }
                    }
                    media::Update::Wifi(Err(_)) => {}
                }
                app.draw();
            }
        })
        .map_err(|e| anyhow::anyhow!("Media event source: {e}"))?;
    app.playback_requests = Some(media::watch(extra_sender));
    status::watch(updates);
    std::thread::spawn(move || {
        loop {
            if sender
                .send(status::Status::read(options.output.as_deref()))
                .is_err()
            {
                break;
            }
            match requests.recv_timeout(Duration::from_secs(1)) {
                Ok(status::Update::Focus(name)) => {
                    if let Err(e) = status::focus(&name) {
                        eprintln!("{e}");
                    }
                    // The next iteration publishes authoritative state immediately.
                }
                Ok(status::Update::Refresh) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !app.exit {
        let wait = app
            .panel
            .as_ref()
            .map(|p| p.dismissal.next_frame(std::time::Instant::now()))
            .unwrap_or(Duration::from_millis(250));
        let wait = if app.preview_target.is_some() {
            wait.min(Duration::from_millis(40))
        } else {
            wait
        };
        event_loop.dispatch(wait, &mut app)?;
        app.tick_preview(&qh);
        app.animate_popup();
        conn.flush()?;
        if app.smoke && std::time::Instant::now() > deadline {
            bail!("No rendered frame with live Sway status within five seconds");
        }
    }
    if let Some(error) = app.error {
        bail!("{error}");
    }
    Ok(())
}
struct App {
    preview_requests: std::sync::mpsc::Sender<Option<String>>,
    preview_target: Option<(String, i32, std::time::Instant)>,
    workspace_preview: Option<workspace_preview::Preview>,
    playback_requests: Option<std::sync::mpsc::Sender<media::Request>>,
    loop_handle: LoopHandle<'static, App>,
    compositor: CompositorState,
    xdg: XdgShell,
    panel: Option<panel::Panel>,
    panel_id: u64,
    ui_sender: channel::Sender<popup_ui::ResultEvent>,
    keyboards: Vec<(wl_seat::WlSeat, wl_keyboard::WlKeyboard)>,
    keyboard_focus: bool,
    updates: std::sync::mpsc::Sender<status::Update>,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    renderer: Renderer,
    width: u32,
    scale: u32,
    configured: bool,
    exit: bool,
    smoke: bool,
    error: Option<String>,
    pointers: Vec<(wl_seat::WlSeat, wl_pointer::WlPointer)>,
    status: status::Status,
    hits: Vec<Hit>,
    hover: Option<f32>,
}
impl App {
    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        if let Err(e) = self.present() {
            self.error = Some(e.to_string());
            self.exit = true;
        }
    }
    fn present(&mut self) -> Result<()> {
        let clock = chrono::Local::now()
            .format("%a %b %-d   %-I:%M %p")
            .to_string();
        let (pix, hits) =
            self.renderer
                .draw(self.width, self.scale, &self.status, &clock, self.hover)?;
        self.hits = hits;
        let (w, h) = (pix.width() as i32, pix.height() as i32);
        let (buffer, canvas) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
        for (src, dest) in pix
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(canvas.as_chunks_mut::<4>().0.iter_mut())
        {
            dest.copy_from_slice(
                &u32::from_be_bytes([src[3], src[0], src[1], src[2]]).to_ne_bytes(),
            );
        }
        let layer = self.layer.as_ref().context("Missing layer surface")?;
        layer.wl_surface().set_buffer_scale(self.scale as i32);
        layer.wl_surface().damage_buffer(0, 0, w, h);
        buffer.attach_to(layer.wl_surface())?;
        layer.commit();
        if self.smoke && self.status.connected {
            println!("Rendered {w}×{h} Wayland buffer at {}× scale", self.scale);
            self.exit = true;
        }
        Ok(())
    }
}
impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(p) = self.workspace_preview.as_mut()
            && p.popup.wl_surface() == surface
        {
            p.scale = factor.max(1) as u32;
            self.draw_preview();
            return;
        }
        if let Some(p) = self.panel.as_mut()
            && p.popup.wl_surface() == surface
        {
            p.scale = factor.max(1) as u32;
            self.draw_panel();
        } else {
            self.scale = factor.max(1) as u32;
            self.draw();
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}
impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.width = c.new_size.0.max(1);
        self.configured = true;
        self.draw();
    }
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard && !self.keyboards.iter().any(|(s, _)| *s == seat) {
            match self.seats.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app, _keyboard, event| app.panel_key(event)),
            ) {
                Ok(k) => self.keyboards.push((seat.clone(), k)),
                Err(e) => eprintln!("Keyboard unavailable: {e}"),
            }
        }
        if cap == Capability::Pointer && !self.pointers.iter().any(|(s, _)| *s == seat) {
            match self.seats.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointers.push((seat, pointer)),
                Err(e) => eprintln!("Pointer unavailable: {e}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard {
            self.keyboards.retain(|(s, k)| {
                if *s == seat {
                    k.release();
                    false
                } else {
                    true
                }
            });
        }
        if cap == Capability::Pointer {
            self.remove_pointer(&seat);
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_pointer(&seat);
        self.keyboards.retain(|(s, k)| {
            if *s == seat {
                k.release();
                false
            } else {
                true
            }
        });
    }
}
impl App {
    fn remove_pointer(&mut self, seat: &wl_seat::WlSeat) {
        self.pointers.retain(|(s, p)| {
            if s == seat {
                p.release();
                false
            } else {
                true
            }
        });
    }
}
impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if self
                .panel
                .as_ref()
                .is_some_and(|p| p.popup.wl_surface() == &event.surface)
            {
                self.panel_pointer(event);
                continue;
            }
            if !self
                .layer
                .as_ref()
                .is_some_and(|l| l.wl_surface() == &event.surface)
            {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.hover = Some(event.position.0 as f32);
                    self.hover_workspace(event.position.0 as f32);
                    self.draw();
                }
                PointerEventKind::Leave { .. } => {
                    self.hover = None;
                    self.close_preview();
                    self.draw();
                }
                PointerEventKind::Press {
                    button: 0x110,
                    serial,
                    ..
                } => {
                    if let Some(hit) = self.hits.iter().find(|h| {
                        event.position.0 as f32 >= h.start && (event.position.0 as f32) < h.end
                    }) {
                        let anchor = if matches!(hit.action, Action::Music) {
                            (hit.end - 12.0) as i32
                        } else {
                            event.position.0 as i32
                        };
                        let action = hit.action.clone();
                        self.close_preview();
                        match action {
                            Action::Workspace(name) => {
                                let _ = self.updates.send(status::Update::Focus(name));
                            }
                            Action::Network
                            | Action::Session
                            | Action::Launcher
                            | Action::Music => {
                                let kind = match action {
                                    Action::Network => panel::Kind::Network,
                                    Action::Session => panel::Kind::Session,
                                    Action::Music => panel::Kind::Music,
                                    _ => panel::Kind::Launcher,
                                };
                                if let Some(seat) = self
                                    .pointers
                                    .iter()
                                    .find(|(_, p)| p == pointer)
                                    .map(|(s, _)| s.clone())
                                    && let Err(e) = self.open_panel(kind, anchor, &seat, serial, qh)
                                {
                                    eprintln!("Popup: {e}");
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);

delegate_keyboard!(App);
delegate_xdg_shell!(App);
delegate_xdg_popup!(App);
