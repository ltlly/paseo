//! Scrollbar geometry and fade schedule.
//!
//! The reference client draws xterm's own scrollable element, not a platform
//! scrollbar: an eight-pixel track with a slider whose length is
//! `max(20, floor(viewport * track / content))`, positioned by the same ratio
//! arithmetic, revealed on scroll or hover and hidden 500 ms later behind an
//! 800 ms fade. These functions reproduce that arithmetic so the native slider
//! lands on the same rows as the reference. Lengths are logical pixels.
//!
//! Transcribed from `@xterm/xterm` 6.1.0-beta.292 `browser/scrollable/
//! scrollbarState.ts` and `scrollableElement.ts` (500 ms hold), and the slider
//! colours from `browser/services/ThemeService.ts` (foreground at 0.2/0.4/0.5).

use std::time::Duration;

/// The authored terminal scrollbar width (`scrollbar: { width: 8 }`).
pub const WIDTH: f32 = 8.;
/// xterm's minimum slider length so the slider stays clickable.
pub const MINIMUM_SLIDER: f32 = 20.;
/// xterm holds the revealed slider for 500 ms after the last reveal.
pub const HOLD: Duration = Duration::from_millis(500);
/// xterm fades an idle slider out over 800 ms.
pub const FADE: Duration = Duration::from_millis(800);
/// The slider is the terminal foreground at these alphas: rest, hover, drag.
pub const REST_ALPHA: u8 = 51;
pub const HOVER_ALPHA: u8 = 102;
pub const ACTIVE_ALPHA: u8 = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slider {
    pub top: f32,
    pub height: f32,
}

impl Slider {
    pub fn bottom(self) -> f32 {
        self.top + self.height
    }
}

struct Ratios {
    height: f32,
    ratio: f32,
}

fn ratios(visible: f32, content: f32) -> Option<Ratios> {
    if content <= visible || content <= 0. {
        return None;
    }
    let height = (visible * visible / content).floor().max(MINIMUM_SLIDER);
    Some(Ratios {
        height,
        ratio: (visible - height) / (content - visible),
    })
}

/// The slider for a scroll offset, or `None` when the content fits.
pub fn slider(visible: f32, content: f32, scroll_top: f32) -> Option<Slider> {
    let ratios = ratios(visible, content)?;
    Some(Slider {
        top: (scroll_top * ratios.ratio).round(),
        height: ratios.height.round(),
    })
}

/// Where a click on the track jumps to: the slider is centred on the pointer,
/// which is what the reference scrollable element does for a plain click. The
/// result is clamped to the scroll range like the scroll element does.
pub fn scroll_from_offset(offset: f32, visible: f32, content: f32) -> f32 {
    let Some(ratios) = ratios(visible, content) else {
        return 0.;
    };
    clamp(
        (offset - ratios.height / 2.) / ratios.ratio,
        content - visible,
    )
}

/// Where a drag that moved the pointer by `delta` lands.
pub fn scroll_from_drag(delta: f32, visible: f32, content: f32, scroll_top: f32) -> f32 {
    let Some(ratios) = ratios(visible, content) else {
        return 0.;
    };
    let position = (scroll_top * ratios.ratio).round();
    clamp((position + delta) / ratios.ratio, content - visible)
}

fn clamp(position: f32, maximum: f32) -> f32 {
    position.round().clamp(0., maximum)
}

/// Slider opacity for the reveal/hide schedule. `since_reveal` is the time of
/// the last reveal; hovering or dragging keeps the slider fully visible.
pub fn opacity(since_reveal: Option<Duration>, hovered: bool, dragging: bool) -> f32 {
    if hovered || dragging {
        return 1.;
    }
    let Some(elapsed) = since_reveal else {
        return 0.;
    };
    if elapsed <= HOLD {
        return 1.;
    }
    let faded = elapsed.saturating_sub(HOLD);
    if faded >= FADE {
        return 0.;
    }
    1. - faded.as_secs_f32() / FADE.as_secs_f32()
}

