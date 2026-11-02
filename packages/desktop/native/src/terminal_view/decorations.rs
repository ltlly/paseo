//! Geometry for underline and strikethrough decorations.
//!
//! xterm draws these inside the glyph atlas, not with CSS or font underline
//! metrics: the stroke width comes from the font size, single and double
//! underlines are centred on the cell's bottom edge, the curly stroke is the
//! Monaco zigzag, and the dot/dash patterns restart at every cell. Painting the
//! same rectangles from the grid keeps the five SGR 4 styles and SGR 58 colours
//! identical to the reference renderer.
//!
//! The rules are transcribed from `@xterm/addon-webgl` 0.20.0-beta.291
//! `TextureAtlas.ts` (`_getRasterizedGlyph`: the underline block, the overline
//! block and the strikethrough line).
//!
//! All lengths here are device pixels relative to the cell origin. The caller
//! divides by the scale when it paints.
//!
//! The anchor is `ceil(cell height)`, which is the reference's
//! `deviceCharHeight`: the app sets `lineHeight: 1.0`, so the cell box and the
//! character box end on the same device row.

use crate::terminal::Underline;

/// xterm's underline stroke width: `max(1, floor(fontSize * dpr / 15))`.
pub fn stroke_thickness(font_size: f32, scale: f32) -> f32 {
    (font_size * scale / 15.).floor().max(1.)
}

/// Strikethrough uses a thicker stroke: `max(1, floor(fontSize * dpr / 10))`.
pub fn strikethrough_thickness(font_size: f32, scale: f32) -> f32 {
    (font_size * scale / 10.).floor().max(1.)
}

