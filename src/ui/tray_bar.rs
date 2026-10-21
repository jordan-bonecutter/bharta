//! The bar owns tray placement and decoration. Renderers only receive clipped content slots.
use egui::{Color32, Painter, Rect, Stroke, TextureId, pos2, vec2};

pub const CONTENT_MARGIN: f32 = 4.;

#[derive(Clone, Copy)]
pub enum Content {
    Icon,
    IconAndPlot,
}

pub struct Layout {
    icon: Rect,
    plot: Option<Rect>,
}

impl Layout {
    fn from_content(
        bounds: Rect,
        content: Content,
        slot: f32,
        icon_size: f32,
        gutter: f32,
    ) -> Self {
        let icon_slot = match content {
            Content::Icon => bounds,
            Content::IconAndPlot => Rect::from_min_max(
                bounds.min,
                pos2(bounds.left() + slot.min(bounds.width()), bounds.bottom()),
            ),
        };
        let size = icon_size.min(icon_slot.width()).min(icon_slot.height());
        let icon = Rect::from_center_size(icon_slot.center(), vec2(size, size));
        let plot = match content {
            Content::Icon => None,
            Content::IconAndPlot => {
                let height = 14_f32.min(bounds.height());
                let plot = Rect::from_min_max(
                    pos2(icon_slot.right() + gutter, bounds.center().y - height / 2.),
                    pos2(bounds.right(), bounds.center().y + height / 2.),
                );
                (plot.width() > 0.).then_some(plot)
            }
        };
        Self { icon, plot }
    }

    #[cfg(test)]
    pub fn icon_bounds(&self) -> Rect {
        self.icon
    }

    pub fn paint_icon(&self, painter: &Painter, texture: TextureId) {
        // Odd-sized icons centered in even-sized slots land on half pixels.
        // Align the raster to physical pixels to avoid filtering it a second time.
        let scale = painter.ctx().pixels_per_point();
        let min = pos2(
            (self.icon.min.x * scale).floor() / scale,
            (self.icon.min.y * scale).floor() / scale,
        );
        let size = vec2(
            (self.icon.width() * scale).round() / scale,
            (self.icon.height() * scale).round() / scale,
        );
        painter.image(
            texture,
            Rect::from_min_size(min, size),
            Rect::from_min_max(egui::Pos2::ZERO, pos2(1., 1.)),
            Color32::WHITE,
        );
    }

    pub fn plot(&self, painter: &Painter) -> Option<(Painter, Rect)> {
        // Consumers receive a painter confined to the reserved plot area, so
        // strokes and markers cannot bleed into icons or neighboring buttons.
        self.plot
            .map(|bounds| (painter.with_clip_rect(bounds), bounds))
    }
}

pub struct Tray<T> {
    pub id: T,
    pub bounds: Rect,
    pub content: Rect,
    pub layout: Layout,
}
impl<T> Tray<T> {
    pub fn paint_background(&self, painter: &Painter, color: Color32, pressed: bool) {
        painter.rect_filled(
            self.bounds.shrink2(vec2(0., 2.)),
            3,
            if pressed {
                color.gamma_multiply(0.9)
            } else {
                color
            },
        );
    }
    pub fn painter(&self, painter: &Painter) -> Painter {
        painter.with_clip_rect(self.content)
    }
}

pub struct Bar<T> {
    items: Vec<(T, f32, Content)>,
    gap: f32,
    height: f32,
    slot: f32,
    icon_size: f32,
}
impl<T> Bar<T> {
    pub fn new(gap: f32, height: f32, slot: f32, icon_size: f32) -> Self {
        Self {
            items: Vec::new(),
            gap,
            height,
            slot,
            icon_size,
        }
    }
    pub fn register(&mut self, id: T, width: f32, content: Content) {
        self.items.push((id, width, content));
    }
    pub fn register_intrinsic(&mut self, id: T, width: f32) {
        self.register(id, width + 2. * CONTENT_MARGIN, Content::Icon);
    }
    pub fn prepend_fitting(
        &mut self,
        candidates: impl IntoIterator<Item = (T, f32, Content)>,
        available_width: f32,
    ) {
        let mut used = self.width();
        let mut prefix = Vec::new();
        for item in candidates {
            let extra = item.1
                + if self.items.is_empty() && prefix.is_empty() {
                    0.
                } else {
                    self.gap
                };
            if used + extra > available_width {
                break;
            }
            used += extra;
            prefix.push(item);
        }
        prefix.append(&mut self.items);
        self.items = prefix;
    }
    pub fn width(&self) -> f32 {
        self.items.iter().map(|(_, width, _)| width).sum::<f32>()
            + self.gap * self.items.len().saturating_sub(1) as f32
    }
    pub fn right_aligned(self, right: f32) -> Vec<Tray<T>> {
        let mut x = right - self.width();
        self.items
            .into_iter()
            .map(|(id, width, kind)| {
                let bounds = Rect::from_min_size(pos2(x, 0.), vec2(width, self.height));
                x += width + self.gap;
                let margin = CONTENT_MARGIN.min(width / 2.).min(self.height / 2.);
                let content = bounds.shrink2(vec2(margin, margin));
                Tray {
                    id,
                    bounds,
                    content,
                    layout: Layout::from_content(
                        content,
                        kind,
                        (self.slot - 2. * margin).max(0.),
                        self.icon_size,
                        margin,
                    ),
                }
            })
            .collect()
    }
}

pub fn separators<T>(painter: &Painter, trays: &[Tray<T>], height: f32, color: Color32) {
    for pair in trays.windows(2) {
        let x = (pair[0].bounds.right() + pair[1].bounds.left()) / 2.;
        let y = pair[0].bounds.center().y;
        painter.line_segment(
            [pos2(x, y - height / 2.), pos2(x, y + height / 2.)],
            Stroke::new(1_f32, color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_trays_share_margins_and_only_gaps_contain_separators() {
        for scale in [1., 1.5, 2.] {
            for height in [20., 28., 34.] {
                let mut bar = Bar::new(8., height, 32., 15.);
                for (id, width, kind) in [
                    (0, 32., Content::Icon),
                    (1, 80., Content::IconAndPlot),
                    (2, 60., Content::IconAndPlot),
                    (3, 32., Content::Icon),
                ] {
                    bar.register(id, width, kind);
                }
                let trays = bar.right_aligned(1000. * scale);
                for tray in &trays {
                    assert_eq!(tray.content.left() - tray.bounds.left(), CONTENT_MARGIN);
                    assert_eq!(tray.bounds.right() - tray.content.right(), CONTENT_MARGIN);
                    assert!(tray.content.contains_rect(tray.layout.icon_bounds()));
                    if let Some((_, plot)) =
                        tray.layout.plot(&egui::Context::default().debug_painter())
                    {
                        assert!(tray.content.contains_rect(plot));
                        assert!(plot.left() >= tray.layout.icon_bounds().right());
                    }
                }
                for pair in trays.windows(2) {
                    assert_eq!(pair[1].bounds.left() - pair[0].bounds.right(), 8.);
                    let separator = (pair[0].bounds.right() + pair[1].bounds.left()) / 2.;
                    assert!(!pair[0].bounds.contains(pos2(separator, height / 2.)));
                    assert!(!pair[1].bounds.contains(pos2(separator, height / 2.)));
                }
            }
        }
    }
}
