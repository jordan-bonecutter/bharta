use egui::{Color32, Mesh, Pos2, pos2};
use std::time::Duration;

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