/// Half-device-pixel centring for odd stroke widths, so a one-pixel line lands
/// on one device row instead of straddling two.
fn centring_offset(thickness: f32) -> f32 {
    if thickness as i32 % 2 == 1 { 0.5 } else { 0. }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stripe {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A cell holds at most a handful of stripes, so the geometry never allocates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stripes {
    items: [Option<Stripe>; 8],
    len: usize,
}

impl Stripes {
    fn push(&mut self, stripe: Stripe) {
        if self.len < self.items.len() {
            self.items[self.len] = Some(stripe);
            self.len += 1;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Stripe> + '_ {
        self.items[..self.len].iter().filter_map(|stripe| *stripe)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Device width of the columns the cluster owns.
    pub width: f32,
    /// Device height of the cell.
    pub height: f32,
    pub scale: f32,
    pub font_size: f32,
}

impl Metrics {
    pub fn thickness(&self) -> f32 {
        stroke_thickness(self.font_size, self.scale)
    }

    /// `deviceCharHeight`: the reference anchors every decoration on
    /// `ceil(cell height)`.
    fn char_height(&self) -> f32 {
        self.height.ceil()
    }
}

pub fn underline_stripes(style: Underline, metrics: Metrics) -> Stripes {
    let thickness = metrics.thickness();
    let offset = centring_offset(thickness);
    let top = metrics.char_height() - offset - thickness / 2.;
    let full = Stripe {
        x: 0.,
        y: top,
        width: metrics.width,
        height: thickness,
    };
    let mut stripes = Stripes::default();
    match style {
        Underline::None => {}
        Underline::Single => stripes.push(full),
        Underline::Double => {
            stripes.push(full);
            stripes.push(Stripe {
                y: top + thickness * 2.,
                ..full
            });
        }
        Underline::Dotted => {
            let dash = thickness.round().max(1.);
            let mut x = 0.;
            while x < metrics.width {
                stripes.push(Stripe {
                    x,
                    width: dash.min(metrics.width - x),
                    ..full
                });
                x += dash * 2.;
            }
        }
        Underline::Dashed => {
            let line = (metrics.width * 0.6).floor();
            let gap = (metrics.width * 0.3).floor();
            stripes.push(Stripe {
                width: line.min(metrics.width),
                ..full
            });
            let second = (line + gap).min(metrics.width);
            if second < metrics.width {
                stripes.push(Stripe {
                    x: second,
                    width: metrics.width - second,
                    ..full
                });
            }
        }
        // The curly stroke is a filled path, not a stripe.
        Underline::Curly => {}
    }
    stripes
}

pub fn strikethrough_stripes(metrics: Metrics) -> Stripes {
    let thickness = strikethrough_thickness(metrics.font_size, metrics.scale);
    let offset = centring_offset(thickness);
    let mut stripes = Stripes::default();
    stripes.push(Stripe {
        x: 0.,
        y: (metrics.char_height() / 2.).floor() - offset - thickness / 2.,
        width: metrics.width,
        height: thickness,
    });
    stripes
}

/// The three Monaco zigzag polygons that make up a curly underline, in device
/// pixels relative to the cell origin. The shape is 6×3 units scaled to the
/// cell, and the band starts one device pixel below the character box, matching
/// the reference renderer's overhang.
pub fn curly_polygons(metrics: Metrics) -> [[[f32; 2]; 4]; 3] {
    const SHAPES: [[f32; 8]; 3] = [
        [0., 2., 1., 3., 2.4, 3., 0., 0.6],
        [5.5, 0., 2.5, 3., 1.1, 3., 4.1, 0.],
        [4., 0., 6., 2., 6., 0.6, 5.4, 0.],
    ];
    let scale_x = metrics.width / 6.;
    let scale_y = metrics.scale;
    let top = metrics.char_height() + 1.;
    SHAPES.map(|shape| {
        std::array::from_fn(|index| {
            [
                shape[index * 2] * scale_x,
                top + 3. * scale_y - shape[index * 2 + 1] * scale_y,
            ]
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(width: f32, height: f32, scale: f32) -> Metrics {
        Metrics {
            width,
            height,
            scale,
            font_size: 12.,
        }
    }

    fn rects(style: Underline, metrics: Metrics) -> Vec<(f32, f32, f32, f32)> {
        underline_stripes(style, metrics)
            .iter()
            .map(|stripe| (stripe.x, stripe.y, stripe.width, stripe.height))
            .collect()
    }

    #[test]
    fn stroke_width_follows_font_size_at_the_device_scale() {
        for (font_size, scale, expected) in [
            (12., 1., 1.),
            (12., 1.5, 1.),
            (12., 2., 1.),
            (20., 1., 1.),
            (20., 2., 2.),
            (30., 1., 2.),
            (45., 1., 3.),
        ] {
            assert_eq!(stroke_thickness(font_size, scale), expected);
        }
    }

    #[test]
    fn single_and_double_underline_are_centred_on_the_cell_bottom_edge() {
        assert_eq!(
            rects(Underline::Single, metrics(7., 17., 1.)),
            [(0., 16., 7., 1.)]
        );
        assert_eq!(
            rects(Underline::Double, metrics(7., 17., 1.)),
            [(0., 16., 7., 1.), (0., 18., 7., 1.)]
        );
        // Two-device-pixel cell height but a 12px font: the stroke stays one
        // device pixel wide and is still centred on the bottom edge.
        assert_eq!(
            rects(Underline::Single, metrics(14., 34., 2.)),
            [(0., 33., 14., 1.)]
        );
        assert_eq!(
            rects(Underline::Double, metrics(14., 34., 2.)),
            [(0., 33., 14., 1.), (0., 35., 14., 1.)]
        );
        // A 20px font at 2x asks for a two-device-pixel stroke, which is not
        // half-pixel shifted.
        let thick = Metrics {
            width: 14.,
            height: 40.,
            scale: 2.,
            font_size: 20.,
        };
        assert_eq!(
            underline_stripes(Underline::Single, thick)
                .iter()
                .map(|stripe| (stripe.y, stripe.height))
                .collect::<Vec<_>>(),
            [(39., 2.)]
        );
    }

    #[test]
    fn dotted_and_dashed_patterns_restart_at_every_cell() {
        assert_eq!(
            rects(Underline::Dotted, metrics(7., 17., 1.)),
            [
                (0., 16., 1., 1.),
                (2., 16., 1., 1.),
                (4., 16., 1., 1.),
                (6., 16., 1., 1.)
            ]
        );
        assert_eq!(
            rects(Underline::Dashed, metrics(7., 17., 1.)),
            [(0., 16., 4., 1.), (6., 16., 1., 1.)]
        );
        assert_eq!(
            rects(Underline::Dashed, metrics(10., 17., 1.)),
            [(0., 16., 6., 1.), (9., 16., 1., 1.)]
        );
        assert!(rects(Underline::None, metrics(7., 17., 1.)).is_empty());
    }

    #[test]
    fn curly_underline_is_the_monaco_zigzag_hanging_below_the_cell() {
        let metrics = metrics(7., 17., 1.);
        let polygons = curly_polygons(metrics);
        assert_eq!(polygons[0][0], [0., 19.]);
        assert_eq!(polygons[0][1], [7. / 6., 18.]);
        assert_eq!(polygons[1][0], [5.5 * 7. / 6., 21.]);
        assert_eq!(polygons[2][2], [7., 20.4]);
        for polygon in polygons {
            for [x, y] in polygon {
                assert!((0. ..=7.).contains(&x), "{x} inside the cell columns");
                assert!((18. ..=21.).contains(&y), "{y} inside the zigzag band");
            }
        }
        // The band is `1 + 3 * dpr` below the character box at any scale.
        let scaled = curly_polygons(metrics_with_scale(2.));
        assert_eq!(scaled[0][0][1], 34. + 1. + 3. * 2. - 2. * 2.);
        assert!(rects(Underline::Curly, metrics).is_empty());
    }

    fn metrics_with_scale(scale: f32) -> Metrics {
        Metrics {
            width: 14.,
            height: 34.,
            scale,
            font_size: 12.,
        }
    }

    #[test]
    fn strikethrough_uses_the_cell_midline() {
        assert_eq!(
            strikethrough_stripes(metrics(7., 17., 1.))
                .iter()
                .map(|stripe| (stripe.x, stripe.y, stripe.width, stripe.height))
                .collect::<Vec<_>>(),
            [(0., 7., 7., 1.)]
        );
        assert_eq!(
            strikethrough_stripes(metrics(14., 34., 2.))
                .iter()
                .map(|stripe| (stripe.x, stripe.y, stripe.width, stripe.height))
                .collect::<Vec<_>>(),
            [(0., 16., 14., 2.)]
        );
        // Strikethrough is `floor(fontSize * dpr / 10)`, so it is thicker than
        // the underline for the same font.
        let thick = Metrics {
            width: 14.,
            height: 40.,
            scale: 2.,
            font_size: 20.,
        };
        assert_eq!(thick.thickness(), 2.);
        assert_eq!(strikethrough_thickness(thick.font_size, thick.scale), 4.);
    }

    #[test]
    fn decorations_anchor_on_the_ceil_of_a_fractional_cell_height() {
        // `deviceCharHeight` is `ceil(charHeight)`; a cell box that ends on a
        // fraction still paints whole device rows.
        assert_eq!(
            rects(Underline::Single, metrics(7., 16.6, 1.)),
            [(0., 16., 7., 1.)]
        );
        assert_eq!(
            strikethrough_stripes(metrics(7., 16.6, 1.))
                .iter()
                .map(|stripe| (stripe.y, stripe.height))
                .collect::<Vec<_>>(),
            [(7., 1.)]
        );
    }
}
