use crate::{capture::Capture, status};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    sync::mpsc,
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
pub struct Window {
    id: Option<String>,
    pub title: String,
    pub rect: Geometry,
    pub pixels: Option<Pixmap>,
}
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
pub fn watch(sender: mpsc::Sender<Snapshot>) -> mpsc::Sender<Option<String>> {
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
