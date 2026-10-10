use crate::status::Status;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use anyhow::{Context, Result};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

pub struct Renderer {
    font: FontArc,
    pub dark: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Volume,
    Workspace(String),
    Network,
    Session,
    Launcher,
    Music,
}
pub struct Hit {
    pub start: f32,
    pub end: f32,
    pub action: Action,
}
impl Renderer {
    pub fn new(path: Option<&str>, dark: bool) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/.local/share/fonts/SF-Pro-Text-Regular.otf"),
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(),
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf".into(),
            "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf".into(),
            "/usr/share/fonts/TTF/DejaVuSans.ttf".into(),
        ];
        let paths: Vec<&str> = path
            .map(|p| vec![p])
            .unwrap_or_else(|| candidates.iter().map(String::as_str).collect());
        for path in paths {
            if let Ok(data) = std::fs::read(path)
                && let Ok(font) = FontArc::try_from_vec(data)
            {
                return Ok(Self { font, dark });
            }
        }
        anyhow::bail!("No supported font found; pass --font /path/to/font.ttf")
    }
    pub(crate) fn width(&self, text: &str, size: f32) -> f32 {
        let f = self.font.as_scaled(PxScale::from(size));
        text.chars().map(|c| f.h_advance(f.glyph_id(c))).sum()
    }
    fn text(&self, pix: &mut Pixmap, text: &str, x: f32, scale: f32, color: Color) {
        self.text_at(pix, text, x, 0.0, scale, color);
    }
    pub(crate) fn text_at(
        &self,
        pix: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        scale: f32,
        color: Color,
    ) {
        let font = self.font.as_scaled(13.0 * scale);
        let mut x = x * scale;
        let baseline = y * scale + (28.0 * scale - font.height()) / 2.0 + font.ascent();
        for c in text.chars() {
            let id = font.glyph_id(c);
            let glyph = id.with_scale_and_position(font.scale(), point(x, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px >= 0 && py >= 0 && px < pix.width() as i32 && py < pix.height() as i32 {
                        let i = (py as usize * pix.width() as usize + px as usize) * 4;
                        let a = coverage * color.alpha();
                        let data = pix.data_mut();
                        for (channel, value) in [color.red(), color.green(), color.blue()]
                            .into_iter()
                            .enumerate()
                        {
                            data[i + channel] = (value * a * 255.0
                                + data[i + channel] as f32 * (1.0 - a))
                                .round() as u8;
                        }
                        data[i + 3] = (a * 255.0 + data[i + 3] as f32 * (1.0 - a)).round() as u8;
                    }
                });
            }
            x += font.h_advance(id);
        }
    }
    fn fit_text(&self, text: &str, width: f32) -> String {
        let mut result = String::new();
        for ch in text.chars().filter(|c| !c.is_control()) {
            if self.width(&format!("{result}{ch}…"), 13.0) > width {
                result.push('…');
                break;
            }
            result.push(ch);
        }
        result
    }
    pub fn draw(
        &self,
        width: u32,
        scale: u32,
        status: &Status,
        clock: &str,
        hover: Option<f32>,
    ) -> Result<(Pixmap, Vec<Hit>)> {
        let s = scale as f32;
        let mut pix = Pixmap::new(width * scale, 28 * scale).context("Invalid bar dimensions")?;
        let (bg, fg, muted) = if self.dark {
            (
                Color::from_rgba8(28, 29, 34, 235),
                Color::from_rgba8(250, 250, 252, 255),
                Color::from_rgba8(185, 187, 194, 255),
            )
        } else {
            (
                Color::from_rgba8(244, 245, 249, 235),
                Color::from_rgba8(28, 29, 33, 255),
                Color::from_rgba8(85, 88, 98, 255),
            )
        };
        pix.fill(bg);
        let rect = |pix: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color| {
            if let Some(r) = Rect::from_xywh(x * s, y * s, w * s, h * s) {
                pix.fill_rect(
                    r,
                    &Paint {
                        shader: tiny_skia::Shader::SolidColor(color),
                        ..Paint::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
        };
        rect(
            &mut pix,
            0.0,
            27.0,
            width as f32,
            1.0,
            Color::from_rgba8(100, 100, 110, 40),
        );
        crate::icons::draw(&mut pix, crate::icons::Icon::Logout, 13., 6., 16., s, fg);
        let compact_clock = clock
            .split_whitespace()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        let clock = if width < 600 {
            compact_clock.as_str()
        } else {
            clock
        };
        let shown_battery = if width < 480 { None } else { status.battery };
        let battery = shown_battery
            .map(|(n, _)| format!("{n}%"))
            .unwrap_or_default();
        let right = format!(
            "{}   {}{}",
            if status.connected {
                ""
            } else {
                "Sway offline   "
            },
            if battery.is_empty() {
                String::new()
            } else {
                format!("{battery}   ")
            },
            clock
        );
        let right_width = self.width(&right, 13.0);
        let right_x = (width as f32 - right_width - 16.0).max(42.0);
        let network_label = self.fit_text(
            status
                .extras
                .wifi_name
                .as_deref()
                .unwrap_or(match status.extras.wifi_enabled {
                    Some(false) => "Wi-Fi off",
                    Some(true) => "Not connected",
                    None => "Wi-Fi",
                }),
            if width < 600 { 85.0 } else { 160.0 },
        );
        let network_width = 28.0 + self.width(&network_label, 13.0);
        let volume_x = right_x - if shown_battery.is_some() { 34.0 } else { 0.0 } - 32.0;
        let net_x = volume_x - network_width - 8.0;
        crate::icons::draw(
            &mut pix,
            crate::icons::Icon::Volume(
                status
                    .extras
                    .volume
                    .is_some_and(|(level, muted)| muted || level == 0),
            ),
            volume_x,
            5.0,
            18.0,
            s,
            if status.extras.volume.is_some() {
                fg
            } else {
                muted
            },
        );
        if hover.is_some_and(|h| h >= net_x - 6.0 && h < net_x + network_width - 4.0) {
            rect(
                &mut pix,
                net_x - 6.0,
                4.0,
                network_width + 2.0,
                20.0,
                if self.dark {
                    Color::from_rgba8(255, 255, 255, 28)
                } else {
                    Color::from_rgba8(0, 0, 0, 18)
                },
            );
        }
        self.text(&mut pix, &network_label, net_x + 21.0, s, fg);
        if let Some((level, charging)) = shown_battery {
            let bx = right_x - 29.0;
            let color = if charging {
                Color::from_rgba8(48, 166, 94, 255)
            } else if level <= 15 {
                Color::from_rgba8(220, 65, 65, 255)
            } else {
                fg
            };
            let outline = PathBuilder::from_rect(Rect::from_xywh(bx, 9.0, 21.0, 10.0).unwrap());
            let mut paint = Paint::default();
            paint.set_color(muted);
            pix.stroke_path(
                &outline,
                &paint,
                &Stroke {
                    width: 1.0,
                    ..Stroke::default()
                },
                Transform::from_scale(s, s),
                None,
            );
            rect(&mut pix, bx + 23.0, 12.0, 2.0, 4.0, muted);
            if charging {
                crate::icons::draw(
                    &mut pix,
                    crate::icons::Icon::Charging,
                    bx + 4.5,
                    6.0,
                    16.0,
                    s,
                    color,
                );
            } else {
                rect(
                    &mut pix,
                    bx + 2.0,
                    11.0,
                    17.0 * level as f32 / 100.0,
                    6.0,
                    color,
                );
            }
        }
        crate::icons::draw(
            &mut pix,
            crate::icons::Icon::Wifi(status.extras.wifi_signal),
            net_x - 2.0,
            5.0,
            18.0,
            s,
            fg,
        );
        let mut x = 46.0;
        let mut hits = vec![Hit {
            start: 8.0,
            end: 34.0,
            action: Action::Session,
        }];
        if x + 40.0 < net_x - 12.0 {
            crate::icons::draw(&mut pix, crate::icons::Icon::Apps, x + 10., 6., 16., s, fg);
            hits.push(Hit {
                start: x - 4.0,
                end: x + 36.0,
                action: Action::Launcher,
            });
            x += 48.0;
        }
        for workspace in &status.workspaces {
            let label: String = workspace
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(20)
                .collect();
            let w = self.width(&label, 13.0) + 36.0;
            if x + w > net_x - 16.0 {
                break;
            }
            if workspace.focused || hover.is_some_and(|h| h >= x && h < x + w) {
                rect(
                    &mut pix,
                    x,
                    4.0,
                    w,
                    20.0,
                    if self.dark {
                        Color::from_rgba8(255, 255, 255, 28)
                    } else {
                        Color::from_rgba8(0, 0, 0, 18)
                    },
                );
            }
            self.text(
                &mut pix,
                &label,
                x + 10.0,
                s,
                if workspace.urgent {
                    Color::from_rgba8(222, 90, 50, 255)
                } else if workspace.focused {
                    fg
                } else {
                    muted
                },
            );
            if workspace.audible {
                let color = Color::from_rgba8(75, 170, 255, 255);
                let sx = x + w - 17.0;
                rect(&mut pix, sx, 12.0, 2.0, 4.0, color);
                rect(&mut pix, sx + 3.0, 9.0, 2.0, 10.0, color);
                rect(&mut pix, sx + 6.0, 11.0, 2.0, 6.0, color);
            }
            hits.push(Hit {
                start: x,
                end: x + w,
                action: Action::Workspace(workspace.name.clone()),
            });
            x += w + 4.0;
        }
        let mut title_right = net_x - 16.0;
        if let Some(track) = &status.extras.track {
            let available = (net_x - x - 48.0).clamp(0.0, 300.0);
            if available > 22.0 {
                let label = if track.playing && width >= 900 {
                    self.fit_text(
                        &format!(
                            "{}{}",
                            track.title,
                            if track.artist.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", track.artist)
                            }
                        ),
                        available - 22.0,
                    )
                } else {
                    String::new()
                };
                let w = if label.is_empty() {
                    18.0
                } else {
                    22.0 + self.width(&label, 13.0)
                };
                let mx = net_x - w - 24.0;
                crate::icons::draw(
                    &mut pix,
                    crate::icons::Icon::Music,
                    net_x - 42.0,
                    6.0,
                    16.0,
                    s,
                    if track.playing { fg } else { muted },
                );
                if !label.is_empty() {
                    self.text(&mut pix, &label, mx, s, fg);
                }
                hits.push(Hit {
                    start: mx - 4.0,
                    end: mx + w + 4.0,
                    action: Action::Music,
                });
                title_right = mx - 20.0;
            }
        }
        // Workspace positions never depend on the focused app's title.
        // Center the title on the output, and elide it before either side overlaps.
        let center = width as f32 / 2.0;
        let room = ((center - x - 16.0).min(title_right - center) * 2.0).clamp(0.0, 320.0);
        let mut app = String::new();
        if room > 30.0 {
            for ch in status.app.chars().filter(|c| !c.is_control()) {
                if self.width(&format!("{app}{ch}…"), 13.0) > room {
                    app.push('…');
                    break;
                }
                app.push(ch);
            }
            self.text(&mut pix, &app, center - self.width(&app, 13.0) / 2.0, s, fg);
        }
        hits.push(Hit {
            start: (net_x - 6.0).max(0.0),
            end: (net_x + network_width - 4.0).min(width as f32),
            action: Action::Network,
        });
        hits.push(Hit {
            start: volume_x - 4.0,
            end: volume_x + 24.0,
            action: Action::Volume,
        });
        self.text(&mut pix, &right, right_x, s, fg);
        Ok((pix, hits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charging_changes_the_icon_without_moving_bar_controls() {
        for dark in [true, false] {
            let renderer = Renderer::new(None, dark).unwrap();
            let mut status = Status::demo();
            for scale in [1, 2] {
                status.battery = Some((35, false));
                let (battery, before) = renderer
                    .draw(1440, scale, &status, "9:41 AM", None)
                    .unwrap();
                status.battery = Some((35, true));
                let (charging, after) = renderer
                    .draw(1440, scale, &status, "9:41 AM", None)
                    .unwrap();
                assert_ne!(battery.data(), charging.data());
                assert_eq!(
                    before
                        .iter()
                        .map(|h| (h.start, h.end, &h.action))
                        .collect::<Vec<_>>(),
                    after
                        .iter()
                        .map(|h| (h.start, h.end, &h.action))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    #[test]
    fn paused_track_hides_metadata_but_keeps_music_control() {
        let renderer = Renderer::new(None, true).unwrap();
        let mut status = Status::demo();
        let (playing, playing_hits) = renderer.draw(1440, 2, &status, "9:41 AM", None).unwrap();
        assert!(
            playing_hits
                .iter()
                .any(|h| matches!(h.action, Action::Music))
        );
        status.extras.track.as_mut().unwrap().playing = false;
        let (paused, paused_hits) = renderer.draw(1440, 2, &status, "9:41 AM", None).unwrap();
        assert!(
            paused_hits
                .iter()
                .any(|h| matches!(h.action, Action::Music))
        );
        let anchor = |hits: &[Hit]| {
            hits.iter()
                .find(|h| matches!(h.action, Action::Music))
                .unwrap()
                .end
        };
        assert_eq!(anchor(&playing_hits), anchor(&paused_hits));
        let track = status.extras.track.as_mut().unwrap();
        track.title = "A completely different paused title".into();
        track.artist = "Another artist".into();
        let (other, _) = renderer.draw(1440, 2, &status, "9:41 AM", None).unwrap();
        assert_eq!(paused.data(), other.data());
        assert_ne!(playing.data(), paused.data());
    }
    #[test]
    fn workspace_positions_do_not_depend_on_app_name() {
        let renderer = Renderer::new(None, true).unwrap();
        let mut status = Status::demo();
        let positions = |status: &Status| {
            renderer
                .draw(1440, 1, status, "9:41 AM", None)
                .unwrap()
                .1
                .into_iter()
                .filter(|h| matches!(h.action, Action::Workspace(_)))
                .map(|h| (h.start, h.end))
                .collect::<Vec<_>>()
        };
        let before = positions(&status);
        status.app = "A very long app name that used to move all the workspace buttons".into();
        assert_eq!(before, positions(&status));
        assert!(!before.is_empty());
    }
    #[test]
    fn scaled_buffers_and_click_regions_stay_valid() {
        let renderer = Renderer::new(None, false).unwrap();
        let mut status = Status::demo();
        status.app = "A very long application name ".repeat(20);
        for width in [320, 640, 1440] {
            for scale in [1, 2] {
                let (pix, hits) = renderer
                    .draw(width, scale, &status, "Tue Oct 6   9:41 AM", None)
                    .unwrap();
                assert_eq!((pix.width(), pix.height()), (width * scale, 28 * scale));
                assert_eq!(
                    hits.iter().filter(|h| h.action == Action::Network).count(),
                    1
                );
                for hit in &hits {
                    assert!(hit.start >= 0.0 && hit.end <= width as f32 && hit.start < hit.end);
                }
                assert!(hits.windows(2).all(|h| h[0].end <= h[1].start));
                assert!(
                    pix.data()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p[..3].iter().all(|c| *c <= p[3]))
                );
            }
        }
    }
}
