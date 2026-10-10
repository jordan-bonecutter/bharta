use crate::{capture::Capture, status};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};
use tiny_skia::Pixmap;

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
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
#[derive(Clone)]
pub struct Window {
    id: Option<String>,
    pub title: String,
    pub rect: Geometry,
    pub pixels: Option<Arc<Pixmap>>,
    pub content: Geometry,
    pub decorations: Vec<Decoration>,
    pub state: usize,
}
#[derive(Clone)]
pub struct Decoration {
    pub rect: Geometry,
    pub title: String,
    pub state: usize,
}
#[derive(Clone)]
pub struct Palette(pub [[egui::Color32; 4]; 5]);
impl Default for Palette {
    fn default() -> Self {
        Self(
            [
                ["#4c7899", "#285577", "#ffffff", "#2e9ef4"],
                ["#333333", "#5f676a", "#ffffff", "#484e50"],
                ["#333333", "#222222", "#888888", "#292d2e"],
                ["#2f343a", "#900000", "#ffffff", "#900000"],
                ["#333333", "#5f676a", "#ffffff", "#484e50"],
            ]
            .map(|row| row.map(|color| egui::Color32::from_hex(color).unwrap())),
        )
    }
}
impl Palette {
    // GET_CONFIG contains the main file, not the effective theme. Literal
    // client colors and simple variables override the standard Sway defaults;
    // included files and runtime theme commands are not exposed here.
    fn read(config: &str) -> Self {
        let mut palette = Self::default();
        let mut variables = std::collections::HashMap::new();
        let names = [
            "client.focused",
            "client.focused_inactive",
            "client.unfocused",
            "client.urgent",
            "client.placeholder",
        ];
        for line in config.lines() {
            let words: Vec<_> = line.split_whitespace().collect();
            if words.len() >= 3 && words[0] == "set" {
                variables.insert(words[1], words[2]);
            }
            if let Some(index) = words
                .first()
                .and_then(|name| names.iter().position(|n| n == name))
            {
                for (dest, word) in palette.0[index]
                    .iter_mut()
                    .zip(words.iter().skip(1).take(4))
                {
                    let value = variables.get(word).copied().unwrap_or(word);
                    if let Ok(color) = egui::Color32::from_hex(value) {
                        *dest = color;
                    }
                }
            }
        }
        palette
    }
}
fn state(v: &Value, selected: bool) -> usize {
    if v["urgent"] == true {
        3
    } else if v["focused"] == true {
        0
    } else if selected {
        1
    } else {
        2
    }
}
fn decoration(
    v: &Value,
    layout: &str,
    index: usize,
    count: usize,
    selected: bool,
) -> Option<Decoration> {
    let height = v["deco_rect"]["height"].as_f64().unwrap_or(0.) as f32;
    if height <= 0. || v["fullscreen_mode"].as_u64().unwrap_or(0) > 0 {
        return None;
    }
    // Sway IPC excludes the title rows from rect, while window_rect is
    // relative to that rect. deco_rect is relative to the parent container.
    let rect = Geometry::read(&v["rect"]);
    let width = v["deco_rect"]["width"].as_f64().unwrap_or(0.) as f32;
    Some(Decoration {
        rect: Geometry {
            x: rect.x
                + if layout == "tabbed" {
                    width * index as f32
                } else {
                    0.
                },
            y: rect.y
                - height
                    * if layout == "stacked" {
                        count as f32
                    } else {
                        1.
                    }
                + if layout == "stacked" {
                    height * index as f32
                } else {
                    0.
                },
            w: width,
            h: height,
        },
        title: v["name"].as_str().unwrap_or("Window").into(),
        state: state(v, selected),
    })
}
#[derive(Clone)]
pub struct Snapshot {
    pub name: String,
    pub rect: Geometry,
    pub windows: Vec<Window>,
    pub message: String,
    pub palette: Palette,
}
impl Snapshot {
    pub fn ready(&self) -> bool {
        !self.message.is_empty()
            || self
                .windows
                .iter()
                .all(|w| w.id.is_none() || w.pixels.is_some())
    }
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
    collect_node(v, result, true, false);
}
fn collect_node(v: &Value, result: &mut Vec<Window>, selected: bool, grouped: bool) {
    // The capture identifier is optional on older Sway versions. Window
    // discovery must not depend on capture protocol support.
    if v["foreign_toplevel_identifier"].is_string()
        || v["app_id"].is_string()
        || v["window"].is_number()
        || v["window_properties"].is_object()
    {
        let rect = Geometry::read(&v["rect"]);
        let mut content = rect;
        if v["window_rect"]["width"].as_f64().unwrap_or(0.) > 0. {
            content = Geometry::read(&v["window_rect"]);
            content.x += rect.x;
            content.y += rect.y;
        }
        result.push(Window {
            id: v["foreign_toplevel_identifier"].as_str().map(str::to_owned),
            title: v["name"].as_str().unwrap_or("Window").into(),
            rect,
            pixels: None,
            content,
            state: state(v, selected),
            decorations: if grouped {
                vec![]
            } else {
                decoration(v, "", 0, 1, selected).into_iter().collect()
            },
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
        collect_node(full, result, true, false);
        return;
    }
    if matches!(v["layout"].as_str(), Some("tabbed" | "stacked")) {
        let focused = v["focus"].as_array().and_then(|a| a.first());
        if let Some(child) = nodes
            .iter()
            .find(|c| Some(&c["id"]) == focused)
            .or(nodes.first())
        {
            let first = result.len();
            collect_node(child, result, selected, true);
            if let Some(window) = result.get_mut(first) {
                window.decorations.splice(
                    0..0,
                    nodes.iter().enumerate().filter_map(|(i, node)| {
                        decoration(
                            node,
                            v["layout"].as_str().unwrap_or(""),
                            i,
                            nodes.len(),
                            selected && node["id"] == child["id"],
                        )
                    }),
                );
            }
        }
    } else {
        for child in nodes {
            collect_node(
                child,
                result,
                selected && Some(&child["id"]) == v["focus"].as_array().and_then(|a| a.first()),
                false,
            );
        }
    }
    for child in floating {
        collect_node(
            child,
            result,
            selected && Some(&child["id"]) == v["focus"].as_array().and_then(|a| a.first()),
            false,
        );
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
        palette: status::ipc(9, "")
            .ok()
            .and_then(|v| v["config"].as_str().map(Palette::read))
            .unwrap_or_default(),
    })
}
pub type Request = (String, f32, [f32; 2]);
pub fn watch(sender: mpsc::SyncSender<Snapshot>) -> mpsc::Sender<Option<Request>> {
    let (tx, rx) = mpsc::channel::<Option<Request>>();
    std::thread::spawn(move || {
        let mut capture = None;
        let mut target = None;
        let mut layout: Option<Snapshot> = None;
        let mut pixels = std::collections::HashMap::new();
        let mut refresh = Instant::now();
        let mut retry = Instant::now();
        loop {
            let request = if target.is_some() {
                rx.recv_timeout(crate::config::get().duration("intervals.preview_poll_ms"))
            } else {
                rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            let mut changed = false;
            match request {
                Ok(value) => {
                    changed = target != value;
                    target = value;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            while let Ok(value) = rx.try_recv() {
                changed |= target != value;
                target = value;
            }
            let Some((name, scale, bounds)) = &target else {
                capture = None;
                layout = None;
                pixels.clear();
                continue;
            };
            if changed {
                capture = None;
                layout = None;
                pixels.clear();
                refresh = Instant::now();
                retry = Instant::now();
            }
            let layout_changed = layout.is_none() || Instant::now() >= refresh;
            if layout_changed {
                layout = Some(snapshot(name).unwrap_or_else(|_| Snapshot {
                    name: name.clone(),
                    rect: Geometry {
                        x: 0.,
                        y: 0.,
                        w: 1.,
                        h: 1.,
                    },
                    windows: vec![],
                    message: "Workspace unavailable".into(),
                    palette: Palette::default(),
                }));
                refresh =
                    Instant::now() + crate::config::get().duration("intervals.preview_layout_ms");
            }
            let shot = layout.as_mut().unwrap();
            let factor = preview_scale(shot.rect, *bounds) * scale.max(1.);
            let targets: Vec<_> = shot
                .windows
                .iter()
                .filter_map(|w| {
                    Some((
                        w.id.clone()?,
                        (w.content.w * factor).round().max(1.) as u32,
                        (w.content.h * factor).round().max(1.) as u32,
                    ))
                })
                .collect();
            pixels.retain(|id, _| targets.iter().any(|(name, _, _)| name == id));
            if capture.is_none() && !targets.is_empty() && Instant::now() >= retry {
                match Capture::new() {
                    Ok(c) => capture = Some(c),
                    Err(e) => {
                        eprintln!("Workspace capture unavailable: {e:#}");
                        retry = Instant::now()
                            + crate::config::get().duration("intervals.preview_retry_ms");
                    }
                }
            }
            let mut new_frame = false;
            if let Some(c) = &mut capture {
                match c.poll(&targets) {
                    Ok(frames) => {
                        for (id, frame) in frames {
                            pixels.insert(id, Arc::new(frame));
                            new_frame = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("Workspace capture failed: {e:#}");
                        capture = None;
                        retry = Instant::now()
                            + crate::config::get().duration("intervals.preview_error_retry_ms");
                    }
                }
            }
            if new_frame || layout_changed {
                shot.message = if !shot.windows.is_empty() && targets.is_empty() {
                    "Window layout · live capture needs Sway 1.12+".into()
                } else if !targets.is_empty() && capture.is_none() {
                    "Window layout · live capture unavailable".into()
                } else {
                    String::new()
                };
                for w in &mut shot.windows {
                    w.pixels = w.id.as_ref().and_then(|id| pixels.get(id).cloned());
                }
                // One pending snapshot bounds memory and provides backpressure.
                if sender.send(shot.clone()).is_err() {
                    break;
                }
            }
        }
    });
    tx
}
pub fn preview_bounds(screen: [f32; 2]) -> [f32; 2] {
    let width = (screen[0] * crate::config::get().number("preview.width_fraction"))
        .max(crate::config::get().number("preview.minimum_width"))
        .min((screen[0] - crate::config::get().number("preview.horizontal_margin")).max(1.));
    let height = (screen[1] - crate::config::get().number("layout.bar_height")).max(1.) * width
        / screen[0].max(1.);
    let scale = ((screen[1] - crate::config::get().number("preview.vertical_margin")).max(1.)
        / height)
        .min(1.);
    [width * scale, height * scale]
}
pub fn preview_scale(rect: Geometry, bounds: [f32; 2]) -> f32 {
    (bounds[0] / rect.w.max(1.)).min(bounds[1] / rect.h.max(1.))
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
    fn decorations_leave_capture_in_its_content_rect() {
        let tree = json!({"nodes":[{
            "id":1,"app_id":"fixture","focused":true,"name":"Title",
            "rect":{"x":100,"y":60,"width":800,"height":500},
            "window_rect":{"x":4,"y":0,"width":792,"height":496},
            "deco_rect":{"x":0,"y":0,"width":800,"height":24}
        }],"focus":[1]});
        let mut windows = vec![];
        collect(&tree, &mut windows);
        let w = &windows[0];
        assert_eq!(
            (w.content.x, w.content.y, w.content.w, w.content.h),
            (104., 60., 792., 496.)
        );
        assert_eq!(
            (
                w.decorations[0].rect.x,
                w.decorations[0].rect.y,
                w.decorations[0].rect.h
            ),
            (100., 36., 24.)
        );
        assert_eq!(w.state, 0);
    }
    #[test]
    fn tabbed_and_stacked_titles_include_hidden_siblings() {
        for layout in ["tabbed", "stacked"] {
            let leaf = |id| {
                json!({"id":id,"app_id":"fixture","name":format!("Title {id}"),
                "rect":{"x":1600,"y":if layout=="stacked" {76} else {52},"width":800,"height":500},
                "deco_rect":{"width":if layout=="tabbed" {400} else {800},"height":24}})
            };
            let tree = json!({"layout":layout,"focus":[2,1],"nodes":[leaf(1),leaf(2)]});
            let mut windows = vec![];
            collect(&tree, &mut windows);
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].title, "Title 2");
            let titles = &windows[0].decorations;
            assert_eq!(titles.len(), 2);
            assert_eq!(titles[0].rect.y, 28.);
            assert_eq!(
                (titles[1].rect.x, titles[1].rect.y),
                if layout == "tabbed" {
                    (2000., 28.)
                } else {
                    (1600., 52.)
                }
            );
            assert_eq!((titles[0].state, titles[1].state), (2, 1));
        }
    }
    #[test]
    fn sway_palette_reads_colors_and_variables() {
        let colors = Palette::read(
            "set $border #123456\nclient.focused $border #abcdef #fedcba #000000 #000000",
        );
        assert_eq!(colors.0[0][0], egui::Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(colors.0[0][1], egui::Color32::from_rgb(0xab, 0xcd, 0xef));
        assert_eq!(colors.0[2], Palette::default().0[2]);
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

#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn previews_are_bounded_independently_of_capture_size() {
        for (w, h) in [(7680., 2160.), (3840., 2160.), (1080., 1920.)] {
            for screen in [[1600., 900.], [3840., 2160.], [1080., 1920.]] {
                let bounds = preview_bounds(screen);
                assert!(
                    (bounds[1] / bounds[0]
                        - (screen[1] - crate::config::get().number("layout.bar_height"))
                            / screen[0])
                        .abs()
                        < 0.001
                );
                let factor = preview_scale(Geometry { x: 0., y: 0., w, h }, bounds);
                assert!(w * factor <= bounds[0] + 0.001 && h * factor <= bounds[1] + 0.001);
            }
        }
    }
}
