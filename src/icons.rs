use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
#[derive(Clone, Copy)]
#[allow(dead_code)] // Also used by the headless sample renderer.
pub enum Icon {
    Link,
    Unlink,
    Back,
    Eye(bool),
    Apps,
    Logout,
    Lock,
    Reboot,
    Power,
    Ethernet,
    Charging,
    Cpu,
    Bluetooth,
    Volume(bool),
    Music,
    Play,
    Pause,
    Previous,
    Next,
    Wifi(Option<u8>),
}
// Average a supersampled premultiplied raster at the actual display pixel size.
// Bilinear minification alone misses thin strokes when icons shrink to 14–16px.
pub fn rasterize(icon: Icon, pixels: u32, color: Color) -> Pixmap {
    const SAMPLES: u32 = 4;
    let mut large = Pixmap::new(pixels * SAMPLES, pixels * SAMPLES).unwrap();
    draw(
        &mut large,
        icon,
        0.,
        0.,
        pixels as f32,
        SAMPLES as f32,
        color,
    );
    let mut result = Pixmap::new(pixels, pixels).unwrap();
    for y in 0..pixels {
        for x in 0..pixels {
            let mut sum = [0u32; 4];
            for sy in y * SAMPLES..(y + 1) * SAMPLES {
                for sx in x * SAMPLES..(x + 1) * SAMPLES {
                    let offset = ((sy * large.width() + sx) * 4) as usize;
                    for (sum, pixel) in sum.iter_mut().zip(&large.data()[offset..offset + 4]) {
                        *sum += u32::from(*pixel);
                    }
                }
            }
            let offset = ((y * pixels + x) * 4) as usize;
            for (pixel, sum) in result.data_mut()[offset..offset + 4].iter_mut().zip(sum) {
                *pixel = ((sum + SAMPLES * SAMPLES / 2) / (SAMPLES * SAMPLES)) as u8;
            }
        }
    }
    result
}
// All symbols are vector geometry in a 24×24 grid, independent of installed fonts.
pub fn draw(pix: &mut Pixmap, icon: Icon, x: f32, y: f32, size: f32, scale: f32, color: Color) {
    let transform = Transform::from_scale(size / 24.0 * scale, size / 24.0 * scale)
        .post_translate(x * scale, y * scale);
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
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
        // Lucide link, unlink and arrow-left; see third_party/lucide.
        Icon::Link | Icon::Unlink | Icon::Back => {
            let mut p = PathBuilder::new();
            if matches!(icon, Icon::Back) {
                p.move_to(12., 19.);
                p.line_to(5., 12.);
                p.line_to(12., 5.);
                p.move_to(19., 12.);
                p.line_to(5., 12.);
            } else if matches!(icon, Icon::Link) {
                p.move_to(10., 13.);
                p.cubic_to(11.84, 15.45, 15.41, 15.71, 17.54, 13.54);
                p.line_to(20.54, 10.54);
                p.cubic_to(22.49, 8.59, 22.49, 5.42, 20.54, 3.47);
                p.cubic_to(18.59, 1.52, 15.42, 1.52, 13.47, 3.47);
                p.line_to(11.75, 5.18);
                p.move_to(14., 11.);
                p.cubic_to(12.16, 8.55, 8.59, 8.29, 6.46, 10.46);
                p.line_to(3.46, 13.46);
                p.cubic_to(1.51, 15.41, 1.51, 18.58, 3.46, 20.53);
                p.cubic_to(5.41, 22.48, 8.58, 22.48, 10.53, 20.53);
                p.line_to(12.24, 18.82);
            } else {
                p.move_to(18.84, 12.25);
                p.line_to(20.54, 10.54);
                p.cubic_to(22.49, 8.57, 22.44, 5.44, 20.42, 3.47);
                p.cubic_to(18.5, 1.55, 15.42, 1.55, 13.47, 3.47);
                p.line_to(11.75, 5.18);
                p.move_to(5.17, 11.75);
                p.line_to(3.46, 13.46);
                p.cubic_to(1.51, 15.43, 1.56, 18.56, 3.58, 20.53);
                p.cubic_to(5.5, 22.45, 8.58, 22.45, 10.53, 20.53);
                p.line_to(12.24, 18.82);
                for (x, y, end_x, end_y) in [
                    (8., 2., 8., 5.),
                    (2., 8., 5., 8.),
                    (16., 19., 16., 22.),
                    (19., 16., 22., 16.),
                ] {
                    p.move_to(x, y);
                    p.line_to(end_x, end_y);
                }
            }
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 2.,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Default::default()
                },
                transform,
                None,
            );
        }
        Icon::Eye(crossed) => {
            let mut p = PathBuilder::new();
            p.move_to(2., 12.);
            p.cubic_to(6., 4., 18., 4., 22., 12.);
            p.cubic_to(18., 20., 6., 20., 2., 12.);
            p.close();
            p.push_circle(12., 12., 3.);
            if crossed {
                p.move_to(3., 3.);
                p.line_to(21., 21.);
            }
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 1.8,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Default::default()
                },
                transform,
                None,
            );
        }
        Icon::Apps => {
            for y in [6., 13.] {
                for x in [6., 13.] {
                    rect(pix, x, y, 5., 5.);
                }
            }
        }
        Icon::Lock | Icon::Reboot | Icon::Power | Icon::Ethernet => {
            let mut p = PathBuilder::new();
            match icon {
                Icon::Ethernet => {
                    p.move_to(7., 20.);
                    p.line_to(7., 16.);
                    p.line_to(4., 16.);
                    p.line_to(4., 4.);
                    p.line_to(20., 4.);
                    p.line_to(20., 16.);
                    p.line_to(17., 16.);
                    p.line_to(17., 20.);
                    p.close();
                    for x in [8., 12., 16.] {
                        p.move_to(x, 4.);
                        p.line_to(x, 9.);
                    }
                }
                Icon::Lock => {
                    p.move_to(7., 10.);
                    p.line_to(7., 7.);
                    p.cubic_to(7., 1., 17., 1., 17., 7.);
                    p.line_to(17., 10.);
                    p.push_rect(Rect::from_xywh(5., 10., 14., 11.).unwrap());
                    p.move_to(12., 14.);
                    p.line_to(12., 17.);
                }
                Icon::Reboot => {
                    p.move_to(20., 10.);
                    p.cubic_to(18., 2., 6., 2., 4., 10.);
                    p.cubic_to(1., 21., 18., 25., 21., 15.);
                    p.move_to(20., 4.);
                    p.line_to(20., 10.);
                    p.line_to(14., 10.);
                }
                Icon::Power => {
                    p.move_to(12., 2.);
                    p.line_to(12., 12.);
                    p.move_to(6., 5.);
                    p.cubic_to(-2., 12., 4., 22., 12., 22.);
                    p.cubic_to(20., 22., 26., 12., 18., 5.);
                }
                _ => unreachable!(),
            }
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 1.8,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Default::default()
                },
                transform,
                None,
            );
        }
        Icon::Logout => {
            let mut p = PathBuilder::new();
            p.move_to(10., 3.);
            p.line_to(4., 3.);
            p.line_to(4., 21.);
            p.line_to(10., 21.);
            p.move_to(10., 12.);
            p.line_to(22., 12.);
            p.move_to(17., 7.);
            p.line_to(22., 12.);
            p.line_to(17., 17.);
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 1.8,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Default::default()
                },
                transform,
                None,
            );
        }
        Icon::Bluetooth => {
            let mut p = PathBuilder::new();
            p.move_to(6., 6.);
            p.line_to(18., 17.);
            p.line_to(12., 22.);
            p.line_to(12., 2.);
            p.line_to(18., 7.);
            p.line_to(6., 18.);
            pix.stroke_path(
                &p.finish().unwrap(),
                &paint,
                &Stroke {
                    width: 1.8,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Default::default()
                },
                transform,
                None,
            );
        }
        Icon::Cpu => {
            for (x, y, w, h) in [
                (6., 6., 12., 2.),
                (6., 16., 12., 2.),
                (6., 8., 2., 8.),
                (16., 8., 2., 8.),
                (10., 10., 4., 4.),
            ] {
                rect(pix, x, y, w, h);
            }
            for p in [8., 12., 16.] {
                rect(pix, p, 2., 1., 4.);
                rect(pix, p, 18., 1., 4.);
                rect(pix, 2., p, 4., 1.);
                rect(pix, 18., p, 4., 1.);
            }
        }
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
