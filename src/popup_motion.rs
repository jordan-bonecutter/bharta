use std::time::{Duration, Instant};
const GRACE: Duration = Duration::from_millis(500);
const FADE: Duration = Duration::from_millis(140);
#[derive(Default)]
pub struct Dismissal {
    outside: Option<Instant>,
    pinned: bool,
}
impl Dismissal {
    pub fn opened(now: Instant) -> Self {
        Self {
            outside: Some(now),
            pinned: false,
        }
    }
    pub fn set_pinned(&mut self, pinned: bool, now: Instant) {
        if self.pinned && !pinned {
            self.activity(now);
        }
        self.pinned = pinned;
    }
    pub fn pointer_inside(&mut self, inside: bool, now: Instant) {
        if inside {
            self.enter();
        } else {
            self.leave(now);
        }
    }
    pub fn enter(&mut self) {
        self.outside = None;
    }
    pub fn leave(&mut self, now: Instant) {
        self.outside.get_or_insert(now);
    }
    pub fn activity(&mut self, now: Instant) {
        if self.outside.is_some() {
            self.outside = Some(now);
        }
    }
    pub fn progress(&self, now: Instant) -> f32 {
        if self.pinned {
            return 0.0;
        }
        self.outside
            .map(|at| {
                now.saturating_duration_since(at)
                    .saturating_sub(GRACE)
                    .as_secs_f32()
                    / FADE.as_secs_f32()
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_input_survives_timeout_and_unpin_starts_fresh_grace() {
        let now = Instant::now();
        let mut d = Dismissal::opened(now);
        d.set_pinned(true, now);
        assert_eq!(d.progress(now + Duration::from_secs(60)), 0.0);
        d.set_pinned(false, now + Duration::from_secs(60));
        assert_eq!(d.progress(now + Duration::from_millis(60500)), 0.0);
        assert_eq!(d.progress(now + Duration::from_millis(60640)), 1.0);
    }
    #[test]
    fn bar_and_popup_share_one_hover_region() {
        let now = Instant::now();
        let mut d = Dismissal::opened(now);
        d.pointer_inside(true, now); // Open while pointer is on the bar.
        assert_eq!(d.progress(now + Duration::from_secs(30)), 0.0);
        d.leave(now + Duration::from_secs(30)); // Surface handoff.
        d.pointer_inside(true, now + Duration::from_secs(30));
        assert_eq!(d.progress(now + Duration::from_secs(60)), 0.0);
        d.pointer_inside(false, now + Duration::from_secs(60));
        d.pointer_inside(false, now + Duration::from_secs(61));
        assert_eq!(d.progress(now + Duration::from_millis(60640)), 1.0);
    }
    #[test]
    fn waits_half_a_second_then_fades() {
        let now = Instant::now();
        let d = Dismissal::opened(now);
        assert_eq!(d.progress(now + Duration::from_millis(499)), 0.0);
        assert!((d.progress(now + Duration::from_millis(570)) - 0.5).abs() < 0.001);
        assert_eq!(d.progress(now + Duration::from_millis(640)), 1.0);
    }
    #[test]
    fn reentry_cancels_and_keyboard_activity_restarts_grace_period() {
        let now = Instant::now();
        let mut d = Dismissal::opened(now);
        d.enter();
        assert_eq!(d.progress(now + Duration::from_secs(10)), 0.0);
        d.leave(now + Duration::from_secs(10));
        d.activity(now + Duration::from_millis(10400));
        assert_eq!(d.progress(now + Duration::from_millis(10800)), 0.0);
        assert_eq!(d.progress(now + Duration::from_millis(11040)), 1.0);
        d.enter();
        assert_eq!(d.progress(now + Duration::from_secs(20)), 0.0);
    }
}
