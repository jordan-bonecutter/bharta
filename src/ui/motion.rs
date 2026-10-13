//! Drawer animation owns time and targets, never network or widget state.
//! Retargeting samples the in-flight size, so interruption cannot jump bounds.
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    List,
    Wifi(String),
    HiddenWifi,
    Bluetooth(String),
}
impl View {
    fn depth(&self) -> u8 {
        u8::from(*self != Self::List)
    }
}
#[derive(Default)]
pub struct DrawerMotion {
    size: Size,
    navigation: Navigation,
}
#[derive(Default)]
enum Size {
    #[default]
    Unmeasured,
    Settled(f32),
    Resizing {
        from: f32,
        to: f32,
        started: Instant,
    },
}
#[derive(Default)]
enum Navigation {
    #[default]
    Unopened,
    Settled(View),
    Entering {
        view: View,
        direction: f32,
        started: Instant,
    },
}
fn progress(now: Instant, started: Instant, duration: Duration) -> f32 {
    let t = (now.saturating_duration_since(started).as_secs_f32() / duration.as_secs_f32())
        .clamp(0., 1.);
    t * t * (3. - 2. * t)
}
impl DrawerMotion {
    pub fn settle_size(&mut self, height: f32) {
        self.size = Size::Settled(height);
    }
    pub fn height(&mut self, now: Instant, duration: Duration) -> Option<f32> {
        match self.size {
            Size::Unmeasured => None,
            Size::Settled(height) => Some(height),
            Size::Resizing { from, to, started } => {
                let linear = (now.saturating_duration_since(started).as_secs_f32()
                    / duration.as_secs_f32())
                .clamp(0., 1.);
                let t = 1. - (1. - linear).powi(3);
                if t == 1. {
                    self.size = Size::Settled(to);
                }
                Some(from + (to - from) * t)
            }
        }
    }
    pub fn measure(&mut self, height: f32, now: Instant, duration: Duration) {
        let target = match self.size {
            Size::Unmeasured => {
                self.size = Size::Settled(height);
                return;
            }
            Size::Settled(height) => height,
            Size::Resizing { to, .. } => to,
        };
        if (height - target).abs() > 0.5 {
            let from = self.height(now, duration).unwrap_or(height);
            self.size = Size::Resizing {
                from,
                to: height,
                started: now,
            };
        }
    }
    pub fn navigate(&mut self, view: View, now: Instant, duration: Duration) -> (f32, f32) {
        let current = match &self.navigation {
            Navigation::Unopened => None,
            Navigation::Settled(view) | Navigation::Entering { view, .. } => Some(view),
        };
        if current != Some(&view) {
            let direction =
                current.map_or(0., |old| if view.depth() >= old.depth() { 1. } else { -1. });
            self.navigation = if current.is_none() {
                Navigation::Settled(view)
            } else {
                Navigation::Entering {
                    view,
                    direction,
                    started: now,
                }
            };
        }
        match &self.navigation {
            Navigation::Entering {
                view,
                direction,
                started,
            } => {
                let t = progress(now, *started, duration);
                let sample = (*direction * 28. * (1. - t), t);
                if t == 1. {
                    self.navigation = Navigation::Settled(view.clone());
                }
                sample
            }
            _ => (0., 1.),
        }
    }
    pub fn animating(&self) -> bool {
        matches!(self.size, Size::Resizing { .. })
            || matches!(self.navigation, Navigation::Entering { .. })
    }
}
/// Weighted outgoing layers preserve continuity when a fade is interrupted.
pub struct Crossfade<T> {
    state: FadeState<T>,
}
enum FadeState<T> {
    Empty,
    Settled(T),
    Fading {
        from: Vec<(T, f32)>,
        target: T,
        started: Instant,
    },
}
impl<T> Default for Crossfade<T> {
    fn default() -> Self {
        Self {
            state: FadeState::Empty,
        }
    }
}
impl<T: Clone + PartialEq> Crossfade<T> {
    fn sample(&mut self, now: Instant, duration: Duration) -> Vec<(T, f32)> {
        match &self.state {
            FadeState::Empty => vec![],
            FadeState::Settled(target) => vec![(target.clone(), 1.)],
            FadeState::Fading {
                from,
                target,
                started,
            } => {
                let t = progress(now, *started, duration);
                let target = target.clone();
                let mut layers: Vec<_> = from
                    .iter()
                    .map(|(value, weight)| (value.clone(), weight * (1. - t)))
                    .collect();
                if let Some((_, weight)) = layers.iter_mut().find(|(value, _)| *value == target) {
                    *weight += t;
                } else {
                    layers.push((target.clone(), t));
                }
                layers.retain(|(_, weight)| *weight > 0.001);
                if t == 1. {
                    self.state = FadeState::Settled(target);
                }
                layers
            }
        }
    }
    pub fn update(&mut self, target: T, now: Instant, duration: Duration) -> Vec<(T, f32)> {
        let current = match &self.state {
            FadeState::Empty => None,
            FadeState::Settled(target) | FadeState::Fading { target, .. } => Some(target),
        };
        if current != Some(&target) {
            let from = self.sample(now, duration);
            self.state = FadeState::Fading {
                from,
                target,
                started: now,
            };
        }
        self.sample(now, duration)
    }
    pub fn animating(&self) -> bool {
        matches!(self.state, FadeState::Fading { .. })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_fades_retarget_without_an_opacity_jump() {
        let now = Instant::now();
        let duration = Duration::from_millis(200);
        let mut fade = Crossfade::default();
        fade.update('a', now, duration);
        assert_eq!(fade.update('a', now + duration, duration), [('a', 1.)]);
        fade.update('b', now + duration, duration);
        let midway = fade.update('b', now + duration + duration / 2, duration);
        assert_eq!(midway, [('a', 0.5), ('b', 0.5)]);
        assert_eq!(
            fade.update('c', now + duration + duration / 2, duration),
            midway
        );
        assert_eq!(fade.update('c', now + duration * 3, duration), [('c', 1.)]);
        assert!(!fade.animating());
    }
    #[test]
    fn opening_snaps_then_open_drawer_resizes() {
        let now = Instant::now();
        let duration = Duration::from_millis(200);
        let mut motion = DrawerMotion::default();
        motion.settle_size(24.);
        motion.settle_size(300.);
        assert_eq!(motion.height(now, duration), Some(300.));
        assert!(!motion.animating());
        motion.measure(100., now, duration);
        assert_eq!(motion.height(now + duration / 2, duration), Some(125.));
    }
    #[test]
    fn resizing_retargets_from_current_height_and_settles() {
        let now = Instant::now();
        let duration = Duration::from_millis(200);
        let mut motion = DrawerMotion::default();
        motion.measure(300., now, duration);
        motion.measure(100., now, duration);
        assert_eq!(motion.height(now + duration / 2, duration), Some(125.));
        motion.measure(400., now + duration / 2, duration);
        assert_eq!(motion.height(now + duration / 2, duration), Some(125.));
        assert_eq!(motion.height(now + duration * 2, duration), Some(400.));
        assert!(!motion.animating());
    }
    #[test]
    fn navigation_reverses_and_repeated_observations_do_not_restart() {
        let now = Instant::now();
        let duration = Duration::from_millis(200);
        let mut motion = DrawerMotion::default();
        assert_eq!(motion.navigate(View::List, now, duration), (0., 1.));
        assert_eq!(motion.navigate(View::HiddenWifi, now, duration), (28., 0.));
        assert_eq!(
            motion.navigate(View::HiddenWifi, now + duration / 2, duration),
            (14., 0.5)
        );
        assert_eq!(
            motion.navigate(View::List, now + duration / 2, duration),
            (-28., 0.)
        );
        assert_eq!(
            motion.navigate(View::List, now + duration * 2, duration),
            (0., 1.)
        );
        assert!(!motion.animating());
    }
}
