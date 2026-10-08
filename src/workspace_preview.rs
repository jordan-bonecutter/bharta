use crate::{App, capture::Capture, render::Action, status};
use anyhow::{Context, Result};
use serde_json::Value;
use smithay_client_toolkit::{
    compositor::Region,
    reexports::calloop::channel,
    shell::xdg::{XdgPositioner, popup::Popup},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::{QueueHandle, protocol::wl_shm};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};

const WIDTH: u32 = 380;
const HEIGHT: u32 = 266;
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}
impl Geometry {
    fn read(v: &Value) -> Self {
        Self {
            x: v["x"].as_f64().unwrap_or(0.) as f32,
            y: v["y"].as_f64().unwrap_or(0.) as f32,
            w: v["width"].as_f64().unwrap_or(1.).max(1.) as f32,
            h: v["height"].as_f64().unwrap_or(1.).max(1.) as f32,
        }
    }
}
pub struct Window {
    id: Option<String>,
    title: String,
    rect: Geometry,
    pixels: Option<Pixmap>,
}
pub struct Snapshot {
    pub name: String,
    rect: Geometry,
    windows: Vec<Window>,
    message: String,
}
pub struct Preview {
    pub popup: Popup,
    pub name: String,
    pub ready: bool,
    pub scale: u32,
    snapshot: Option<Snapshot>,
}
fn workspace<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    if v["type"] == "workspace" && v["name"] == name {
        return Some(v);
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|k| v[*k].as_array())
        .flatten()
        .find_map(|c| workspace(c, name))
}
fn collect(v: &Value, result: &mut Vec<Window>) {
    // The capture identifier is optional on older Sway versions. Window
    // discovery must not depend on capture protocol support.
    if v["foreign_toplevel_identifier"].is_string()
        || v["app_id"].is_string()
        || v["window"].is_number()
        || v["window_properties"].is_object()
    {
        result.push(Window {
            id: v["foreign_toplevel_identifier"].as_str().map(str::to_owned),
            title: v["name"].as_str().unwrap_or("Window").into(),
            rect: Geometry::read(&v["rect"]),
            pixels: None,
        });
        return;
    }
    let nodes = v["nodes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let floating = v["floating_nodes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    // A fullscreen child covers its siblings, including floating windows.
    if let Some(full) = nodes
        .iter()
        .chain(floating)
        .find(|c| c["fullscreen_mode"].as_u64().unwrap_or(0) > 0)
    {
        collect(full, result);
        return;
    }
    if matches!(v["layout"].as_str(), Some("tabbed" | "stacked")) {
        let focused = v["focus"].as_array().and_then(|a| a.first());
        if let Some(child) = nodes
            .iter()
            .find(|c| Some(&c["id"]) == focused)
            .or(nodes.first())
        {
            collect(child, result);
        }
    } else {
        for child in nodes {
            collect(child, result);
        }
    }
    for child in floating {
        collect(child, result);
    }
}
fn snapshot(name: &str) -> Result<Snapshot> {
    let tree = status::ipc(4, "")?;
    let ws = workspace(&tree, name).context("Workspace is no longer available")?;
    let mut windows = Vec::new();
    collect(ws, &mut windows);
    windows.truncate(32);
    Ok(Snapshot {
        name: name.into(),
        rect: Geometry::read(&ws["rect"]),
        windows,
        message: String::new(),
    })
}
pub fn watch(sender: channel::Sender<Snapshot>) -> mpsc::Sender<Option<String>> {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut capture = None;
        let mut target = None;
        let mut retry_capture = Instant::now();
        loop {
            let request = if target.is_some() {
                rx.recv_timeout(Duration::from_millis(200))
            } else {
                rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            match request {
                Ok(value) => target = value,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            while let Ok(value) = rx.try_recv() {
                target = value;
            }
            let Some(name) = target.clone() else {
                capture = None;
                continue;
            };
            let mut shot = match snapshot(&name) {
                Ok(s) => s,
                Err(_) => Snapshot {
                    name: name.clone(),
                    rect: Geometry {
                        x: 0.,
                        y: 0.,
                        w: 1.,
                        h: 1.,
                    },
                    windows: vec![],
                    message: "Workspace unavailable".into(),
                },
            };
            let capturable = shot.windows.iter().any(|w| w.id.is_some());
            if !shot.windows.is_empty() && !capturable {
                shot.message = "Window layout · live capture needs Sway 1.12+".into();
            }
            if capture.is_none() && capturable && Instant::now() >= retry_capture {
                match Capture::new() {
                    Ok(c) => capture = Some(c),
                    Err(error) => {
                        eprintln!("Workspace capture unavailable: {error:#}");
                        retry_capture = Instant::now() + Duration::from_secs(5);
                    }
                }
            }
            if capturable && capture.is_none() {
                shot.message = "Window layout · live capture unavailable".into();
            }
            let mut cancelled = false;
            if let Some(c) = &mut capture {
                for window in &mut shot.windows {
                    if let Ok(value) = rx.try_recv() {
                        target = value;
                        cancelled = true;
                        break;
                    }
                    window.pixels = window.id.as_deref().and_then(|id| c.window(id).ok());
                }
            }
            if cancelled {
                continue;
            }
            if shot.windows.iter().any(|w| w.pixels.is_none()) && shot.message.is_empty() {
                shot.message = "Window layout · some captures unavailable".into();
            }
            if sender.send(shot).is_err() {
                break;
            }
        }
    });
    tx
}
impl App {
    pub(crate) fn hover_workspace(&mut self, x: f32) {
        let target = self
            .hits
            .iter()
            .find(|h| x >= h.start && x < h.end)
            .and_then(|h| {
                if let Action::Workspace(name) = &h.action {
                    Some((name.clone(), ((h.start + h.end) / 2.) as i32))
                } else {
                    None
                }
            });
        if target.as_ref().map(|t| &t.0) == self.preview_target.as_ref().map(|t| &t.0) {
            return;
        }
        self.close_preview();
        if self.panel.is_none() {
            self.preview_target = target.map(|(n, x)| (n, x, Instant::now()));
        }
    }
    pub(crate) fn close_preview(&mut self) {
        self.preview_target = None;
        self.workspace_preview = None;
        let _ = self.preview_requests.send(None);
    }
    pub(crate) fn tick_preview(&mut self, qh: &QueueHandle<Self>) {
        let Some((name, x, since)) = &self.preview_target else {
            return;
        };
        if self.workspace_preview.is_some()
            || self.panel.is_some()
            || since.elapsed() < Duration::from_millis(220)
        {
            return;
        }
        let (name, x) = (name.clone(), *x);
        let result = (|| -> Result<()> {
            let position = XdgPositioner::new(&self.xdg)?;
            position.set_size(WIDTH as i32, HEIGHT as i32);
            position.set_anchor_rect(x, 28, 1, 1);
            position.set_anchor(Anchor::Bottom);
            position.set_gravity(Gravity::Bottom);
            position.set_constraint_adjustment(
                ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
            );
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
            // A tooltip is input-transparent and never grabs the seat or keyboard.
            let region = Region::new(&self.compositor)?;
            popup
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
            popup.wl_surface().commit();
            self.workspace_preview = Some(Preview {
                popup,
                name: name.clone(),
                ready: false,
                scale: self.scale,
                snapshot: None,
            });
            self.preview_requests.send(Some(name))?;
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("Workspace preview: {e}");
            self.close_preview();
        }
    }
    pub(crate) fn preview_result(&mut self, snapshot: Snapshot) {
        if let Some(p) = &mut self.workspace_preview
            && p.name == snapshot.name
        {
            p.snapshot = Some(snapshot);
            self.draw_preview();
        }
    }
    pub(crate) fn draw_preview(&mut self) {
        let Some(p) = &self.workspace_preview else {
            return;
        };
        if !p.ready {
            return;
        }
        let pix = render(&self.renderer, &p.name, p.snapshot.as_ref(), p.scale);
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
            p.popup
                .xdg_surface()
                .set_window_geometry(0, 0, WIDTH as i32, HEIGHT as i32);
            p.popup.wl_surface().set_buffer_scale(p.scale as i32);
            p.popup.wl_surface().damage_buffer(0, 0, w, h);
            buffer.attach_to(p.popup.wl_surface())?;
            p.popup.wl_surface().commit();
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("Workspace preview drawing: {e}");
            self.close_preview();
        }
    }
}
fn fill(pix: &mut Pixmap, r: [f32; 4], color: Color, scale: f32) {
    if let Some(rect) = Rect::from_xywh(r[0], r[1], r[2], r[3]) {
        let mut paint = Paint::default();
        paint.set_color(color);
        pix.fill_rect(rect, &paint, Transform::from_scale(scale, scale), None);
    }
}
fn render(
    renderer: &crate::render::Renderer,
    name: &str,
    shot: Option<&Snapshot>,
    scale: u32,
) -> Pixmap {
    let mut pix = Pixmap::new(WIDTH * scale, HEIGHT * scale).unwrap();
    let scale = scale as f32;
    let (bg, fg, muted) = if renderer.dark {
        (
            Color::from_rgba8(36, 30, 25, 255),
            Color::from_rgba8(255, 241, 218, 255),
            Color::from_rgba8(185, 165, 142, 255),
        )
    } else {
        (
            Color::from_rgba8(249, 244, 235, 255),
            Color::from_rgba8(52, 40, 29, 255),
            Color::from_rgba8(112, 91, 68, 255),
        )
    };
    crate::panel::rounded(
        &mut pix,
        [4., 5., 372., 256.],
        12.,
        Color::from_rgba8(0, 0, 0, 65),
        scale,
    );
    crate::panel::rounded(&mut pix, [6., 3., 368., 254.], 11., bg, scale);
    let title: String = format!("Workspace {name}")
        .chars()
        .filter(|c| !c.is_control())
        .take(34)
        .collect();
    renderer.text_at(&mut pix, &title, 18., 10., scale, fg);
    let Some(shot) = shot else {
        renderer.text_at(&mut pix, "Loading preview…", 18., 100., scale, muted);
        return pix;
    };
    let ratio = (344. / shot.rect.w).min(184. / shot.rect.h);
    let ox = 18. + (344. - shot.rect.w * ratio) / 2.;
    let oy = 44. + (184. - shot.rect.h * ratio) / 2.;
    fill(
        &mut pix,
        [ox, oy, shot.rect.w * ratio, shot.rect.h * ratio],
        Color::from_rgba8(24, 20, 17, 255),
        scale,
    );
    // Clip floating windows to workspace bounds and keep chrome outside the image.
    let mut mask_pix = Pixmap::new(pix.width(), pix.height()).unwrap();
    fill(
        &mut mask_pix,
        [ox, oy, shot.rect.w * ratio, shot.rect.h * ratio],
        Color::WHITE,
        scale,
    );
    let mask = tiny_skia::Mask::from_pixmap(mask_pix.as_ref(), tiny_skia::MaskType::Alpha);
    for window in &shot.windows {
        let x = ox + (window.rect.x - shot.rect.x) * ratio;
        let y = oy + (window.rect.y - shot.rect.y) * ratio;
        let (w, h) = (window.rect.w * ratio, window.rect.h * ratio);
        let mut tile = Pixmap::new(pix.width(), pix.height()).unwrap();
        fill(
            &mut tile,
            [x, y, w, h],
            Color::from_rgba8(112, 84, 62, 255),
            scale,
        );
        if let Some(image) = &window.pixels {
            let iw = (w - 2.).max(1.);
            let ih = (h - 2.).max(1.);
            tile.draw_pixmap(
                0,
                0,
                image.as_ref(),
                &tiny_skia::PixmapPaint {
                    quality: tiny_skia::FilterQuality::Bilinear,
                    ..Default::default()
                },
                Transform::from_scale(
                    iw * scale / image.width() as f32,
                    ih * scale / image.height() as f32,
                )
                .post_translate((x + 1.) * scale, (y + 1.) * scale),
                None,
            );
        } else {
            let title: String = window
                .title
                .chars()
                .filter(|c| !c.is_control())
                .take((w / 8.).max(0.) as usize)
                .collect();
            renderer.text_at(&mut tile, &title, x + 3., y + 2., scale, fg);
        }
        pix.draw_pixmap(
            0,
            0,
            tile.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            Transform::identity(),
            Some(&mask),
        );
    }
    let footer = if !shot.message.is_empty() {
        shot.message.as_str()
    } else if shot.windows.is_empty() {
        "Empty workspace"
    } else {
        "Live preview · click workspace to switch"
    };
    renderer.text_at(&mut pix, footer, 18., 228., scale, muted);
    pix
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn hidden_tabbed_workspace_uses_selected_tab_and_floating_windows() {
        let leaf =
            |id| json!({"id":id,"foreign_toplevel_identifier":format!("w{id}"),"visible":false});
        let tree = json!({"nodes":[{"type":"workspace","name":"other","layout":"tabbed","focus":[2,1],"nodes":[leaf(1),leaf(2)],"floating_nodes":[leaf(3)]}]});
        let mut windows = vec![];
        collect(workspace(&tree, "other").unwrap(), &mut windows);
        assert_eq!(
            windows
                .iter()
                .map(|w| w.id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["w2", "w3"]
        );
        assert!(workspace(&tree, "missing").is_none());
    }
    #[test]
    fn older_sway_windows_are_not_mistaken_for_an_empty_workspace() {
        let tree = json!({"type":"workspace", "nodes":[
            {"type":"con","app_id":"foot","name":"Terminal","rect":{"width":900,"height":700}},
            {"type":"con","window":123,"window_properties":{"class":"Firefox"},"name":"Browser"}
        ], "floating_nodes":[{"type":"floating_con","nodes":[
            {"type":"con","app_id":"settings","name":"Settings"}
        ]}]});
        let mut windows = vec![];
        collect(&tree, &mut windows);
        assert_eq!(windows.len(), 3);
        assert!(windows.iter().all(|w| w.id.is_none()));
        assert_eq!(windows[0].title, "Terminal");
        let mut empty = vec![];
        collect(&json!({"type":"workspace","nodes":[]}), &mut empty);
        assert!(empty.is_empty());
    }
    #[test]
    fn fullscreen_hides_siblings() {
        let tree = json!({"nodes":[{"foreign_toplevel_identifier":"a"},{"foreign_toplevel_identifier":"b","fullscreen_mode":1}],"floating_nodes":[{"foreign_toplevel_identifier":"c"}]});
        let mut windows = vec![];
        collect(&tree, &mut windows);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id.as_deref(), Some("b"));
    }
}
