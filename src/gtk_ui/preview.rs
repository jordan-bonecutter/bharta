use super::*;
use crate::workspace_preview::{Geometry, Window};
fn scale(rect: Geometry) -> f64 {
    (312.0 / rect.w.max(1.0) as f64).min(220.0 / rect.h.max(1.0) as f64)
}
pub fn canvas(rect: Geometry, windows: Vec<Window>) -> gtk::DrawingArea {
    let factor = scale(rect);
    let area = gtk::DrawingArea::new();
    area.set_content_width(312);
    area.set_content_height((rect.h as f64 * factor).ceil().max(60.0) as i32);
    area.set_hexpand(false);
    area.set_halign(gtk::Align::Center);
    area.set_overflow(gtk::Overflow::Hidden);
    let tiles: Vec<_> = windows
        .into_iter()
        .map(|w| {
            let surface = w.pixels.and_then(|p| {
                let (width, height) = (p.width() as i32, p.height() as i32);
                // Cairo ARGB32 is native endian; capture pixels are premultiplied RGBA.
                let mut data = p.data().to_vec();
                for pixel in data.as_chunks_mut::<4>().0 {
                    let argb = u32::from_be_bytes([pixel[3], pixel[0], pixel[1], pixel[2]]);
                    pixel.copy_from_slice(&argb.to_ne_bytes());
                }
                gtk::cairo::ImageSurface::create_for_data(
                    data,
                    gtk::cairo::Format::ARgb32,
                    width,
                    height,
                    width * 4,
                )
                .ok()
            });
            (w.rect, w.title, surface)
        })
        .collect();
    area.set_draw_func(move |area, cr, width, height| {
        #[allow(deprecated)]
        let color = area.style_context().color();
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.05,
        );
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();
        for (window, title, surface) in &tiles {
            let x = (window.x - rect.x) as f64 * factor;
            let y = (window.y - rect.y) as f64 * factor;
            let w = window.w as f64 * factor;
            let h = window.h as f64 * factor;
            let _ = cr.save();
            cr.rectangle(x, y, w, h);
            cr.clip();
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.14,
            );
            let _ = cr.paint();
            if let Some(surface) = surface {
                let _ = cr.save();
                cr.translate(x, y);
                cr.scale(w / surface.width() as f64, h / surface.height() as f64);
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                let _ = cr.paint();
                let _ = cr.restore();
            }
            if surface.is_none() {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.72);
                cr.rectangle(x, y + (h - 17.0).max(0.0), w, 17.0);
                let _ = cr.fill();
                cr.set_source_rgb(0.95, 0.95, 0.97);
                cr.select_font_face(
                    "Sans",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Normal,
                );
                cr.set_font_size(10.0);
                cr.move_to(x + 4.0, y + h - 5.0);
                let _ = cr.show_text(title);
            }
            let _ = cr.restore();
        }
    });
    area
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_and_portrait_outputs_stay_bounded() {
        for (w, h) in [(7680.0, 2160.0), (3840.0, 2160.0), (1080.0, 1920.0)] {
            let r = Geometry {
                x: 0.0,
                y: 0.0,
                w,
                h,
            };
            let s = scale(r);
            assert!(w as f64 * s <= 312.001);
            assert!(h as f64 * s <= 220.001);
        }
    }
}
