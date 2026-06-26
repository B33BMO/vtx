//! Active animations, advanced by the server frame clock.

use std::time::{Duration, Instant};
use vtx_core::animation::{ease, Easing};
use vtx_core::PaneId;
use vtx_layout::Rect;

/// What an animation drives.
#[derive(Debug, Clone)]
pub enum AnimationKind {
    /// A pane shrinking to nothing before removal: interpolate its rect to `to`.
    PaneClose { pane: PaneId, from: Rect, to: Rect },
}

/// A single in-flight animation.
#[derive(Debug, Clone)]
pub struct Animation {
    pub kind: AnimationKind,
    start: Instant,
    duration: Duration,
    easing: Easing,
}

/// All active animations for the server.
#[derive(Default)]
pub struct AnimationRegistry {
    items: Vec<Animation>,
}

impl AnimationRegistry {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn push(&mut self, kind: AnimationKind, now: Instant, duration: Duration, easing: Easing) {
        self.items.push(Animation { kind, start: now, duration, easing });
    }

    /// Drop animations whose duration has elapsed as of `now`. Returns the
    /// kinds of the animations that just completed (so callers can apply the
    /// deferred end-state, e.g. actually remove a closed pane).
    pub fn prune(&mut self, now: Instant) -> Vec<AnimationKind> {
        let mut done = Vec::new();
        self.items.retain(|a| {
            if now.duration_since(a.start) >= a.duration {
                done.push(a.kind.clone());
                false
            } else {
                true
            }
        });
        done
    }

    /// Current interpolated rect for `pane`, if a PaneClose animation targets it.
    pub fn pane_rect(&self, pane: PaneId, now: Instant) -> Option<Rect> {
        self.items.iter().find_map(|a| match &a.kind {
            AnimationKind::PaneClose { pane: p, from, to } if *p == pane => {
                let raw = (now.duration_since(a.start).as_secs_f32()
                    / a.duration.as_secs_f32())
                .clamp(0.0, 1.0);
                Some(Rect::lerp(*from, *to, ease(a.easing, raw)))
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(cols: u16, rows: u16) -> Rect {
        Rect { x: 0, y: 0, cols, rows }
    }

    #[test]
    fn pane_rect_interpolates_then_prunes_on_completion() {
        let mut reg = AnimationRegistry::default();
        let t0 = Instant::now();
        reg.push(
            AnimationKind::PaneClose { pane: PaneId(1), from: rect(100, 40), to: rect(0, 0) },
            t0,
            Duration::from_millis(100),
            Easing::Linear,
        );

        let mid = reg.pane_rect(PaneId(1), t0 + Duration::from_millis(50)).unwrap();
        assert!(mid.cols > 0 && mid.cols < 100, "got {}", mid.cols);
        assert!(!reg.is_empty());

        let done = reg.prune(t0 + Duration::from_millis(101));
        assert_eq!(done.len(), 1);
        assert!(reg.is_empty());
        assert!(reg.pane_rect(PaneId(1), t0 + Duration::from_millis(101)).is_none());
    }
}
