use egui::{Color32, Mesh, Painter, Pos2, Rect, pos2};
use std::time::Duration;
use std::{collections::VecDeque, time::Instant};

fn usage_color(level: f32) -> Color32 {
    let level = level.clamp(0., 1.);
    let warmth = ((level - 0.5) * 2.).max(0.);
    let red = egui::ecolor::Hsva::from(Color32::from_rgb(255, 91, 96));
    Color32::from(egui::ecolor::Hsva::new(
        ((1. - level) / 3. + warmth * (red.h - 1.)).rem_euclid(1.),
        0.95 + warmth * (red.s - 0.95),
        0.90 + warmth * (red.v - 0.90),
        1.,
    ))
}
pub fn paint(
    painter: &Painter,
    chart: Rect,
    history: &VecDeque<f32>,
    sampled_at: Instant,
    sample_interval: Duration,
    frame_interval: Duration,
) {
    // Reveal the newest segment over one sampling interval, retaining
    // an extra point so the left edge stays continuous as history rolls.
    let phase = (sampled_at.elapsed().as_secs_f32() / sample_interval.as_secs_f32()).clamp(0., 1.);
    if phase < 1. && history.len() > 1 {
        painter.ctx().request_repaint_after(
            repaint_interval(
                chart.width(),
                painter.ctx().pixels_per_point(),
                sample_interval,
            )
            .max(frame_interval),
        );
    }
    let color = painter.ctx().global_style().visuals.text_color();
    painter.line_segment(
        [chart.left_bottom(), chart.right_bottom()],
        egui::Stroke::new(1_f32, color.gamma_multiply(0.2)),
    );
    let points: Vec<_> = history
        .iter()
        .enumerate()
        .map(|(i, percent)| {
            let usage = percent.clamp(0., 100.) / 100.;
            let point = pos2(
                chart.right() - ((history.len() - 1 - i) as f32 - 1. + phase) * chart.width() / 31.,
                chart.bottom() - usage * chart.height(),
            );
            (point, usage)
        })
        .collect();
    // Sample the hue ramp at physical-pixel intervals so GPU RGB
    // interpolation cannot desaturate a long green-to-red segment.
    let mut colored_points = Vec::new();
    for pair in points.windows(2) {
        let steps = ((pair[1].0.x - pair[0].0.x).abs() * painter.ctx().pixels_per_point())
            .ceil()
            .max(1.) as usize;
        for step in 0..steps {
            let t = step as f32 / steps as f32;
            colored_points.push((
                pair[0].0 + (pair[1].0 - pair[0].0) * t,
                usage_color(pair[0].1 + (pair[1].1 - pair[0].1) * t),
            ));
        }
    }
    if let Some((point, usage)) = points.last() {
        colored_points.push((*point, usage_color(*usage)));
    }
    let points = colored_points;
    if points.len() > 1 {
        painter.add(egui::Shape::mesh(fill(
            &points,
            chart.bottom(),
            painter.ctx().pixels_per_point(),
        )));
        for pair in points.windows(2) {
            painter.line_segment([pair[0].0, pair[1].0], egui::Stroke::new(1_f32, pair[1].1));
        }
    } else if let Some((point, color)) = points.first() {
        painter.line_segment(
            [*point, pos2(point.x, chart.bottom())],
            egui::Stroke::new(1_f32, color.gamma_multiply(0.55)),
        );
        painter.circle_filled(*point, 1., *color);
    }
}

pub fn repaint_interval(width: f32, pixels_per_point: f32, sample: Duration) -> Duration {
    // History moves one thirty-first of the chart per sample. Updating at
    // eighth-pixel steps preserves subpixel scrolling without rendering the
    // entire bar at 60 Hz for a chart moving less than one pixel per second.
    let seconds = sample.as_secs_f64() * 31. / (width * pixels_per_point) as f64 / 8.;
    Duration::from_secs_f64(seconds.clamp(0.016, 0.250))
}

pub fn fill(points: &[(Pos2, Color32)], bottom: f32, pixels_per_point: f32) -> Mesh {
    let mut mesh = Mesh::default();
    let half_pixel = 0.5 / pixels_per_point;
    for (point, color) in points {
        let color = color.gamma_multiply(0.55);
        // Raw mesh triangles have no automatic edge antialiasing. Fade across
        // one physical pixel so the fill follows fractional scrolling, like
        // egui's antialiased line above it.
        mesh.colored_vertex(pos2(point.x, point.y - half_pixel), Color32::TRANSPARENT);
        mesh.colored_vertex(pos2(point.x, (point.y + half_pixel).min(bottom)), color);
        mesh.colored_vertex(pos2(point.x, bottom), color);
    }
    for i in 0..points.len().saturating_sub(1) {
        let index = i as u32 * 3;
        for row in 0..2 {
            mesh.add_triangle(index + row, index + row + 1, index + row + 3);
            mesh.add_triangle(index + row + 1, index + row + 4, index + row + 3);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{ClippedPrimitive, Rect, epaint::Primitive};
    use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};

    #[test]
    fn scrolling_cadence_tracks_physical_motion() {
        let sample = Duration::from_secs(1);
        for width in [24., 48., 120.] {
            for scale in [1., 2.] {
                let interval = repaint_interval(width, scale, sample);
                let displacement = width * scale / 31. * interval.as_secs_f32();
                assert!((displacement - 0.125).abs() < 0.0001);
            }
        }
        assert_eq!(
            repaint_interval(0.1, 1., sample),
            Duration::from_millis(250)
        );
        assert_eq!(
            repaint_interval(4096., 2., sample),
            Duration::from_millis(16)
        );
    }

    #[test]
    fn fill_moves_between_pixels_at_native_and_hidpi_scale() {
        for scale in [1., 2.] {
            let render = |offset: f32| {
                let points = [
                    (pos2(-2. + offset, 3.), Color32::WHITE),
                    (pos2(18. + offset, 13.), Color32::WHITE),
                ];
                let jobs = [ClippedPrimitive {
                    clip_rect: Rect::from_min_max(pos2(0., 0.), pos2(16., 16.)),
                    primitive: Primitive::Mesh(fill(&points, 16., scale)),
                }];
                let size = (16. * scale) as usize;
                let mut pixels = vec![[0; 4]; size * size];
                let mut renderer = EguiSoftwareRender::new(ColorFieldOrder::Rgba);
                let textures = egui::TexturesDelta {
                    set: vec![(
                        egui::TextureId::default(),
                        egui::epaint::ImageDelta::full(
                            egui::ColorImage::filled([1, 1], Color32::WHITE),
                            egui::TextureOptions::LINEAR,
                        ),
                    )],
                    ..Default::default()
                };
                renderer.render(
                    &mut BufferMutRef::new(&mut pixels, size, size),
                    &jobs,
                    &textures,
                    scale,
                );
                pixels
            };
            let frames: Vec<_> = (0..5).map(|i| render(i as f32 * 0.2 / scale)).collect();
            for pair in frames.windows(2) {
                assert!(
                    pair[0] != pair[1],
                    "Fill stalled during subpixel scrolling at {scale}x"
                );
            }
        }
    }
}