/// The next repaint deadline while the reveal schedule still has work to do.
pub fn next_repaint(
    since_reveal: Option<Duration>,
    hovered: bool,
    dragging: bool,
) -> Option<Duration> {
    if hovered || dragging {
        return None;
    }
    let elapsed = since_reveal?;
    if elapsed >= HOLD + FADE {
        return None;
    }
    if elapsed < HOLD {
        return Some(HOLD - elapsed);
    }
    Some(Duration::from_millis(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VISIBLE: f32 = 680.;
    const CONTENT: f32 = 17_680.;

    #[test]
    fn slider_matches_xterm_length_and_position_rounding() {
        assert_eq!(
            slider(VISIBLE, CONTENT, 0.).unwrap(),
            Slider {
                top: 0.,
                height: 26.
            }
        );
        let bottom = slider(VISIBLE, CONTENT, CONTENT - VISIBLE).unwrap();
        assert_eq!(
            bottom,
            Slider {
                top: 654.,
                height: 26.
            }
        );
        assert_eq!(bottom.bottom(), VISIBLE);
        assert_eq!(
            slider(VISIBLE, CONTENT, (CONTENT - VISIBLE) / 2.)
                .unwrap()
                .top,
            327.
        );
        assert!(slider(VISIBLE, VISIBLE, 0.).is_none());
        assert!(slider(VISIBLE, 0., 0.).is_none());
    }

    #[test]
    fn short_content_keeps_the_minimum_slider_length() {
        assert_eq!(slider(680., 34_000., 0.).unwrap().height, MINIMUM_SLIDER);
        assert_eq!(slider(680., 34_000., 33_320.).unwrap().bottom(), 680.);
    }

    #[test]
    fn click_and_drag_land_on_the_same_scroll_positions_as_the_slider() {
        assert_eq!(scroll_from_offset(0., VISIBLE, CONTENT), 0.);
        assert_eq!(scroll_from_offset(680., VISIBLE, CONTENT), 17_000.);
        assert_eq!(scroll_from_offset(13., VISIBLE, CONTENT), 0.);
        assert_eq!(scroll_from_drag(654., VISIBLE, CONTENT, 0.), 17_000.);
        assert_eq!(scroll_from_drag(-327., VISIBLE, CONTENT, 327.), 0.);
        assert_eq!(scroll_from_drag(10., VISIBLE, VISIBLE, 0.), 0.);
        let landed = scroll_from_offset(340., VISIBLE, CONTENT);
        // A click centres the slider on the pointer.
        let top = slider(VISIBLE, CONTENT, landed).unwrap().top;
        assert_eq!(top, 327.);
        assert!(top <= 340. && 340. <= top + 26.);
    }

    #[test]
    fn opacity_holds_then_fades_and_hover_keeps_it_solid() {
        assert_eq!(opacity(None, false, false), 0.);
        assert_eq!(opacity(Some(Duration::ZERO), false, false), 1.);
        assert_eq!(opacity(Some(HOLD), false, false), 1.);
        assert_eq!(
            opacity(Some(HOLD + Duration::from_millis(400)), false, false),
            0.5
        );
        assert_eq!(opacity(Some(HOLD + FADE), false, false), 0.);
        assert_eq!(opacity(Some(HOLD + FADE), true, false), 1.);
        assert_eq!(opacity(Some(HOLD + FADE), false, true), 1.);
    }

    #[test]
    fn repaint_deadlines_end_when_the_fade_ends() {
        assert_eq!(next_repaint(None, false, false), None);
        assert_eq!(
            next_repaint(Some(Duration::from_millis(100)), false, false),
            Some(Duration::from_millis(400))
        );
        assert_eq!(
            next_repaint(Some(HOLD + Duration::from_millis(100)), false, false),
            Some(Duration::from_millis(16))
        );
        assert_eq!(next_repaint(Some(HOLD + FADE), false, false), None);
        assert_eq!(next_repaint(Some(Duration::ZERO), true, false), None);
    }
}
