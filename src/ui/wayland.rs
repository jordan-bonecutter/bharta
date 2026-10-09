use super::{
    Ui,
    keyboard::{KeyEvent, Keyboard},
};
use anyhow::{Context, Result};
use egui::{Event, Key, Modifiers as EModifiers, PointerButton, pos2, vec2};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
    },
    delegate_compositor, delegate_data_device, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{calloop::EventLoop, calloop_wayland_source::WaylandSource},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{
    io::{Read, Write},
    os::{fd::OwnedFd, unix::fs::FileExt},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{
        wl_data_device, wl_data_device_manager::DndAction, wl_data_source, wl_keyboard, wl_output,
        wl_pointer, wl_seat, wl_shm, wl_surface,
    },
};
use xkeysym::Keysym;

pub fn run(options: crate::Options) -> Result<()> {
    let conn =
        Connection::connect_to_env().context("Connect to Wayland; run bharta inside Sway")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(4096, &shm)?;
    let (paste_tx, paste_rx) = mpsc::channel();
    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        buffers: vec![],
        frame_ready: true,
        ui: Ui::new(&options)?,
        layer: None,
        keyboard: None,
        keymap: Keyboard::default(),
        pointer: None,
        data_manager: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        copy_source: None,
        copied: String::new(),
        paste_tx,
        paste_rx,
        input: egui::RawInput::default(),
        renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra),
        width: 1,
        height: 28,
        output_height: 900,
        output: None,
        scale: 1,
        requested_height: 28,
        keyboard_interactive: false,
        compositor,
        repaint_at: Arc::new(Mutex::new(Some(Instant::now()))),
        focus: false,
        serial: 0,
        exit: false,
        configured: false,
        dirty: true,
        start: Instant::now(),
        repeated: None,
        repeat_delay: 400,
        repeat_rate: 25,
    };
    queue.roundtrip(&mut app)?;
    let output = app
        .outputs
        .outputs()
        .find(|o| {
            app.outputs.info(o).is_some_and(|info| {
                options
                    .output
                    .as_ref()
                    .is_none_or(|name| info.name.as_ref() == Some(name))
            })
        })
        .context("Requested Wayland output is unavailable")?;
    if let Some(info) = app.outputs.info(&output) {
        app.output_height = info.logical_size.map(|(_, h)| h as u32).unwrap_or_else(|| {
            info.modes
                .iter()
                .find(|m| m.current)
                .map(|m| m.dimensions.1 as u32 / info.scale_factor.max(1) as u32)
                .unwrap_or(900)
        });
        app.scale = info.scale_factor.max(1);
    }
    app.output = Some(output.clone());
    let repaint_at = app.repaint_at.clone();
    app.ui.ctx.set_request_repaint_callback(move |info| {
        if let Some(at) = Instant::now().checked_add(info.delay) {
            let mut next = repaint_at.lock().unwrap();
            *next = Some(next.map_or(at, |old| old.min(at)));
        }
    });
    let surface = app.compositor.create_surface(&qh);
    let layer = shell.create_layer_surface(&qh, surface, Layer::Top, Some("bharta"), Some(&output));
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 28);
    layer.set_exclusive_zone(28);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();
    app.layer = Some(layer);
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Wayland event source: {e}"))?;
    let mut last = Instant::now();
    while !app.exit && (!options.smoke || app.start.elapsed() < Duration::from_secs(3)) {
        let now = Instant::now();
        let mut wait = Duration::from_millis(if app.ui.preview.is_some() { 4 } else { 50 });
        if app.frame_ready {
            let at = if app.dirty {
                Some(now)
            } else {
                *app.repaint_at.lock().unwrap()
            };
            if let Some(at) = at {
                wait = wait.min(
                    at.max(last + Duration::from_millis(16))
                        .saturating_duration_since(now),
                );
            }
        }
        if let Some((_, at)) = app.repeated {
            wait = wait.min(at.saturating_duration_since(now));
        }
        event_loop.dispatch(wait, &mut app)?;
        app.ui.poll();
        while let Ok(text) = app.paste_rx.try_recv() {
            app.input.events.push(Event::Paste(text));
            app.dirty = true;
        }
        if let Some((event, next)) = app.repeated
            && Instant::now() >= next
        {
            if let Some(key) = app.keymap.event(event, true) {
                app.key(key, true, true);
            }
            app.repeated = Some((
                event,
                Instant::now() + Duration::from_millis(1000 / app.repeat_rate.max(1) as u64),
            ));
        }
        if app.configured
            && app.frame_ready
            && last.elapsed() >= Duration::from_millis(16)
            && (app.dirty
                || app
                    .repaint_at
                    .lock()
                    .unwrap()
                    .is_some_and(|at| Instant::now() >= at)
                || last.elapsed() >= Duration::from_secs(1))
        {
            app.draw(&qh)?;
            last = Instant::now();
            app.dirty = false;
        }
        conn.flush()?;
    }
    Ok(())
}
struct App {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    buffers: Vec<Buffer>,
    frame_ready: bool,
    ui: Ui,
    layer: Option<LayerSurface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keymap: Keyboard,
    pointer: Option<wl_pointer::WlPointer>,
    data_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    copy_source: Option<CopyPasteSource>,
    copied: String,
    paste_tx: mpsc::Sender<String>,
    paste_rx: mpsc::Receiver<String>,
    input: egui::RawInput,
    renderer: EguiSoftwareRender,
    width: u32,
    height: u32,
    output_height: u32,
    output: Option<wl_output::WlOutput>,
    scale: i32,
    requested_height: u32,
    keyboard_interactive: bool,
    compositor: CompositorState,
    repaint_at: Arc<Mutex<Option<Instant>>>,
    focus: bool,
    serial: u32,
    exit: bool,
    configured: bool,
    dirty: bool,
    start: Instant,
    repeated: Option<(u32, Instant)>,
    repeat_delay: u32,
    repeat_rate: u32,
}
impl App {
    fn draw(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        *self.repaint_at.lock().unwrap() = None;
        let ctx = self.ui.ctx.clone();
        self.input.screen_rect = Some(egui::Rect::from_min_size(
            Default::default(),
            vec2(self.width as f32, self.output_height as f32),
        ));
        self.input.time = Some(self.start.elapsed().as_secs_f64());
        self.input.focused = self.focus;
        self.input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(self.scale as f32);
        let out = ctx.run_ui(self.input.take(), |root| {
            let ctx = root.ctx();
            self.ui
                .frame(ctx, self.width as f32, self.output_height as f32)
        });
        // Delayed egui requests are deadlines, not reasons to draw every frame.
        *self.repaint_at.lock().unwrap() = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .and_then(|v| Instant::now().checked_add(v.repaint_delay));
        for command in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command
                && let (Some(manager), Some(device)) = (&self.data_manager, &self.data_device)
            {
                self.copied = text.clone();
                let source = manager
                    .create_copy_paste_source(qh, ["text/plain;charset=utf-8", "text/plain"]);
                source.set_selection(device, self.serial);
                self.copy_source = Some(source);
            }
        }
        let layer = self.layer.as_ref().unwrap();
        let keyboard = self.ui.keyboard();
        if self.keyboard_interactive != keyboard {
            self.keyboard_interactive = keyboard;
            layer.set_keyboard_interactivity(if keyboard {
                KeyboardInteractivity::Exclusive
            } else {
                KeyboardInteractivity::None
            });
        }
        let height = if self.ui.panel.is_some() {
            self.ui.panel_rect.max.y.ceil().max(28.) as u32
        } else {
            28
        }
        .min(self.output_height);
        if self.requested_height != height {
            self.requested_height = height;
            layer.set_size(0, height);
            layer.commit();
        }
        // Transparent popup margins must pass pointer input to the desktop.
        let region = Region::new(&self.compositor)?;
        region.add(0, 0, self.width as i32, 28);
        if self.ui.panel.is_some() {
            let r = self.ui.panel_rect;
            region.add(
                r.min.x.floor() as i32,
                r.min.y.floor() as i32,
                r.width().ceil() as i32,
                r.height().ceil() as i32,
            );
        }
        layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
        let scale = self.scale as u32;
        let (w, h) = (self.width * scale, self.height * scale);
        self.buffers
            .retain(|buffer| buffer.height() == h as i32 && buffer.stride() == w as i32 * 4);
        let available = self
            .buffers
            .iter()
            .position(|buffer| self.pool.canvas(buffer).is_some());
        let index = if let Some(index) = available {
            index
        } else {
            let (buffer, _) = self.pool.create_buffer(
                w as i32,
                h as i32,
                w as i32 * 4,
                wl_shm::Format::Argb8888,
            )?;
            self.buffers.push(buffer);
            self.buffers.len() - 1
        };
        let buffer = &self.buffers[index];
        let canvas = self
            .pool
            .canvas(buffer)
            .context("Released presentation buffer")?;
        canvas.fill(0);
        let pixels = canvas.as_chunks_mut::<4>().0;
        let jobs = ctx.tessellate(out.shapes, out.pixels_per_point);
        self.renderer.render(
            &mut BufferMutRef::new(pixels, w as usize, h as usize),
            &jobs,
            &out.textures_delta,
            out.pixels_per_point,
        );
        layer.wl_surface().set_buffer_scale(self.scale);
        layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        layer.wl_surface().frame(qh, layer.wl_surface().clone());
        self.frame_ready = false;
        buffer.attach_to(layer.wl_surface())?;
        layer.commit();
        Ok(())
    }
    fn key(&mut self, event: KeyEvent, pressed: bool, repeat: bool) {
        self.dirty = true;
        if pressed && self.input.modifiers.ctrl {
            match event.keysym {
                Keysym::c | Keysym::C => {
                    self.input.events.push(Event::Copy);
                    return;
                }
                Keysym::x | Keysym::X => {
                    self.input.events.push(Event::Cut);
                    return;
                }
                Keysym::v | Keysym::V => {
                    if let Some(offer) = self
                        .data_device
                        .as_ref()
                        .and_then(|d| d.data().selection_offer())
                        && let Some(mime) = offer.with_mime_types(|types| {
                            types
                                .iter()
                                .find(|t| {
                                    t.as_str() == "text/plain;charset=utf-8"
                                        || t.as_str() == "text/plain"
                                })
                                .cloned()
                        })
                        && let Ok(pipe) = offer.receive(mime)
                    {
                        // Read on a worker; the compositor must dispatch the transfer first.
                        let tx = self.paste_tx.clone();
                        std::thread::spawn(move || {
                            let mut f = std::fs::File::from(OwnedFd::from(pipe));
                            let mut bytes = vec![];
                            // A stalled clipboard owner must not stall the UI or leak a reader forever.
                            let mut poll = libc::pollfd {
                                fd: std::os::fd::AsRawFd::as_raw_fd(&f),
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            let end = Instant::now() + Duration::from_secs(2);
                            while Instant::now() < end && bytes.len() < 1024 * 1024 {
                                if unsafe { libc::poll(&mut poll, 1, 100) } <= 0 {
                                    continue;
                                }
                                let mut chunk = [0; 4096];
                                match f.read(&mut chunk) {
                                    Ok(0) => break,
                                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                        continue;
                                    }
                                    Err(_) => break,
                                }
                            }
                            if let Ok(text) = String::from_utf8(bytes) {
                                let _ = tx.send(text);
                            }
                        });
                    }
                    return;
                }
                _ => {}
            }
        }
        if let Some(key) = egui_key(event.keysym) {
            self.input.events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat,
                modifiers: self.input.modifiers,
            });
        }
        if pressed
            && !self.input.modifiers.ctrl
            && !self.input.modifiers.alt
            && let Some(text) = event.utf8
            && !text.chars().any(char::is_control)
        {
            self.input.events.push(Event::Text(text));
        }
    }
}
fn egui_key(sym: Keysym) -> Option<Key> {
    Some(match sym {
        Keysym::Escape => Key::Escape,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Delete => Key::Delete,
        Keysym::Insert => Key::Insert,
        Keysym::Left => Key::ArrowLeft,
        Keysym::Right => Key::ArrowRight,
        Keysym::Up => Key::ArrowUp,
        Keysym::Down => Key::ArrowDown,
        Keysym::Home => Key::Home,
        Keysym::End => Key::End,
        Keysym::Page_Up => Key::PageUp,
        Keysym::Page_Down => Key::PageDown,
        Keysym::space => Key::Space,
        Keysym::a | Keysym::A => Key::A,
        Keysym::c | Keysym::C => Key::C,
        Keysym::v | Keysym::V => Key::V,
        Keysym::x | Keysym::X => Key::X,
        Keysym::z | Keysym::Z => Key::Z,
        Keysym::y | Keysym::Y => Key::Y,
        _ => return None,
    })
}
impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        self.scale = factor.max(1);
        if let Some(panel) = &self.ui.panel
            && let super::Menu::Workspace(name) = &panel.kind
        {
            let _ = self
                .ui
                .services
                .preview
                .send(Some((name.clone(), self.scale as f32)));
        }
        self.dirty = true;
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_ready = true;
    }
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
        let (width, height) = (c.new_size.0.max(1), c.new_size.1.max(28));
        if !self.configured || (self.width, self.height) != (width, height) {
            self.width = width;
            self.height = height;
            self.configured = true;
            self.frame_ready = true;
            self.dirty = true;
        }
    }
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, o: wl_output::WlOutput) {
        if self.output.as_ref() == Some(&o)
            && let Some(info) = self.outputs.info(&o)
            && let Some((_, h)) = info.logical_size
        {
            self.output_height = h.max(28) as u32;
            self.dirty = true;
        }
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.data_device.is_none()
            && let Some(manager) = &self.data_manager
        {
            self.data_device = Some(manager.get_data_device(qh, &seat));
        }
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        c: Capability,
    ) {
        match c {
            Capability::Keyboard if self.keyboard.is_none() => {
                self.keyboard = Some(seat.get_keyboard(qh, ()))
            }
            Capability::Pointer if self.pointer.is_none() => {
                self.pointer = self.seats.get_pointer(qh, &seat).ok()
            }
            _ => {}
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        c: Capability,
    ) {
        match c {
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
                self.repeated = None;
            }
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            _ => {}
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}
impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            self.dirty = true;
            let p = pos2(event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.serial = serial;
                    self.input.events.push(Event::PointerMoved(p));
                }
                PointerEventKind::Leave { .. } => self.input.events.push(Event::PointerGone),
                PointerEventKind::Motion { .. } => self.input.events.push(Event::PointerMoved(p)),
                PointerEventKind::Press { button, serial, .. }
                | PointerEventKind::Release { button, serial, .. } => {
                    self.serial = serial;
                    if matches!(event.kind, PointerEventKind::Press { .. })
                        && p.y >= 28.
                        && self.ui.panel.is_some()
                        && !self.ui.panel_rect.contains(p)
                    {
                        self.ui.close();
                        self.ui.hover = None;
                    }
                    let button = match button {
                        0x110 => PointerButton::Primary,
                        0x111 => PointerButton::Secondary,
                        0x112 => PointerButton::Middle,
                        _ => continue,
                    };
                    self.input.events.push(Event::PointerButton {
                        pos: p,
                        button,
                        pressed: matches!(event.kind, PointerEventKind::Press { .. }),
                        modifiers: self.input.modifiers,
                    });
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => self.input.events.push(Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: vec2(-horizontal.absolute as f32, -vertical.absolute as f32),
                    modifiers: self.input.modifiers,
                }),
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => {
                // The compositor can send a descriptor positioned at EOF.
                // pread preserves its shared offset and reads the map from byte zero.
                let mut bytes = vec![0; size as usize];
                match std::fs::File::from(fd)
                    .read_exact_at(&mut bytes, 0)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| app.keymap.keymap(&bytes))
                {
                    Ok(()) => app.repeated = None,
                    Err(error) => eprintln!("Keyboard map: {error}"),
                }
            }
            wl_keyboard::Event::Enter { serial, .. } => {
                app.focus = true;
                app.serial = serial;
                app.dirty = true;
            }
            wl_keyboard::Event::Leave { .. } => {
                app.focus = false;
                app.repeated = None;
                app.keymap.reset_compose();
                app.dirty = true;
            }
            wl_keyboard::Event::Key {
                serial, key, state, ..
            } => {
                app.serial = serial;
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                if pressed && app.repeat_rate > 0 && app.keymap.repeats(key) {
                    app.repeated = Some((
                        key,
                        Instant::now() + Duration::from_millis(app.repeat_delay as u64),
                    ));
                } else if !pressed && app.repeated.is_some_and(|(raw, _)| raw == key) {
                    app.repeated = None;
                }
                if let Some(event) = app.keymap.event(key, pressed) {
                    app.key(event, pressed, false);
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                app.keymap
                    .modifiers(mods_depressed, mods_latched, mods_locked, group);
                let mods = app.keymap.components.mods.0;
                app.input.modifiers = EModifiers {
                    alt: mods & 8 != 0,
                    ctrl: mods & 4 != 0,
                    shift: mods & 1 != 0,
                    mac_cmd: false,
                    command: mods & 4 != 0,
                };
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                app.repeat_rate = rate.max(0) as u32;
                app.repeat_delay = delay.max(0) as u32;
                if rate <= 0 {
                    app.repeated = None;
                }
            }
            _ => {}
        }
    }
}
impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_device::WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_data_device::WlDataDevice) {}
    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_device::WlDataDevice,
        _: f64,
        _: f64,
    ) {
    }
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_device::WlDataDevice,
    ) {
    }
    fn drop_performed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_device::WlDataDevice,
    ) {
    }
}
impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}
impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
        _: String,
        pipe: WritePipe,
    ) {
        let text = self.copied.clone();
        std::thread::spawn(move || {
            let _ = std::fs::File::from(OwnedFd::from(pipe)).write_all(text.as_bytes());
        });
    }
    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
    ) {
        self.copy_source = None;
        self.copied.clear();
    }
    fn dnd_dropped(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
    ) {
    }
    fn dnd_finished(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
    ) {
    }
    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_data_source::WlDataSource,
        _: DndAction,
    ) {
    }
}
delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);
delegate_data_device!(App);
impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
