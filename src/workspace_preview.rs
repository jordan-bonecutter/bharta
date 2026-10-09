use crate::{capture::Capture, status};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
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
}
#[derive(Clone)]
pub struct Snapshot {
    pub name: String,
    pub rect: Geometry,
    pub windows: Vec<Window>,
    pub message: String,
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
pub fn watch(sender: mpsc::SyncSender<Snapshot>) -> mpsc::Sender<Option<(String, f32)>> {
    let (tx, rx) = mpsc::channel::<Option<(String, f32)>>();
    std::thread::spawn(move || {
        let mut capture = None;
        let mut target = None;
        let mut layout: Option<Snapshot> = None;
        let mut pixels = std::collections::HashMap::new();
        let mut refresh = Instant::now();
        let mut retry = Instant::now();
        loop {
            let request = if target.is_some() {
                rx.recv_timeout(Duration::from_millis(4))
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
            let Some((name, scale)) = &target else {
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
                }));
                refresh = Instant::now() + Duration::from_millis(500);
            }
            let shot = layout.as_mut().unwrap();
            let factor = preview_scale(shot.rect) * scale.max(1.);
            let targets: Vec<_> = shot
                .windows
                .iter()
                .filter_map(|w| {
                    Some((
                        w.id.clone()?,
                        (w.rect.w * factor).round().max(1.) as u32,
                        (w.rect.h * factor).round().max(1.) as u32,
                    ))
                })
                .collect();
            pixels.retain(|id, _| targets.iter().any(|(name, _, _)| name == id));
            if capture.is_none() && !targets.is_empty() && Instant::now() >= retry {
                match Capture::new() {
                    Ok(c) => capture = Some(c),
                    Err(e) => {
                        eprintln!("Workspace capture unavailable: {e:#}");
                        retry = Instant::now() + Duration::from_secs(5);
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
                        retry = Instant::now() + Duration::from_secs(1);
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
pub fn preview_scale(rect: Geometry) -> f32 {
    (312. / rect.w.max(1.)).min(220. / rect.h.max(1.))
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

#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn previews_are_bounded_independently_of_capture_size() {
        for (w, h) in [(7680., 2160.), (3840., 2160.), (1080., 1920.)] {
            let factor = preview_scale(Geometry { x: 0., y: 0., w, h });
            assert!(w * factor <= 312.001 && h * factor <= 220.001);
        }
    }
}
