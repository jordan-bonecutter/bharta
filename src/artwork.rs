use anyhow::{Context, Result, bail};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Artwork {
    pub url: String,
    pub pixels: tiny_skia::Pixmap,
    #[allow(dead_code)] // Retained for consumers of the artwork backend.
    pub tint: [u8; 3],
}
const MAX_BYTES: u64 = 8 * 1024 * 1024;
pub fn worker(sender: std::sync::mpsc::Sender<crate::media::Update>) -> mpsc::Sender<Vec<String>> {
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    std::thread::spawn(move || {
        let mut cache: VecDeque<Arc<Artwork>> = VecDeque::new();
        let mut desired = Vec::new();
        let mut delivered = HashSet::new();
        let mut retries: HashMap<String, (Instant, u32)> = HashMap::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(mut urls) => {
                    while let Ok(newer) = rx.try_recv() {
                        urls = newer;
                    }
                    urls.retain(|url| !url.is_empty());
                    urls.sort();
                    urls.dedup();
                    delivered.retain(|url| urls.contains(url));
                    retries.retain(|url, _| urls.contains(url));
                    desired = urls;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            for url in &desired {
                if delivered.contains(url)
                    || retries.get(url).is_some_and(|(at, _)| Instant::now() < *at)
                {
                    continue;
                }
                let art = if let Some(art) = cache.iter().find(|art| &art.url == url).cloned() {
                    art
                } else {
                    match load(url) {
                        Ok(art) => {
                            let art = Arc::new(art);
                            cache.push_back(art.clone());
                            if cache.len() > 8 {
                                cache.pop_front();
                            }
                            art
                        }
                        Err(error) => {
                            let attempts = retries.get(url).map_or(0, |(_, attempts)| *attempts);
                            if attempts == 0 {
                                eprintln!("bharta: artwork load failed: {error}");
                            }
                            let delay = Duration::from_secs((1u64 << attempts.min(5)).min(30));
                            retries.insert(url.clone(), (Instant::now() + delay, attempts + 1));
                            continue;
                        }
                    }
                };
                if sender.send(crate::media::Update::Artwork(art)).is_err() {
                    return;
                }
                delivered.insert(url.clone());
                retries.remove(url);
            }
        }
    });
    tx
}
fn load(location: &str) -> Result<Artwork> {
    if std::path::Path::new(location).is_absolute() {
        let bytes = read_file(std::path::Path::new(location))?;
        return decode(location, &bytes);
    }
    let url = url::Url::parse(location)?;
    let reader: Box<dyn Read> = match url.scheme() {
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid file URL"))?;
            anyhow::ensure!(path.is_file(), "Artwork must be a regular file");
            Box::new(std::fs::File::open(path)?)
        }
        "https" | "http" => Box::new(
            ureq::AgentBuilder::new()
                .timeout(crate::config::get().duration("timeouts.artwork_ms"))
                .build()
                .get(location)
                .call()?
                .into_reader(),
        ),
        _ => bail!("Unsupported artwork URL"),
    };
    let mut bytes = Vec::new();
    reader.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "Artwork too large");
    decode(location, &bytes)
}
fn read_file(path: &std::path::Path) -> Result<Vec<u8>> {
    anyhow::ensure!(path.is_file(), "Artwork must be a regular file");
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "Artwork too large");
    Ok(bytes)
}
fn decode(url: &str, bytes: &[u8]) -> Result<Artwork> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()?
        .resize_to_fill(384, 384, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let mut tint = [0u64; 3];
    let mut weight = 0u64;
    let mut pixels = tiny_skia::Pixmap::new(384, 384).context("Artwork buffer")?;
    for (dst, src) in pixels.pixels_mut().iter_mut().zip(image.pixels()) {
        let [r, g, b, a] = src.0;
        for (sum, value) in tint.iter_mut().zip([r, g, b]) {
            *sum += value as u64 * a as u64;
        }
        weight += a as u64;
        *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    Ok(Artwork {
        url: url.into(),
        pixels,
        tint: tint.map(|v| (v / weight.max(1)) as u8),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_urls_decode_spaces_and_premultiply_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("album cover.png");
        image::RgbaImage::from_pixel(2, 4, image::Rgba([200, 100, 50, 128]))
            .save(&path)
            .unwrap();
        let art = load(url::Url::from_file_path(path).unwrap().as_str()).unwrap();
        assert_eq!(art.tint, [200, 100, 50]);
        assert_eq!(art.pixels.width(), 384);
        assert_eq!(art.pixels.pixels()[0].red(), 100);
    }
    #[test]
    fn worker_retries_missing_art_and_loads_other_sources() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("late cover.png");
        let ready = dir.path().join("ready.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .save(&ready)
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let requests = worker(tx);
        requests
            .send(vec![
                missing.to_string_lossy().into_owned(),
                ready.to_string_lossy().into_owned(),
            ])
            .unwrap();
        let crate::media::Update::Artwork(art) = rx.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("Expected ready artwork")
        };
        assert_eq!(art.tint, [10, 20, 30]);
        image::RgbaImage::from_pixel(2, 2, image::Rgba([80, 90, 100, 255]))
            .save(&missing)
            .unwrap();
        let crate::media::Update::Artwork(art) = rx.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("Expected retry artwork")
        };
        assert_eq!(art.tint, [80, 90, 100]);
    }
    #[test]
    fn invalid_images_and_unsupported_urls_fail_cleanly() {
        assert!(decode("test", b"not an image").is_err());
        assert!(load("ftp://example.com/cover.png").is_err());
    }
}
