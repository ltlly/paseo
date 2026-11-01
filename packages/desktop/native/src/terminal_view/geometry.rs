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

#[cfg(test)]
mod tests {
    use super::*;

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
