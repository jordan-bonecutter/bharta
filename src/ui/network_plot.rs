//! Physical-link traffic in bits/s; exclude loopback and virtual links to avoid double counting.
use egui::{Color32, Rect, pos2};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub struct Traffic {
    previous: HashMap<String, (u64, u64)>,
    sampled: Instant,
    pub history: VecDeque<(f64, f64)>,
}
impl Default for Traffic {
    fn default() -> Self {
        Self {
            previous: counters(),
            sampled: Instant::now(),
            history: VecDeque::with_capacity(60),
        }
    }
}
fn counters() -> HashMap<String, (u64, u64)> {
    std::fs::read_to_string("/proc/net/dev")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (name, values) = line.split_once(':')?;
            let name = name.trim();
            if !std::path::Path::new("/sys/class/net")
                .join(name)
                .join("device")
                .exists()
            {
                return None;
            }
            let values: Vec<_> = values.split_whitespace().collect();
            Some((
                name.to_owned(),
                (values.first()?.parse().ok()?, values.get(8)?.parse().ok()?),
            ))
        })
        .collect()
}
fn rate(previous: u64, current: u64, seconds: f64) -> f64 {
    current.saturating_sub(previous) as f64 * 8. / seconds
}
fn level(bits: f64) -> f32 {
    ((bits.max(1_000.).log10() - 3.) / 7.).clamp(0., 1.) as f32
}
impl Traffic {
    pub fn update(&mut self, ctx: &egui::Context) {
        let elapsed = self.sampled.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let next = counters();
            let (mut tx, mut rx) = (0., 0.);
            for (name, &(received, sent)) in &next {
                if let Some(&(old_rx, old_tx)) = self.previous.get(name) {
                    tx += rate(old_tx, sent, elapsed.as_secs_f64());
                    rx += rate(old_rx, received, elapsed.as_secs_f64());
                }
            }
            self.previous = next;
            self.sampled = Instant::now();
            if self.history.len() == 60 {
                self.history.pop_front();
            }
            self.history.push_back((tx, rx));
        }
        ctx.request_repaint_after(Duration::from_secs(1).saturating_sub(self.sampled.elapsed()));
    }
    pub fn paint(&self, painter: &egui::Painter, bounds: Rect) {
        let blue = Color32::from_rgb(65, 139, 245);
        let red = Color32::from_rgb(240, 78, 97);
        let purple = Color32::from_rgb(160, 95, 211);
        let step = bounds.width() / 59.;
        for (index, &(tx, rx)) in self.history.iter().enumerate() {
            let x = bounds.right() - (self.history.len() - 1 - index) as f32 * step;
            let tx = bounds.bottom() - bounds.height() * level(tx);
            let rx = bounds.bottom() - bounds.height() * level(rx);
            for (top, bottom, color) in [
                (tx, rx.max(tx), blue),
                (rx, tx.max(rx), red),
                (tx.max(rx), bounds.bottom(), purple),
            ] {
                if bottom > top {
                    painter.rect_filled(
                        Rect::from_min_max(pos2(x - step, top), pos2(x, bottom)),
                        0.,
                        color,
                    );
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logarithmic_decades_and_limits() {
        assert_eq!(level(0.), 0.);
        assert_eq!(level(1_000.), 0.);
        assert!((level(1_000_000.) - 3. / 7.).abs() < 0.0001);
        assert_eq!(level(10_000_000_000.), 1.);
        assert_eq!(level(100_000_000_000.), 1.);
    }
    #[test]
    fn counters_reset_without_spikes() {
        assert_eq!(rate(100, 50, 1.), 0.);
        assert_eq!(rate(100, 350, 2.), 1_000.);
    }
}
