//! Easing functions for time-based animations. `t` is normalized progress in
//! `[0,1]`; the return value is the eased progress, also in `[0,1]`.

/// Easing curve, selectable from config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Easing {
    Linear,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Parse a config string; unknown values fall back to `EaseOut`.
    pub fn from_name(s: &str) -> Easing {
        match s {
            "linear" => Easing::Linear,
            "ease_in_out" => Easing::EaseInOut,
            _ => Easing::EaseOut,
        }
    }
}

/// Apply `easing` to normalized progress `t` (clamped to `[0,1]`).
pub fn ease(easing: Easing, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match easing {
        Easing::Linear => t,
        Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        Easing::EaseInOut => t * t * (3.0 - 2.0 * t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_are_fixed_and_clamped() {
        for e in [Easing::Linear, Easing::EaseOut, Easing::EaseInOut] {
            assert_eq!(ease(e, 0.0), 0.0, "{e:?} at 0");
            assert_eq!(ease(e, 1.0), 1.0, "{e:?} at 1");
            assert_eq!(ease(e, -5.0), 0.0, "{e:?} clamps below 0");
            assert_eq!(ease(e, 5.0), 1.0, "{e:?} clamps above 1");
        }
    }

    #[test]
    fn ease_out_is_ahead_of_linear_in_the_middle() {
        assert!(ease(Easing::EaseOut, 0.5) > 0.5);
    }

    #[test]
    fn from_name_parses_known_and_falls_back() {
        assert_eq!(Easing::from_name("linear"), Easing::Linear);
        assert_eq!(Easing::from_name("ease_in_out"), Easing::EaseInOut);
        assert_eq!(Easing::from_name("ease_out"), Easing::EaseOut);
        assert_eq!(Easing::from_name("bogus"), Easing::EaseOut);
    }
}
