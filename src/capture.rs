//! On-demand toplevel capture. Pixels never leave memory or the local Wayland connection.
use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    delegate_shm,
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::{
    collections::HashMap,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output, wl_registry, wl_shm},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1 as handle, ext_foreign_toplevel_list_v1 as list,
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1 as source_manager,
        ext_image_capture_source_v1 as source,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as manager,
        ext_image_copy_capture_session_v1 as session,
    },
};
struct State {
    shm: Shm,
    handles: HashMap<String, handle::ExtForeignToplevelHandleV1>,
    generation: u64,
    size: (u32, u32),
    format: Option<wl_shm::Format>,
    constraints: bool,
    finished: bool,
    failed: bool,
    transform: wl_output::Transform,
}
pub struct Capture {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    sources: source_manager::ExtForeignToplevelImageCaptureSourceManagerV1,
    manager: manager::ExtImageCopyCaptureManagerV1,
    _list: list::ExtForeignToplevelListV1,
}
impl Capture {
    pub fn new() -> Result<Self> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let sources = globals
            .bind(&qh, 1..=1, ())
            .context("Sway does not offer individual window capture")?;
        let manager = globals.bind(&qh, 1..=1, ())?;
        let list = globals.bind(&qh, 1..=1, ())?;
        let mut state = State {
            shm: Shm::bind(&globals, &qh)?,
            handles: HashMap::new(),
            generation: 0,
            size: (0, 0),
            format: None,
            constraints: false,
            finished: false,
            failed: false,
            transform: wl_output::Transform::Normal,
        };
        queue.roundtrip(&mut state)?;
        Ok(Self {
            conn,
            queue,
            state,
            sources,
            manager,
            _list: list,
        })
    }
    fn pump(&mut self, deadline: Instant, done: impl Fn(&State) -> bool) -> Result<()> {
        loop {
            self.queue.dispatch_pending(&mut self.state)?;
            if done(&self.state) {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "Window capture timed out");
            self.conn.flush()?;
            if let Some(guard) = self.queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: self.conn.backend().poll_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let timeout = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(100) as i32;
                let result = unsafe { libc::poll(&mut fd, 1, timeout) };
                if result > 0 {
                    guard.read()?;
                } else if result < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
    }
    pub fn window(&mut self, id: &str) -> Result<tiny_skia::Pixmap> {
        // Process new/closed toplevels even when the previous workspace was empty.
        self.queue.roundtrip(&mut self.state)?;
        let handle = self
            .state
            .handles
            .get(id)
            .context("Window is no longer available")?
            .clone();
        self.state.generation += 1;
        self.state.size = (0, 0);
        self.state.format = None;
        self.state.constraints = false;
        self.state.finished = false;
        self.state.failed = false;
        self.state.transform = wl_output::Transform::Normal;
        let qh = self.queue.handle();
        let source = self.sources.create_source(&handle, &qh, ());
        let session = self.manager.create_session(
            &source,
            manager::Options::empty(),
            &qh,
            self.state.generation,
        );
        let result = self.capture_frame(&session);
        session.destroy();
        source.destroy();
        self.conn.flush()?;
        result
    }
    fn capture_frame(
        &mut self,
        session: &session::ExtImageCopyCaptureSessionV1,
    ) -> Result<tiny_skia::Pixmap> {
        let deadline = Instant::now() + Duration::from_millis(300);
        self.pump(deadline, |s| s.constraints || s.failed)?;
        ensure!(!self.state.failed, "Window capture stopped");
        let (w, h) = self.state.size;
        ensure!(
            w > 0 && h > 0 && u64::from(w) * u64::from(h) <= 16_777_216,
            "Unsupported window size"
        );
        let format = self
            .state
            .format
            .context("No supported shared memory format")?;
        let mut pool = SlotPool::new(w as usize * h as usize * 4, &self.state.shm)?;
        let (buffer, _) = pool.create_buffer(w as i32, h as i32, w as i32 * 4, format)?;
        let frame = session.create_frame(&self.queue.handle(), self.state.generation);
        frame.attach_buffer(buffer.wl_buffer());
        frame.damage_buffer(0, 0, w as i32, h as i32);
        frame.capture();
        let result = self.pump(deadline, |s| s.finished || s.failed);
        frame.destroy();
        result?;
        ensure!(!self.state.failed, "Window capture failed");
        // Toplevel sources normally use Normal. Avoid showing an incorrectly rotated frame.
        ensure!(
            self.state.transform == wl_output::Transform::Normal,
            "Unsupported window transform"
        );
        let bytes = pool.canvas(&buffer).context("Capture buffer unavailable")?;
        let ratio = (640.0 / w as f64).min(480.0 / h as f64).min(1.0);
        let tw = (w as f64 * ratio).round().max(1.0) as u32;
        let th = (h as f64 * ratio).round().max(1.0) as u32;
        let mut pix = tiny_skia::Pixmap::new(tw, th).context("Thumbnail allocation failed")?;
        for y in 0..th {
            for x in 0..tw {
                let offset = ((y as u64 * h as u64 / th as u64) * w as u64
                    + x as u64 * w as u64 / tw as u64) as usize
                    * 4;
                let pixel = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
                let [a, mut r, g, mut b] = pixel.to_be_bytes();
                if matches!(format, wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888) {
                    std::mem::swap(&mut r, &mut b);
                }
                let alpha = if matches!(format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888)
                {
                    255
                } else {
                    a
                };
                let dest = ((y * tw + x) * 4) as usize;
                pix.data_mut()[dest..dest + 4].copy_from_slice(&[
                    r.min(alpha),
                    g.min(alpha),
                    b.min(alpha),
                    alpha,
                ]);
            }
        }
        Ok(pix)
    }
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<list::ExtForeignToplevelListV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &list::ExtForeignToplevelListV1,
        _: list::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
    wayland_client::event_created_child!(State, list::ExtForeignToplevelListV1, [0 => (handle::ExtForeignToplevelHandleV1, ())]);
}
impl Dispatch<handle::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        s: &mut Self,
        h: &handle::ExtForeignToplevelHandleV1,
        e: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            handle::Event::Identifier { identifier } => {
                s.handles.insert(identifier, h.clone());
            }
            handle::Event::Closed => {
                s.handles.retain(|_, v| v != h);
                h.destroy();
            }
            _ => {}
        }
    }
}
impl Dispatch<session::ExtImageCopyCaptureSessionV1, u64> for State {
    fn event(
        s: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        e: session::Event,
        generation: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *generation != s.generation {
            return;
        }
        match e {
            session::Event::BufferSize { width, height } => s.size = (width, height),
            session::Event::ShmFormat {
                format: WEnum::Value(f),
            } if matches!(
                f,
                wl_shm::Format::Argb8888
                    | wl_shm::Format::Xrgb8888
                    | wl_shm::Format::Abgr8888
                    | wl_shm::Format::Xbgr8888
            ) =>
            {
                s.format = Some(f)
            }
            session::Event::Done => s.constraints = true,
            session::Event::Stopped => s.failed = true,
            _ => {}
        }
    }
}
impl Dispatch<frame::ExtImageCopyCaptureFrameV1, u64> for State {
    fn event(
        s: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        e: frame::Event,
        generation: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *generation != s.generation {
            return;
        }
        match e {
            frame::Event::Ready => s.finished = true,
            frame::Event::Failed { .. } => s.failed = true,
            frame::Event::Transform {
                transform: WEnum::Value(t),
            } => s.transform = t,
            _ => {}
        }
    }
}
impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
delegate_shm!(State);
delegate_noop!(State: ignore source_manager::ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(State: ignore source::ExtImageCaptureSourceV1);
delegate_noop!(State: ignore manager::ExtImageCopyCaptureManagerV1);
