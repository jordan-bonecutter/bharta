use crate::{
    network::{Network, Snapshot},
    render::Renderer,
};
use smithay_client_toolkit::shell::xdg::popup::Popup;
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Transform};

pub const WIDTH: u32 = 380;
pub const HEIGHT: u32 = 520;
pub fn height(kind: Kind) -> u32 {
    match kind {
        Kind::Session => 260,
        Kind::Music => 468,
        _ => HEIGHT,
    }
}
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Volume,
    Network,
    Session,
    Launcher,
    Music,
}
#[derive(Clone)]
pub enum Action {
    Volume(crate::volume::Control),
    VolumeSlider(String, Option<String>),
    VolumeTab(bool),
    Scan,
    Radio,
    Choose(Network),
    Connect,
    Back,
    Disconnect(String),
    Advanced,
    Previous,
    Next,
    Launch,
    Lock,
    Logout,
    ConfirmLogout,
    App(std::path::PathBuf),
    Media(crate::media::Control),
}
pub struct Row {
    pub x: f32,
    pub w: f32,
    pub y: f32,
    pub h: f32,
    pub action: Action,
}
impl Row {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}
pub struct Panel {
    pub volume: Option<crate::volume::Snapshot>,
    pub volume_channels: bool,
    pub popup: Popup,
    pub kind: Kind,
    pub ready: bool,
    pub dismissal: crate::popup_motion::Dismissal,
    pub id: u64,
    pub scale: u32,
    pub snapshot: Snapshot,
    pub network_ready: bool,
    pub selected: Option<Network>,
    pub password: String,
    pub message: String,
    pub busy: bool,
    pub page: usize,
    pub confirm: bool,
    pub hover: Option<(f32, f32)>,
    pub track: Option<crate::media::Track>,
    pub artwork: Option<std::sync::Arc<crate::artwork::Artwork>>,
    pub rows: Vec<Row>,
    pub apps: Vec<crate::launcher::Entry>,
    pub query: String,
    pub selection: usize,
}
impl Panel {
    pub fn update_dismissal(&mut self) {
        let pinned = self.selected.is_some()
            || self.busy
            || self.confirm
            || (self.kind == Kind::Launcher && !self.query.is_empty());
        self.dismissal.set_pinned(pinned, std::time::Instant::now());
    }

