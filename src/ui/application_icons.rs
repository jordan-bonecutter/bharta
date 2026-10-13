//! Resolve desktop metadata and image files off the render thread.
use crate::{launcher, tray};
use std::sync::mpsc;
pub type Result = (String, Option<tray::Icon>);
pub fn worker() -> (mpsc::Sender<String>, mpsc::Receiver<Result>) {
    let (requests, incoming) = mpsc::channel::<String>();
    let (results, outgoing) = mpsc::channel();
    std::thread::spawn(move || {
        for name in incoming {
            let icon = launcher::icon_name(&name)
                .and_then(|icon| tray::load_icon_at_size(&icon, "", 192))
                .or_else(|| tray::load_icon_at_size(&name, "", 192))
                .or_else(|| tray::load_icon_at_size(&name.to_lowercase(), "", 192));
            if results.send((name, icon)).is_err() {
                break;
            }
        }
    });
    (requests, outgoing)
}

/// Filter premultiplied pixels at the actual destination size, avoiding aliasing
/// and dark fringes around transparent logos when GPU minification has no mipmaps.
pub fn raster(icon: &tray::Icon, pixels: u32) -> egui::ColorImage {
    let mut rgba = icon.rgba.clone();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        for channel in 0..3 {
            pixel[channel] = ((u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255) as u8;
        }
    }
    raster_premultiplied(icon.size, &rgba, pixels)
}

pub fn raster_premultiplied(size: [usize; 2], rgba: &[u8], pixels: u32) -> egui::ColorImage {
    let source = image::RgbaImage::from_raw(size[0] as u32, size[1] as u32, rgba.to_vec())
        .expect("validated icon dimensions");
    let mut filtered = image::imageops::resize(
        &source,
        pixels,
        pixels,
        image::imageops::FilterType::Lanczos3,
    );
    for pixel in filtered.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] = pixel[channel].min(pixel[3]);
        }
    }
    egui::ColorImage::from_rgba_premultiplied([pixels as usize, pixels as usize], filtered.as_raw())
}
