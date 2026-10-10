//! Capturable fixture windows, only on the private headless compositor.
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_registry, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::collections::HashMap;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};
fn main() -> anyhow::Result<()> {
    assert_eq!(std::env::var("BHARTA_HEADLESS").as_deref(), Ok("1"));
    if let Ok(path) = std::env::var("BHARTA_TEST_WINDOW_PID") {
        std::fs::write(path, std::process::id().to_string())?;
    }
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = XdgShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let pool = SlotPool::new(4096, &shm)?;
    let mut app = Fixture {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        windows: vec![],
        sizes: HashMap::new(),
        tick: 0,
        exit: false,
    };
    for title in [
        "Preview fixture — wide window",
        "Preview fixture — second window",
    ] {
        let window = shell.create_window(
            compositor.create_surface(&qh),
            WindowDecorations::RequestServer,
            &qh,
        );
        window.set_title(title);
        window.set_app_id("bharta-fixture");
        window.commit();
        app.windows.push(window);
    }
    while !app.exit {
        queue.blocking_dispatch(&mut app)?;
    }
    Ok(())
}
struct Fixture {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    windows: Vec<Window>,
    sizes: HashMap<wl_surface::WlSurface, (i32, i32)>,
    tick: u32,
    exit: bool,
}
impl WindowHandler for Fixture {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _qh: &QueueHandle<Self>,
        window: &Window,
        c: WindowConfigure,
        _: u32,
    ) {
        let w = c.new_size.0.map(|n| n.get()).unwrap_or(1400) as i32;
        let h = c.new_size.1.map(|n| n.get()).unwrap_or(900) as i32;
        self.sizes.insert(window.wl_surface().clone(), (w, h));
        self.draw(window);
        if self.windows.first() == Some(window) {
            window.wl_surface().frame(_qh, window.wl_surface().clone());
            window.commit();
        }
    }
}
impl Fixture {
    fn draw(&mut self, window: &Window) {
        let second = self.windows.get(1) == Some(window);
        let (w, h) = self.sizes[window.wl_surface()];
        let (buffer, pixels) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)
            .unwrap();
        for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = i as i32 % w;
            let y = i as i32 / w;
            *pixel = if ((x + self.tick as i32 * 12) / 80 + y / 80) % 2 == 0 {
                [80, 60, 40, 255]
            } else {
                [60, 45, 30, 255]
            };
            if second {
                pixel.swap(0, 1);
            }
        }
        window.wl_surface().damage_buffer(0, 0, w, h);
        buffer.attach_to(window.wl_surface()).unwrap();
        window.commit();
    }
}
impl CompositorHandler for Fixture {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        if let Some(window) = self.windows.first().cloned() {
            if std::env::var_os("BHARTA_TEST_ANIMATE")
                .is_some_and(|p| std::path::Path::new(&p).exists())
            {
                self.tick += 1;
                self.draw(&window);
            }
            window.wl_surface().frame(qh, window.wl_surface().clone());
            window.commit();
        }
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
impl OutputHandler for Fixture {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl ShmHandler for Fixture {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for Fixture {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}
delegate_compositor!(Fixture);
delegate_output!(Fixture);
delegate_shm!(Fixture);
delegate_registry!(Fixture);
delegate_xdg_shell!(Fixture);
delegate_xdg_window!(Fixture);