    pub fn new(popup: Popup, kind: Kind, id: u64, scale: u32) -> Self {
        Self {
            volume: None,
            volume_channels: false,
            popup,
            kind,
            ready: false,
            dismissal: crate::popup_motion::Dismissal::opened(std::time::Instant::now()),
            id,
            scale,
            snapshot: Snapshot::default(),
            network_ready: false,
            selected: None,
            password: String::new(),
            message: String::new(),
            busy: false,
            page: 0,
            confirm: false,
            hover: None,
            track: None,
            artwork: None,
            rows: vec![],
            apps: vec![],
            query: String::new(),
            selection: 0,
        }
    }
    pub fn render(&mut self, renderer: &Renderer) -> Pixmap {
        if self.kind == Kind::Volume {
            return crate::volume_ui::render(self, renderer);
        }
        let scale = self.scale as f32;
        let mut pix = Pixmap::new(WIDTH * self.scale, height(self.kind) * self.scale).unwrap();
        let dark = renderer.dark;
        let bg = if dark {
            Color::from_rgba8(36, 37, 43, 250)
        } else {
            Color::from_rgba8(247, 247, 250, 250)
        };
        let fg = if dark {
            Color::from_rgba8(245, 245, 250, 255)
        } else {
            Color::from_rgba8(30, 31, 36, 255)
        };
        let muted = if dark {
            Color::from_rgba8(170, 174, 184, 255)
        } else {
            Color::from_rgba8(100, 103, 115, 255)
        };
        let accent = Color::from_rgba8(70, 145, 255, 255);
        rounded(
            &mut pix,
            [5.0, 7.0, 370.0, height(self.kind) as f32 - 11.0],
            12.0,
            Color::from_rgba8(0, 0, 0, 65),
            scale,
        );
        rounded(
            &mut pix,
            [8.0, 4.0, 364.0, height(self.kind) as f32 - 14.0],
            12.0,
            bg,
            scale,
        );
        let artwork = self
            .artwork
            .as_ref()
            .filter(|a| self.track.as_ref().is_some_and(|t| t.art_url == a.url));
        if self.kind == Kind::Music
            && let Some(art) = artwork
        {
            let [r, g, b] = art.tint;
            rounded(
                &mut pix,
                [8.0, 4.0, 364.0, height(self.kind) as f32 - 14.0],
                12.0,
                Color::from_rgba8(r, g, b, if dark { 35 } else { 22 }),
                scale,
            );
        }
        let title = if self.kind == Kind::Network {
            "Wi-Fi"
        } else if self.kind == Kind::Launcher {
            "Applications"
        } else if self.kind == Kind::Music {
            "Now playing"
        } else {
            "bharta"
        };
        renderer.text_at(&mut pix, title, 24.0, 17.0, scale, fg);
        renderer.text_at(&mut pix, "Esc to close", 270.0, 17.0, scale, muted);
        self.rows.clear();
        let mut labels: Vec<(f32, String, Option<Action>, bool)> = vec![];
        if self.kind == Kind::Music {
            if let Some(track) = &self.track {
                draw_cover(&mut pix, artwork.map(|a| a.as_ref()), scale, muted);
                labels.push((267.0, track.title.clone(), None, true));
                labels.push((299.0, track.artist.clone(), None, false));
                let controls = [
                    (
                        78.0,
                        crate::media::Control::Previous,
                        crate::icons::Icon::Previous,
                        track.can_previous,
                    ),
                    (
                        164.0,
                        crate::media::Control::Toggle,
                        if track.playing {
                            crate::icons::Icon::Pause
                        } else {
                            crate::icons::Icon::Play
                        },
                        track.can_toggle,
                    ),
                    (
                        250.0,
                        crate::media::Control::Next,
                        crate::icons::Icon::Next,
                        track.can_next,
                    ),
                ];
                for (x, control, icon, enabled) in controls {
                    let enabled = enabled && !self.busy;
                    let hover = self.hover.is_some_and(|(hx, hy)| {
                        hx >= x && hx < x + 52.0 && (352.0..400.0).contains(&hy)
                    });
                    rounded(
                        &mut pix,
                        [x, 352.0, 52.0, 48.0],
                        10.0,
                        if hover && enabled {
                            accent
                        } else if dark {
                            Color::from_rgba8(255, 255, 255, 16)
                        } else {
                            Color::from_rgba8(0, 0, 0, 12)
                        },
                        scale,
                    );
                    let mut color = fg;
                    if !enabled {
                        color.set_alpha(0.3);
                    }
                    crate::icons::draw(&mut pix, icon, x + 14.0, 364.0, 24.0, scale, color);
                    if enabled {
                        self.rows.push(Row {
                            x,
                            w: 52.0,
                            y: 352.0,
                            h: 48.0,
                            action: Action::Media(control),
                        });
                    }
                }
                labels.push((
                    417.0,
                    if track.playing { "Playing" } else { "Paused" }.into(),
                    None,
                    false,
                ));
            } else {
                labels.push((70.0, "Nothing playing".into(), None, false));
            }
        } else if self.kind == Kind::Launcher {
            rounded(
                &mut pix,
                [22.0, 58.0, 336.0, 38.0],
                7.0,
                if dark {
                    Color::from_rgba8(15, 16, 21, 255)
                } else {
                    Color::WHITE
                },
                scale,
            );
            renderer.text_at(
                &mut pix,
                &truncate(renderer, &format!("{}│", self.query), 310.0),
                32.0,
                63.0,
                scale,
                fg,
            );
            let matches: Vec<_> = self
                .apps
                .iter()
                .filter(|a| {
                    self.query
                        .split_whitespace()
                        .all(|w| a.search.contains(&w.to_lowercase()))
                })
                .collect();
            self.selection = self.selection.min(matches.len().saturating_sub(1));
            let start = (self.selection / 9) * 9;
            for (i, app) in matches.iter().enumerate().skip(start).take(9) {
                labels.push((
                    112.0 + (i - start) as f32 * 36.0,
                    format!(
                        "{}{}",
                        if i == self.selection { "› " } else { "" },
                        app.name
                    ),
                    Some(Action::App(app.path.clone())),
                    i == self.selection,
                ));
            }
            if matches.is_empty() {
                labels.push((
                    117.0,
                    if self.busy {
                        "Loading apps…"
                    } else {
                        "No matching applications"
                    }
                    .into(),
                    None,
                    false,
                ));
            }
            labels.push((
                470.0,
                "Type to search · ↑ ↓ select · Enter launch".into(),
                None,
                false,
            ));
        } else if self.kind == Kind::Session {
            if self.confirm {
                labels.push((65.0, "Log out of Sway?".into(), None, false));
                labels.push((97.0, "Your running apps will close.".into(), None, false));
                labels.push((153.0, "Cancel".into(), Some(Action::Back), false));
                labels.push((
                    195.0,
                    "Log out now".into(),
                    Some(Action::ConfirmLogout),
                    true,
                ));
            } else {
                labels.push((
                    65.0,
                    "Open app launcher".into(),
                    Some(Action::Launch),
                    false,
                ));
                labels.push((111.0, "Lock".into(), Some(Action::Lock), false));
                labels.push((157.0, "Log out…".into(), Some(Action::Logout), false));
            }
        } else if let Some(network) = &self.selected {
            labels.push((58.0, format!("Join {}", network.ssid), None, true));
            labels.push((
                91.0,
                "Password (leave blank for saved credentials)".into(),
                None,
                false,
            ));
            rounded(
                &mut pix,
                [22.0, 129.0, 336.0, 38.0],
                7.0,
                if dark {
                    Color::from_rgba8(15, 16, 21, 255)
                } else {
                    Color::WHITE
                },
                scale,
            );
            renderer.text_at(
                &mut pix,
                &format!("{}│", "•".repeat(self.password.chars().count().min(32))),
                32.0,
                134.0,
                scale,
                fg,
            );
            labels.push((
                187.0,
                if self.busy {
                    "Connecting…"
                } else {
                    "Connect"
                }
                .into(),
                Some(Action::Connect),
                true,
            ));
            labels.push((231.0, "Back to networks".into(), Some(Action::Back), false));
        } else {
            if self.network_ready {
                labels.push((
                    55.0,
                    format!(
                        "Wi-Fi is {}  ·  click to {}",
                        if self.snapshot.enabled { "on" } else { "off" },
                        if self.snapshot.enabled {
                            "turn off"
                        } else {
                            "turn on"
                        }
                    ),
                    Some(Action::Radio),
                    false,
                ));
                let current = self.snapshot.networks.iter().find(|n| n.active);
                let text = current
                    .map(|n| format!("Connected: {}", n.ssid))
                    .unwrap_or_else(|| "Not connected to Wi-Fi".into());
                labels.push((
                    93.0,
                    text,
                    current.map(|n| Action::Disconnect(n.device.clone())),
                    true,
                ));
                if current.is_some() {
                    renderer.text_at(
                        &mut pix,
                        "Click current network to disconnect",
                        26.0,
                        119.0,
                        scale,
                        muted,
                    );
                }
            } else {
                labels.push((
                    55.0,
                    if self.busy {
                        "Reading Wi-Fi status…"
                    } else {
                        "Wi-Fi status unavailable"
                    }
                    .into(),
                    None,
                    false,
                ));
            }
            let start = self.page * 6;
            for (i, n) in self
                .snapshot
                .networks
                .iter()
                .skip(start)
                .take(6)
                .enumerate()
            {
                let secure = if n.security.is_empty() || n.security == "--" {
                    "Open"
                } else {
                    &n.security
                };
                let name = truncate(renderer, &n.ssid, 185.0);
                labels.push((
                    155.0 + i as f32 * 35.0,
                    format!(
                        "{}{}  ·  {}%  {}",
                        if n.active { "✓ " } else { "" },
                        name,
                        n.signal,
                        secure
                    ),
                    Some(Action::Choose(n.clone())),
                    n.active,
                ));
            }
            if self.snapshot.networks.is_empty() {
                labels.push((
                    167.0,
                    if self.busy {
                        "Looking for networks…"
                    } else if !self.network_ready {
                        "Could not read Wi-Fi. See the message below."
                    } else if self.snapshot.enabled {
                        "No networks found. Try Scan again."
                    } else {
                        "Turn Wi-Fi on to find networks."
                    }
                    .into(),
                    None,
                    false,
                ));
            }
            if self.page > 0 {
                labels.push((
                    370.0,
                    "‹ Previous networks".into(),
                    Some(Action::Previous),
                    false,
                ));
            }
            if start + 6 < self.snapshot.networks.len() {
                labels.push((401.0, "More networks ›".into(), Some(Action::Next), false));
            }
        }
        if self.kind == Kind::Network {
            labels.push((
                439.0,
                if self.busy {
                    "Working…"
                } else {
                    "Scan again"
                }
                .into(),
                Some(Action::Scan),
                false,
            ));
            if self.network_ready
                && self.snapshot.backend == crate::network::Backend::NetworkManager
            {
                labels.push((
                    474.0,
                    "Advanced network settings…".into(),
                    Some(Action::Advanced),
                    false,
                ));
            }
        }
        for (y, text, action, highlight) in labels {
            if let Some(action) = action {
                let enabled = !self.busy || matches!(action, Action::Advanced | Action::Back);
                if enabled {
                    if self.hover.is_some_and(|(hx, hy)| {
                        (18.0..362.0).contains(&hx) && hy >= y && hy < y + 30.0
                    }) {
                        rounded(
                            &mut pix,
                            [18.0, y, 344.0, 30.0],
                            6.0,
                            if dark {
                                Color::from_rgba8(255, 255, 255, 20)
                            } else {
                                Color::from_rgba8(0, 0, 0, 15)
                            },
                            scale,
                        );
                    }
                    self.rows.push(Row {
                        x: 18.0,
                        w: 344.0,
                        y,
                        h: 30.0,
                        action,
                    });
                }
            }
            renderer.text_at(
                &mut pix,
                &truncate(renderer, &text, 325.0),
                26.0,
                y,
                scale,
                if highlight { accent } else { fg },
            );
        }
        if !self.message.is_empty() {
            // Status replaces the pagination area; never truncate a secret into a message.
            let message_y = if self.kind == Kind::Music {
                404.0
            } else {
                365.0
            };
            rounded(&mut pix, [18.0, message_y, 344.0, 60.0], 5.0, bg, scale);
            self.rows.retain(|r| !(365.0..435.0).contains(&r.y));
            for (i, line) in wrap(renderer, &self.message, 320.0)
                .iter()
                .take(3)
                .enumerate()
            {
                renderer.text_at(
                    &mut pix,
                    line,
                    26.0,
                    message_y + 1.0 + i as f32 * 20.0,
                    scale,
                    muted,
                );
            }
        }
        pix
    }
}
fn truncate(renderer: &Renderer, text: &str, width: f32) -> String {
    let mut s = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if renderer.width(&format!("{s}{c}…"), 13.0) > width {
            s.push('…');
            break;
        }
        s.push(c);
    }
    s
}
fn wrap(renderer: &Renderer, text: &str, width: f32) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let last = lines.last_mut().unwrap();
        if renderer.width(&format!("{last} {word}"), 13.0) > width && !last.is_empty() {
            lines.push(truncate(renderer, word, width));
        } else {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    lines
}
pub(crate) fn rounded(pix: &mut Pixmap, rect: [f32; 4], r: f32, color: Color, scale: f32) {
    let [x, y, w, h] = rect;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    let mut paint = Paint::default();
    paint.set_color(color);
    pix.fill_path(
        &p.finish().unwrap(),
        &paint,
        tiny_skia::FillRule::Winding,
        Transform::from_scale(scale, scale),
        None,
    );
}

