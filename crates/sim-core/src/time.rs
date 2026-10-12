use serde::{Deserialize, Serialize};

/// Local time-of-day window, in seconds from midnight. The end is exclusive.
/// A window that passes through midnight has `start_s > end_s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeWindow {
    pub start_s: u32,
    pub end_s: u32,
}

impl TimeWindow {
    pub const NIGHT_FLASHING: Self = Self {
        start_s: 0,
        end_s: 6 * 3600,
    };

    pub fn contains(self, seconds_from_midnight: u32) -> bool {
        let t = seconds_from_midnight % 86_400;
        if self.start_s <= self.end_s {
            t >= self.start_s && t < self.end_s
        } else {
            t >= self.start_s || t < self.end_s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_window_and_wrap() {
        assert!(TimeWindow::NIGHT_FLASHING.contains(3 * 3600));
        assert!(!TimeWindow::NIGHT_FLASHING.contains(12 * 3600));
        let wrap = TimeWindow {
            start_s: 22 * 3600,
            end_s: 6 * 3600,
        };
        assert!(wrap.contains(23 * 3600));
        assert!(wrap.contains(60 * 60));
        assert!(!wrap.contains(12 * 3600));
    }
}
