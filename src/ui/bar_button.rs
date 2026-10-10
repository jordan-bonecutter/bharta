//! Bar content declares its layout; empty labels and live values never decide it.
use egui::{Color32, Painter, Rect, TextureId, pos2, vec2};

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
    pub fn new(bounds: Rect, content: Content, slot: f32, icon_size: f32) -> Self {
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
                    pos2(icon_slot.right(), bounds.center().y - height / 2.),
                    pos2(bounds.right() - 4., bounds.center().y + height / 2.),
                );
                (plot.width() > 0.).then_some(plot)
            }
        };
        Self { icon, plot }
    }

    pub fn paint_icon(&self, painter: &Painter, texture: TextureId) {
        painter.image(
            texture,
            self.icon,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plots_reserve_the_same_icon_slot_as_neighboring_buttons() {
        let origin = pos2(100., 0.);
        let neighbor = Layout::new(
            Rect::from_min_size(origin, vec2(32., 28.)),
            Content::Icon,
            32.,
            15.,
        );
        // Wider plots must not move their icons into the plot or center them
        // across the whole button. This is the original CPU/Sound regression.
        for width in [60., 80., 120.] {
            let layout = Layout::new(
                Rect::from_min_size(origin, vec2(width, 28.)),
                Content::IconAndPlot,
                32.,
                15.,
            );
            assert_eq!(layout.icon, neighbor.icon);
            let plot = layout.plot.unwrap();
            assert_eq!(plot.center().y, neighbor.icon.center().y);
            assert!(layout.icon.right() < plot.left());
            assert_eq!(plot.right(), origin.x + width - 4.);
        }
    }

    #[test]
    fn icon_only_buttons_center_even_when_wider_than_a_slot() {
        for width in [30., 40., 60., 120.] {
            let bounds = Rect::from_min_size(pos2(37., 0.), vec2(width, 28.));
            let layout = Layout::new(bounds, Content::Icon, 32., 15.);
            assert_eq!(layout.icon.center(), bounds.center());
            assert!(layout.plot.is_none());
        }
    }

    #[test]
    fn configured_geometry_keeps_icons_and_plots_inside_the_button() {
        for height in [20., 28., 34., 48.] {
            for slot in [16., 24., 32., 48.] {
                for width in [8., slot, slot + 4., slot + 8., 120.] {
                    for icon_size in [12., 15., 24., 64.] {
                        let bounds = Rect::from_min_size(pos2(37.5, 2.5), vec2(width, height));
                        for content in [Content::Icon, Content::IconAndPlot] {
                            let layout = Layout::new(bounds, content, slot, icon_size);
                            assert!(bounds.contains_rect(layout.icon));
                            assert_eq!(layout.icon.center().y, bounds.center().y);
                            if let Some(plot) = layout.plot {
                                assert!(bounds.contains_rect(plot));
                                assert!(plot.width() > 0.);
                                assert!(layout.icon.right() <= plot.left());
                                assert_eq!(plot.center().y, bounds.center().y);
                            }
                        }
                    }
                }
            }
        }
    }
}
