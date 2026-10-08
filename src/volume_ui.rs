use crate::{
    App,
    panel::{Action, Kind, Panel, Row, rounded},
    render::Renderer,
    volume,
};
use tiny_skia::{Color, Pixmap};
pub fn percent_at(x: f32) -> u8 {
    (((x - 152.) / 192.).clamp(0., 1.) * 100.).round() as u8
}
#[derive(Clone, Debug)]
pub struct Drag {
    pub output: String,
    pub channel: Option<String>,
    percent: u8,
}
impl Drag {
    pub fn new(output: String, channel: Option<String>, x: f32) -> Self {
        Self {
            output,
            channel,
            percent: percent_at(x),
        }
    }
    pub fn control(&self) -> volume::Control {
        volume::Control::Volume {
            output: self.output.clone(),
            channel: self.channel.clone(),
            percent: self.percent,
        }
    }
    pub fn motion(&mut self, x: f32) -> Option<volume::Control> {
        let percent = percent_at(x);
        if percent == self.percent {
            return None;
        }
        self.percent = percent;
        Some(self.control())
    }
}
impl App {
    pub(crate) fn queue_volume(&mut self, control: volume::Control) {
        let Some(p) = &mut self.panel else {
            return;
        };
        if let Some(snapshot) = &mut p.volume {
            volume::preview(snapshot, &control);
        }
        p.message.clear();
        if p.busy {
            // Exactly one command is in flight and one latest value is pending.
            // Fast pointer motion cannot build a backlog of stale volume changes.
            p.volume_pending = Some(control);
            return;
        }
        p.busy = true;
        if self
            .volume_requests
            .send(volume::Request::Control(p.id, control))
            .is_err()
        {
            p.busy = false;
            p.volume_drag = None;
            p.volume_pending = None;
            p.message = "Audio service unavailable".into();
        }
    }
    pub(crate) fn volume_result(&mut self, update: volume::Update) {
        match &update.result {
            Ok(snapshot) => {
                self.status.extras.volume = snapshot.active().map(|o| (o.percent(), o.muted));
                self.volume_snapshot = Some(snapshot.clone());
                self.volume_error.clear();
            }
            Err(error) => {
                self.volume_error = error.clone();
                self.status.extras.volume = None;
            }
        }
        let mut next = None;
        if let Some(p) = &mut self.panel
            && p.kind == Kind::Volume
        {
            if update.completed == Some(p.id) {
                p.busy = false;
                if update.result.is_err() {
                    p.volume_pending = None;
                    p.volume_drag = None;
                } else {
                    next = p.volume_pending.take();
                }
            }
            // Older acknowledgements and polls must not pull the thumb away
            // from the cursor or overwrite a newer value awaiting submission.
            if !p.busy && next.is_none() && p.volume_drag.is_none() {
                p.volume = self.volume_snapshot.clone();
                p.message = self.volume_error.clone();
            }
        }
        if let Some(control) = next {
            self.queue_volume(control);
        }
        self.draw_panel();
        self.draw();
    }
}
fn button(
    p: &mut Panel,
    pix: &mut Pixmap,
    renderer: &Renderer,
    rect: [f32; 4],
    label: &str,
    action: Action,
    active: bool,
) {
    let [x, y, w, h] = rect;
    let hovered = p
        .hover
        .is_some_and(|(px, py)| px >= x && px < x + w && py >= y && py < y + h);
    let bg = if active || hovered {
        Color::from_rgba8(155, 111, 65, 100)
    } else {
        Color::from_rgba8(128, 128, 128, 20)
    };
    rounded(pix, rect, 6., bg, p.scale as f32);
    renderer.text_at(
        pix,
        &label
            .chars()
            .filter(|c| !c.is_control())
            .take((w / 7.).max(1.) as usize)
            .collect::<String>(),
        x + 8.,
        y + 1.,
        p.scale as f32,
        if renderer.dark {
            Color::from_rgba8(245, 235, 220, 255)
        } else {
            Color::from_rgba8(40, 33, 26, 255)
        },
    );
    if !p.busy {
        p.rows.push(Row { x, y, w, h, action });
    }
}
fn slider(
    p: &mut Panel,
    pix: &mut Pixmap,
    renderer: &Renderer,
    y: f32,
    label: &str,
    value: u32,
    target: (&str, Option<String>),
) {
    let s = p.scale as f32;
    let fg = if renderer.dark {
        Color::from_rgba8(245, 235, 220, 255)
    } else {
        Color::from_rgba8(40, 33, 26, 255)
    };
    renderer.text_at(pix, label, 26., y, s, fg);
    rounded(
        pix,
        [152., y + 11., 192., 6.],
        3.,
        Color::from_rgba8(128, 128, 128, 85),
        s,
    );
    rounded(
        pix,
        [
            152.,
            y + 11.,
            (192. * value.min(100) as f32 / 100.).max(1.),
            6.,
        ],
        3.,
        Color::from_rgba8(214, 165, 111, 255),
        s,
    );
    rounded(
        pix,
        [148. + 192. * value.min(100) as f32 / 100., y + 7., 8., 14.],
        4.,
        fg,
        s,
    );
    if !p.busy {
        p.rows.push(Row {
            x: 148.,
            w: 200.,
            y,
            h: 30.,
            action: Action::VolumeSlider(target.0.into(), target.1),
        });
    }
}
pub fn render(p: &mut Panel, r: &Renderer) -> Pixmap {
    let s = p.scale as f32;
    let mut pix = Pixmap::new(380 * p.scale, 520 * p.scale).unwrap();
    let bg = if r.dark {
        Color::from_rgba8(36, 30, 25, 252)
    } else {
        Color::from_rgba8(249, 244, 235, 252)
    };
    let fg = if r.dark {
        Color::from_rgba8(245, 235, 220, 255)
    } else {
        Color::from_rgba8(40, 33, 26, 255)
    };
    rounded(
        &mut pix,
        [5., 7., 370., 509.],
        12.,
        Color::from_rgba8(0, 0, 0, 65),
        s,
    );
    rounded(&mut pix, [8., 4., 364., 506.], 12., bg, s);
    r.text_at(&mut pix, "Sound", 24., 17., s, fg);
    r.text_at(&mut pix, "Esc to close", 270., 17., s, fg);
    p.rows.clear();
    let snapshot = p.volume.clone().unwrap_or_default();
    if let Some(o) = snapshot.active() {
        let label = format!("{}%{}", o.percent(), if o.muted { " · Muted" } else { "" });
        r.text_at(&mut pix, &label, 26., 58., s, fg);
        button(
            p,
            &mut pix,
            r,
            [264., 57., 90., 30.],
            if o.muted { "Unmute" } else { "Mute" },
            Action::Volume(volume::Control::Mute(o.name.clone())),
            o.muted,
        );
        slider(p, &mut pix, r, 96., "Volume", o.percent(), (&o.name, None));
    } else {
        r.text_at(&mut pix, "No active audio output", 26., 76., s, fg);
    }
    button(
        p,
        &mut pix,
        r,
        [24., 143., 160., 30.],
        "Outputs",
        Action::VolumeTab(false),
        !p.volume_channels,
    );
    button(
        p,
        &mut pix,
        r,
        [194., 143., 160., 30.],
        "Channels",
        Action::VolumeTab(true),
        p.volume_channels,
    );
    if p.volume_channels {
        if let Some(o) = snapshot.active() {
            p.page = p.page.min(o.channels.len().saturating_sub(1) / 6);
            for (i, c) in o.channels.iter().skip(p.page * 6).take(6).enumerate() {
                let label = format!(
                    "{} {}%",
                    c.name.replace("front-", "").replace('-', " "),
                    c.percent()
                );
                slider(
                    p,
                    &mut pix,
                    r,
                    191. + i as f32 * 40.,
                    &label.chars().take(17).collect::<String>(),
                    c.percent(),
                    (&o.name, Some(c.name.clone())),
                );
            }
        }
    } else {
        let mut rows = vec![];
        for o in &snapshot.outputs {
            rows.push((
                format!(
                    "{}{}",
                    if o.name == snapshot.default {
                        "✓ "
                    } else {
                        ""
                    },
                    o.description
                ),
                Some(Action::Volume(volume::Control::Output(o.name.clone()))),
                o.name == snapshot.default,
            ));
            if o.name == snapshot.default {
                for port in &o.ports {
                    rows.push((
                        format!(
                            "  {}{}{}",
                            if port.name == o.active_port {
                                "› "
                            } else {
                                ""
                            },
                            port.description,
                            if port.available { "" } else { " (unavailable)" }
                        ),
                        port.available.then(|| {
                            Action::Volume(volume::Control::Port(o.name.clone(), port.name.clone()))
                        }),
                        false,
                    ));
                }
            }
        }
        p.page = p.page.min(rows.len().saturating_sub(1) / 6);
        for (i, (label, action, active)) in rows.into_iter().skip(p.page * 6).take(6).enumerate() {
            let y = 191. + i as f32 * 40.;
            if let Some(action) = action {
                button(p, &mut pix, r, [24., y, 330., 30.], &label, action, active);
            } else {
                r.text_at(
                    &mut pix,
                    &label.chars().take(42).collect::<String>(),
                    32.,
                    y,
                    s,
                    fg,
                );
            }
        }
    }
    if !p.message.is_empty() {
        r.text_at(
            &mut pix,
            &p.message.chars().take(46).collect::<String>(),
            24.,
            447.,
            s,
            fg,
        );
    }
    r.text_at(
        &mut pix,
        if p.busy {
            "Applying…"
        } else {
            "Drag levels to adjust · scroll for more"
        },
        24.,
        478.,
        s,
        fg,
    );
    pix
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drag_keeps_its_channel_and_clamps_outside_the_track() {
        let mut drag = Drag::new("speaker".into(), Some("front-left".into()), 248.);
        assert!(drag.motion(248.).is_none());
        for (x, expected) in [(-100., 0), (900., 100), (200., 25)] {
            let volume::Control::Volume {
                output,
                channel,
                percent,
            } = drag.motion(x).unwrap()
            else {
                panic!()
            };
            assert_eq!(output, "speaker");
            assert_eq!(channel.as_deref(), Some("front-left"));
            assert_eq!(percent, expected);
        }
    }
    #[test]
    fn slider_clamps_and_maps_endpoints() {
        assert_eq!(percent_at(140.), 0);
        assert_eq!(percent_at(248.), 50);
        assert_eq!(percent_at(344.), 100);
        assert_eq!(percent_at(370.), 100);
    }
}
