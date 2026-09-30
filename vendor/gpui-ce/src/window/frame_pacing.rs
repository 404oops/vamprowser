use crate::{RequestFrameOptions, ThermalState};
use std::time::Duration;

pub(super) fn frame_pacing_interval(
    options: RequestFrameOptions,
    has_frame_callbacks: bool,
    active: bool,
    popup: bool,
    thermal_state: Option<ThermalState>,
    high_rate_input: impl FnOnce() -> bool,
) -> Option<Duration> {
    if options.require_presentation || (!options.force_render && !has_frame_callbacks) {
        None
    } else if !active && !popup && !high_rate_input() {
        Some(Duration::from_micros(33333))
    } else if let Some(ThermalState::Critical | ThermalState::Serious) = thermal_state {
        Some(Duration::from_micros(16667))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_motion_matches_foreground_windows() {
        for active in [false, true] {
            assert_eq!(
                frame_pacing_interval(
                    RequestFrameOptions::default(),
                    true,
                    active,
                    true,
                    Some(ThermalState::Nominal),
                    || panic!("popup pacing does not need fabricated input"),
                ),
                None,
            );
        }
    }

    #[test]
    fn ordinary_background_motion_keeps_its_cap() {
        let options = RequestFrameOptions::default();
        assert_eq!(
            frame_pacing_interval(options, true, false, false, None, || false),
            Some(Duration::from_micros(33333)),
        );
        assert_eq!(
            frame_pacing_interval(options, true, false, false, None, || true),
            None,
        );
    }

    #[test]
    fn popup_motion_retains_the_thermal_cap() {
        for thermal in [ThermalState::Serious, ThermalState::Critical] {
            assert_eq!(
                frame_pacing_interval(
                    RequestFrameOptions::default(),
                    true,
                    false,
                    true,
                    Some(thermal),
                    || false,
                ),
                Some(Duration::from_micros(16667)),
            );
        }
    }

    #[test]
    fn idle_and_required_presentation_remain_unthrottled() {
        for popup in [false, true] {
            for (options, callbacks) in [
                (RequestFrameOptions::default(), false),
                (
                    RequestFrameOptions {
                        require_presentation: true,
                        force_render: false,
                    },
                    true,
                ),
            ] {
                assert_eq!(
                    frame_pacing_interval(
                        options,
                        callbacks,
                        false,
                        popup,
                        Some(ThermalState::Critical),
                        || panic!("idle frames do not inspect input history"),
                    ),
                    None,
                );
            }
        }
    }
}
