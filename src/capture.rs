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
#[derive(Default)]
struct ImageState {
    size: (u32, u32),
    format: Option<wl_shm::Format>,
    constraints: bool,
    finished: bool,
    failed: bool,
    transform: Option<wl_output::Transform>,
}
struct State {
    shm: Shm,
    handles: HashMap<String, handle::ExtForeignToplevelHandleV1>,
    images: HashMap<String, ImageState>,
}
struct WindowCapture {
    source: source::ExtImageCaptureSourceV1,
    session: session::ExtImageCopyCaptureSessionV1,
    frame: Option<frame::ExtImageCopyCaptureFrameV1>,
    buffer: Option<smithay_client_toolkit::shm::slot::Buffer>,
    size: (u32, u32),
    format: Option<wl_shm::Format>,
}
impl Drop for WindowCapture {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
        self.session.destroy();
        self.source.destroy();
    }
}
pub struct Capture {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    pool: SlotPool,
    windows: HashMap<String, WindowCapture>,
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
        let shm = Shm::bind(&globals, &qh)?;
        let pool = SlotPool::new(4096, &shm)?;
        let mut state = State {
            shm,
            handles: HashMap::new(),
            images: HashMap::new(),
        };
        queue.roundtrip(&mut state)?;
        Ok(Self {
            conn,
            queue,
            state,
            pool,
            windows: HashMap::new(),
            sources,
            manager,
            _list: list,
        })
    }
    // All windows have one outstanding frame each. A quiet or hidden source
    // waits for damage without holding up an animating source or the UI thread.
    pub fn poll(
        &mut self,
        targets: &[(String, u32, u32)],
    ) -> Result<Vec<(String, tiny_skia::Pixmap)>> {
        self.conn.flush()?;
        self.queue.dispatch_pending(&mut self.state)?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: self.conn.backend().poll_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 0) } > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        self.windows.retain(|id, _| {
            targets.iter().any(|(name, _, _)| name == id) && self.state.handles.contains_key(id)
        });
        self.state
            .images
            .retain(|id, _| self.windows.contains_key(id));
        let qh = self.queue.handle();
        let mut ready = vec![];
        for (id, tw, th) in targets {
            if !self.windows.contains_key(id) {
                let Some(handle) = self.state.handles.get(id) else {
                    continue;
                };
                let source = self.sources.create_source(handle, &qh, ());
                let session = self.manager.create_session(
                    &source,
                    manager::Options::empty(),
                    &qh,
                    id.clone(),
                );
                self.windows.insert(
                    id.clone(),
                    WindowCapture {
                        source,
                        session,
                        frame: None,
                        buffer: None,
                        size: (0, 0),
                        format: None,
                    },
                );
                self.state.images.insert(id.clone(), ImageState::default());
            }
            let image = self.state.images.get_mut(id).unwrap();
            let window = self.windows.get_mut(id).unwrap();
            if image.finished {
                if let Some(frame) = window.frame.take() {
                    frame.destroy();
                }
                if image
                    .transform
                    .is_none_or(|t| t == wl_output::Transform::Normal)
                    && let Some(buffer) = &window.buffer
                    && let Some(bytes) = self.pool.canvas(buffer)
                {
                    ready.push((
                        id.clone(),
                        thumbnail(bytes, window.size, window.format.unwrap(), (*tw, *th))?,
                    ));
                }
                image.finished = false;
            }
            if image.failed {
                self.windows.remove(id);
                self.state.images.remove(id);
                continue;
            }
            if !image.constraints || window.frame.is_some() {
                continue;
            }
            let (w, h) = image.size;
            ensure!(
                w > 0 && h > 0 && u64::from(w) * u64::from(h) <= 16_777_216,
                "Unsupported window size"
            );
            let Some(format) = image.format else {
                continue;
            };
            let reallocate = window.size != image.size || window.format != Some(format);
            if reallocate {
                window.buffer = None;
                let (buffer, _) =
                    self.pool
                        .create_buffer(w as i32, h as i32, w as i32 * 4, format)?;
                window.buffer = Some(buffer);
                window.size = image.size;
                window.format = Some(format);
            }
            let frame = window.session.create_frame(&qh, id.clone());
            frame.attach_buffer(window.buffer.as_ref().unwrap().wl_buffer());
            if reallocate {
                frame.damage_buffer(0, 0, w as i32, h as i32);
            }
            frame.capture();
            window.frame = Some(frame);
        }
        self.conn.flush()?;
        Ok(ready)
    }
    #[allow(dead_code)] // Standalone capture probe keeps its blocking single-shot API.
    pub fn window(&mut self, id: &str) -> Result<tiny_skia::Pixmap> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Some((_, pixels)) = self.poll(&[(id.into(), 640, 480)])?.into_iter().next() {
                return Ok(pixels);
            }
            ensure!(Instant::now() < deadline, "Window capture timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
// Box/area filtering averages premultiplied pixels directly from shared memory.
// It allocates only the thumbnail, preserving alpha without a full-size RGBA copy.
fn thumbnail(
    bytes: &[u8],
    (w, h): (u32, u32),
    format: wl_shm::Format,
    (max_w, max_h): (u32, u32),
) -> Result<tiny_skia::Pixmap> {
    let ratio = (max_w.max(1) as f64 / w as f64)
        .min(max_h.max(1) as f64 / h as f64)
        .min(1.);
    let tw = (w as f64 * ratio).round().max(1.) as u32;
    let th = (h as f64 * ratio).round().max(1.) as u32;
    let mut pix = tiny_skia::Pixmap::new(tw, th).context("Thumbnail allocation failed")?;
    let opaque = matches!(format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888);
    let swap = matches!(format, wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888);
    for y in 0..th {
        let (y0, y1) = (
            y as u64 * h as u64 / th as u64,
            (y + 1) as u64 * h as u64 / th as u64,
        );
        for x in 0..tw {
            let (x0, x1) = (
                x as u64 * w as u64 / tw as u64,
                (x + 1) as u64 * w as u64 / tw as u64,
            );
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let start = ((sy * w as u64 + x0) * 4) as usize;
                let end = ((sy * w as u64 + x1) * 4) as usize;
                for pixel in bytes[start..end].as_chunks::<4>().0 {
                    let [a, mut r, g, mut b] = u32::from_ne_bytes(*pixel).to_be_bytes();
                    if swap {
                        std::mem::swap(&mut r, &mut b);
                    }
                    let a = if opaque { 255 } else { a };
                    for (sum, c) in sum.iter_mut().zip([r.min(a), g.min(a), b.min(a), a]) {
                        *sum += c as u64;
                    }
                }
            }
            let count = (x1 - x0) * (y1 - y0);
            let dest = ((y * tw + x) * 4) as usize;
            for (out, sum) in pix.data_mut()[dest..dest + 4].iter_mut().zip(sum) {
                *out = ((sum + count / 2) / count) as u8;
            }
        }
    }
    Ok(pix)
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
impl Dispatch<session::ExtImageCopyCaptureSessionV1, String> for State {
    fn event(
        s: &mut Self,
        _: &session::ExtImageCopyCaptureSessionV1,
        e: session::Event,
        id: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(s) = s.images.get_mut(id) else {
            return;
        };
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
impl Dispatch<frame::ExtImageCopyCaptureFrameV1, String> for State {
    fn event(
        s: &mut Self,
        _: &frame::ExtImageCopyCaptureFrameV1,
        e: frame::Event,
        id: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(s) = s.images.get_mut(id) else {
            return;
        };
        match e {
            frame::Event::Ready => s.finished = true,
            frame::Event::Failed { .. } => s.failed = true,
            frame::Event::Transform {
                transform: WEnum::Value(t),
            } => s.transform = Some(t),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn area_filter_removes_checkerboard_aliasing_and_preserves_alpha() {
        for format in [wl_shm::Format::Argb8888, wl_shm::Format::Abgr8888] {
            let bytes: Vec<_> = (0..64)
                .flat_map(|i| {
                    let value = if (i % 8 + i / 8) % 2 == 0 { 0 } else { 128 };
                    u32::from_be_bytes([128, value, value, value]).to_ne_bytes()
                })
                .collect();
            let image = thumbnail(&bytes, (8, 8), format, (2, 2)).unwrap();
            assert_eq!((image.width(), image.height()), (2, 2));
            for pixel in image.data().as_chunks::<4>().0 {
                assert_eq!(*pixel, [64, 64, 64, 128]);
            }
        }
    }
    #[test]
    fn opaque_formats_and_odd_dimensions_remain_bounded() {
        for (format, pixel) in [
            (wl_shm::Format::Xrgb8888, [0, 80, 40, 20]),
            (wl_shm::Format::Xbgr8888, [0, 20, 40, 80]),
        ] {
            let bytes: Vec<_> = (0..35)
                .flat_map(|_| u32::from_be_bytes(pixel).to_ne_bytes())
                .collect();
            let image = thumbnail(&bytes, (7, 5), format, (3, 2)).unwrap();
            assert!(image.width() <= 3 && image.height() <= 2);
            for pixel in image.data().as_chunks::<4>().0 {
                assert_eq!(*pixel, [80, 40, 20, 255]);
            }
        }
    }
}
