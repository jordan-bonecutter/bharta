use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
#[derive(Clone, Copy)]
#[allow(dead_code)] // Also used by the headless sample renderer.
pub enum Icon {
    Charging,
    Volume(bool),
    Music,
    Play,
    Pause,
    Previous,
    Next,
    Wifi(Option<u8>),
}
// All symbols are vector geometry in a 24×24 grid, independent of installed fonts.
pub fn draw(pix: &mut Pixmap, icon: Icon, x: f32, y: f32, size: f32, scale: f32, color: Color) {
    let transform = Transform::from_scale(size / 24.0 * scale, size / 24.0 * scale)
        .post_translate(x * scale, y * scale);
    let mut paint = Paint::default();
    paint.set_color(color);
    let fill = |pix: &mut Pixmap, p: PathBuilder| {
        if let Some(path) = p.finish() {
            pix.fill_path(&path, &paint, tiny_skia::FillRule::Winding, transform, None);
        }
    };
    let rect = |pix: &mut Pixmap, x, y, w, h| {
        pix.fill_rect(
            Rect::from_xywh(x, y, w, h).unwrap(),
            &paint,
            transform,
            None,
        );
    };
    let triangle = |pix: &mut Pixmap, points: [(f32, f32); 3]| {
        let mut p = PathBuilder::new();
        p.move_to(points[0].0, points[0].1);
        p.line_to(points[1].0, points[1].1);
        p.line_to(points[2].0, points[2].1);
        p.close();
        fill(pix, p);
    };
    match icon {
        Icon::Charging => {
            let mut p = PathBuilder::new();
            p.move_to(14., 1.);
            p.line_to(5., 14.);
            p.line_to(11., 14.);
            p.line_to(9., 23.);
            p.line_to(20., 9.);
            p.line_to(13., 9.);
            p.close();
            fill(pix, p);
        }

        Icon::Volume(muted) => {
            let mut p = PathBuilder::new();
            p.move_to(3., 9.);
            p.line_to(7., 9.);
            p.line_to(12., 4.);
            p.line_to(12., 20.);
            p.line_to(7., 15.);
            p.line_to(3., 15.);
            p.close();
            fill(pix, p);
            let mut p = PathBuilder::new();
            if muted {
                p.move_to(16., 8.);
                p.line_to(22., 16.);
                p.move_to(22., 8.);
                p.line_to(16., 16.);
            } else {
                p.move_to(16., 7.);
                p.quad_to(21., 12., 16., 17.);
                p.move_to(19., 3.);
                p.quad_to(27., 12., 19., 21.);
            }
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 1.8,
                    ..Default::default()
                },
                transform,
                None,
            );
        }

        Icon::Play => triangle(pix, [(7.0, 4.0), (20.0, 12.0), (7.0, 20.0)]),
        Icon::Pause => {
            rect(pix, 6.0, 4.0, 4.0, 16.0);
            rect(pix, 14.0, 4.0, 4.0, 16.0);
        }
        Icon::Previous => {
            rect(pix, 4.0, 5.0, 3.0, 14.0);
            triangle(pix, [(19.0, 4.0), (8.0, 12.0), (19.0, 20.0)]);
        }
        Icon::Next => {
            rect(pix, 17.0, 5.0, 3.0, 14.0);
            triangle(pix, [(5.0, 4.0), (16.0, 12.0), (5.0, 20.0)]);
        }
        Icon::Music => {
            let mut p = PathBuilder::new();
            p.move_to(8.0, 5.0);
            p.line_to(20.0, 2.0);
            p.line_to(20.0, 7.0);
            p.line_to(8.0, 10.0);
            p.close();
            fill(pix, p);
            rect(pix, 8.0, 6.0, 2.0, 12.0);
            rect(pix, 18.0, 4.0, 2.0, 12.0);
            let mut p = PathBuilder::new();
            p.push_circle(6.5, 18.0, 3.5);
            p.push_circle(16.5, 16.0, 3.5);
            fill(pix, p);
        }
        Icon::Wifi(strength) => {
            for (x, y, cx, cy, end, threshold) in [
                (2.0, 8.0, 12.0, -1.0, 22.0, 66),
                (5.0, 11.5, 12.0, 5.0, 19.0, 33),
                (8.0, 15.0, 12.0, 11.0, 16.0, 1),
            ] {
                let mut p = PathBuilder::new();
                p.move_to(x, y);
                p.quad_to(cx, cy, end, y);
                let mut paint = Paint::default();
                let mut c = color;
                if strength.is_none_or(|s| s < threshold) {
                    c.set_alpha(color.alpha() * 0.28);
                }
                paint.set_color(c);
                pix.stroke_path(
                    &p.finish().unwrap(),
                    &paint,
                    &Stroke {
                        width: 2.0,
                        line_cap: tiny_skia::LineCap::Round,
                        ..Stroke::default()
                    },
                    transform,
                    None,
                );
            }
            let mut p = PathBuilder::new();
            p.push_circle(12.0, 19.0, 1.6);
            fill(pix, p);
        }
    }
}
