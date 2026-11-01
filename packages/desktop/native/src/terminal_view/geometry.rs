use alacritty_terminal::vte::ansi::CursorShape;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub width: f32,
    pub height: f32,
}

impl Metrics {
    /// Match xterm's WebGL grid quantization at the device scale. Browser font
    /// bounding boxes have integer CSS ascent/descent before device rounding.
    pub fn new(advance: f32, ascent: f32, descent: f32, scale: f32) -> Self {
        Self {
            width: (advance * scale).floor().max(1.) / scale,
            height: ((ascent.round() + descent.abs().round()) * scale)
                .ceil()
                .max(1.)
                / scale,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub fn cursor_rects(
    metrics: Metrics,
    columns: usize,
    shape: CursorShape,
) -> [Option<CursorRect>; 4] {
    let width = metrics.width * columns as f32;
    let full = CursorRect {
        x: 0.,
        y: 0.,
        width,
        height: metrics.height,
    };
    let left = CursorRect { width: 1., ..full };
    let bottom = CursorRect {
        y: metrics.height - 1.,
        height: 1.,
        ..full
    };
    match shape {
        CursorShape::Block => [Some(full), None, None, None],
        CursorShape::Beam => [Some(left), None, None, None],
        CursorShape::Underline => [Some(bottom), None, None, None],
        CursorShape::HollowBlock => [
            Some(left),
            Some(bottom),
            Some(CursorRect { height: 1., ..full }),
            Some(CursorRect {
                x: width - 1.,
                ..left
            }),
        ],
        CursorShape::Hidden => [None; 4],
    }
}

pub fn selection_scroll(pointer_y: f32, top: f32, bottom: f32) -> i32 {
    let distance = if pointer_y < top {
        pointer_y - top
    } else if pointer_y >= bottom {
        pointer_y - bottom
    } else {
        return 0;
    };
    if distance == 0. {
        return 0;
    }
    let ratio = distance.clamp(-50., 50.) / 50.;
    // JS Math.round breaks negative ties toward positive infinity.
    -(ratio.signum() as i32 + (ratio * 14. + 0.5).floor() as i32)
}

const BLINK_INTERVAL: Duration = Duration::from_millis(600);
const BLINK_IDLE: Duration = Duration::from_secs(300);

pub struct CursorBlink {
    enabled: bool,
    visible: bool,
    phase_deadline: Instant,
    idle_deadline: Instant,
    idle: bool,
}

impl CursorBlink {
    pub fn new(now: Instant) -> Self {
        Self {
            enabled: false,
            visible: true,
            phase_deadline: now + BLINK_INTERVAL,
            idle_deadline: now + BLINK_IDLE,
            idle: false,
        }
    }

    pub fn enable(&mut self, enabled: bool, now: Instant) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.visible = true;
            self.idle = false;
            self.phase_deadline = now + BLINK_INTERVAL;
            self.idle_deadline = now + BLINK_IDLE;
        }
    }

    pub fn restart(&mut self, now: Instant) {
        self.visible = true;
        self.phase_deadline = now + BLINK_INTERVAL;
        if self.idle {
            self.idle = false;
            self.idle_deadline = now + BLINK_IDLE;
        }
    }

    pub fn tick(&mut self, now: Instant) {
        if !self.enabled || self.idle {
            return;
        }
        if now >= self.idle_deadline {
            self.idle = true;
            self.visible = true;
        } else if now >= self.phase_deadline {
            self.visible = !self.visible;
            self.phase_deadline = now + BLINK_INTERVAL;
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn next(&self, now: Instant) -> Option<Duration> {
        (self.enabled && !self.idle).then(|| {
            self.phase_deadline
                .min(self.idle_deadline)
                .saturating_duration_since(now)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_autoscroll_matches_xterm_distance_direction_and_speed_cap() {
        assert_eq!(selection_scroll(50., 0., 100.), 0);
        assert_eq!(selection_scroll(100., 0., 100.), 0);
        assert_eq!(selection_scroll(101., 0., 100.), -1);
        assert_eq!(selection_scroll(-1., 0., 100.), 1);
        assert_eq!(selection_scroll(125., 0., 100.), -8);
        assert_eq!(selection_scroll(-25., 0., 100.), 8);
        assert_eq!(selection_scroll(1000., 0., 100.), -15);
        assert_eq!(selection_scroll(-1000., 0., 100.), 15);
    }

    #[test]
    fn cursor_edges_match_xterm_logical_pixels_at_device_scales_and_wide_cells() {
        for scale in [1., 1.5, 2.] {
            let metrics = Metrics::new(7.2, 12.828, 3.516, scale);
            for columns in [1, 2] {
                let width = metrics.width * columns as f32;
                assert_eq!(
                    cursor_rects(metrics, columns, CursorShape::Beam)[0],
                    Some(CursorRect {
                        x: 0.,
                        y: 0.,
                        width: 1.,
                        height: metrics.height
                    })
                );
                assert_eq!(
                    cursor_rects(metrics, columns, CursorShape::Underline)[0],
                    Some(CursorRect {
                        x: 0.,
                        y: metrics.height - 1.,
                        width,
                        height: 1.
                    })
                );
                let outline = cursor_rects(metrics, columns, CursorShape::HollowBlock);
                assert_eq!(
                    outline[2],
                    Some(CursorRect {
                        x: 0.,
                        y: 0.,
                        width,
                        height: 1.
                    })
                );
                assert_eq!(
                    outline[3],
                    Some(CursorRect {
                        x: width - 1.,
                        y: 0.,
                        width: 1.,
                        height: metrics.height
                    })
                );
                assert_eq!(
                    cursor_rects(metrics, columns, CursorShape::Block)[0],
                    Some(CursorRect {
                        x: 0.,
                        y: 0.,
                        width,
                        height: metrics.height
                    })
                );
            }
        }
        assert_eq!(
            cursor_rects(
                Metrics {
                    width: 7.,
                    height: 17.
                },
                1,
                CursorShape::Hidden
            ),
            [None; 4]
        );
    }

    #[test]
    fn cursor_blink_is_paced_restarts_on_movement_and_pauses_visibly_when_hidden_or_idle() {
        let now = Instant::now();
        let mut blink = CursorBlink::new(now);
        assert_eq!(blink.next(now), None);
        blink.enable(true, now);
        assert!(blink.visible());
        assert_eq!(blink.next(now), Some(Duration::from_millis(600)));
        blink.tick(now + Duration::from_millis(599));
        assert!(blink.visible());
        blink.tick(now + Duration::from_millis(600));
        assert!(!blink.visible());
        blink.restart(now + Duration::from_millis(650));
        assert!(blink.visible());
        blink.tick(now + Duration::from_millis(1200));
        assert!(blink.visible());
        blink.tick(now + Duration::from_millis(1250));
        assert!(!blink.visible());
        blink.enable(false, now + Duration::from_millis(1300));
        assert!(blink.visible());
        assert_eq!(blink.next(now), None);
        blink.enable(true, now + Duration::from_secs(2));
        blink.tick(now + Duration::from_secs(302));
        assert!(blink.visible());
        assert_eq!(blink.next(now + Duration::from_secs(302)), None);
        blink.restart(now + Duration::from_secs(303));
        assert_eq!(
            blink.next(now + Duration::from_secs(303)),
            Some(Duration::from_millis(600))
        );
    }

    #[test]
    fn grid_uses_device_pixels_without_accumulating_fractional_cell_advance() {
        assert_eq!(
            Metrics::new(7.200058, 12.828, -3.516, 1.),
            Metrics {
                width: 7.,
                height: 17.
            }
        );
        assert_eq!(
            Metrics::new(7.200058, 12.828, -3.516, 1.5),
            Metrics {
                width: 10. / 1.5,
                height: 26. / 1.5
            }
        );
        assert_eq!(
            Metrics::new(7.200058, 12.828, 3.516, 2.),
            Metrics {
                width: 7.,
                height: 17.
            }
        );
    }
}