fn draw_cover(pix: &mut Pixmap, art: Option<&crate::artwork::Artwork>, scale: f32, muted: Color) {
    let mut mask_pix = Pixmap::new(pix.width(), pix.height()).unwrap();
    rounded(
        &mut mask_pix,
        [94.0, 54.0, 192.0, 192.0],
        12.0,
        Color::WHITE,
        scale,
    );
    if let Some(art) = art {
        let mask = tiny_skia::Mask::from_pixmap(mask_pix.as_ref(), tiny_skia::MaskType::Alpha);
        pix.draw_pixmap(
            0,
            0,
            art.pixels.as_ref(),
            &tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::from_scale(scale * 0.5, scale * 0.5)
                .post_translate(94.0 * scale, 54.0 * scale),
            Some(&mask),
        );
    } else {
        let mut bg = muted;
        bg.set_alpha(0.1);
        rounded(pix, [94.0, 54.0, 192.0, 192.0], 12.0, bg, scale);
        crate::icons::draw(
            pix,
            crate::icons::Icon::Music,
            158.0,
            118.0,
            64.0,
            scale,
            muted,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_controls_have_separate_click_regions() {
        let prev = Row {
            x: 78.0,
            w: 52.0,
            y: 352.0,
            h: 48.0,
            action: Action::Media(crate::media::Control::Previous),
        };
        let toggle = Row {
            x: 164.0,
            w: 52.0,
            y: 352.0,
            h: 48.0,
            action: Action::Media(crate::media::Control::Toggle),
        };
        let next = Row {
            x: 250.0,
            w: 52.0,
            y: 352.0,
            h: 48.0,
            action: Action::Media(crate::media::Control::Next),
        };
        for (index, x) in [104.0, 190.0, 276.0].into_iter().enumerate() {
            for (other, row) in [&prev, &toggle, &next].into_iter().enumerate() {
                assert_eq!(row.contains(x, 376.0), index == other);
            }
        }
        assert!(!toggle.contains(190.0, 400.0));
        assert!(!toggle.contains(216.0, 376.0));
    }
}
