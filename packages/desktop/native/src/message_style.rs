//! Runtime message presentation derived from the appearance settings. The
//! generated `message_geometry` module stays the byte-exact sync of the authored
//! default sizes; this module re-derives the sizes that the app scales at apply
//! time (`packages/app/src/components/markdown/styles.ts`): the body uses the
//! absolute content size, headings scale from it, and code uses the absolute
//! code size.

use gpui::SharedString;
use paseo_desktop::{appearance::Palette, settings::Settings};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MessageGeometry {
    pub body_size: f32,
    pub body_line: f32,
    pub headings: [(f32, f32); 6],
    /// Inline code is a fraction of the body size, exactly like `fontScale`.
    pub inline_code_scale: f32,
    pub code_size: f32,
    pub code_line: f32,
}

impl MessageGeometry {
    pub fn resolve(content_font_size: i64, code_font_size: i64) -> Self {
        let body_size = content_font_size as f32;
        let code_size = code_font_size as f32;
        let heading = |scale: f32| {
            (
                (body_size * scale).round(),
                (body_size * scale * 1.3).round(),
            )
        };
        Self {
            body_size,
            body_line: (body_size * 1.4).round(),
            headings: [
                heading(1.6),
                heading(1.4),
                heading(1.2),
                heading(1.1),
                heading(1.0),
                heading(1.0),
            ],
            inline_code_scale: code_size / body_size,
            code_size,
            code_line: (code_size * 1.45).round(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageStyle {
    pub palette: Palette,
    pub geometry: MessageGeometry,
    pub ui_family: SharedString,
    pub mono_family: SharedString,
}

impl MessageStyle {
    pub fn resolve(settings: &Settings, palette: Palette) -> Self {
        Self {
            palette,
            geometry: MessageGeometry::resolve(settings.content_font_size, settings.code_font_size),
            ui_family: crate::fonts::ui_family_for(&settings.ui_font_family),
            mono_family: crate::fonts::mono_family_for(&settings.mono_font_family),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_geometry as authored;

    #[test]
    fn default_geometry_matches_the_authored_message_geometry() {
        let geometry = MessageGeometry::resolve(15, 12);
        assert_eq!(geometry.body_size, authored::BODY_SIZE);
        assert_eq!(geometry.body_line, authored::BODY_LINE);
        assert_eq!(geometry.headings, authored::HEADINGS);
        assert_eq!(geometry.code_size, authored::CODE_SIZE);
        assert_eq!(geometry.inline_code_scale, authored::INLINE_CODE_SCALE);
    }

    #[test]
    fn content_size_scales_the_body_and_headings() {
        let geometry = MessageGeometry::resolve(20, 12);
        assert_eq!(geometry.body_size, 20.);
        assert_eq!(geometry.body_line, 28.);
        assert_eq!(geometry.headings[0], (32., 42.));
        assert_eq!(geometry.headings[2], (24., 31.));
        assert_eq!(geometry.headings[5], (20., 26.));
    }

    #[test]
    fn code_size_is_absolute_and_sets_the_inline_scale() {
        let geometry = MessageGeometry::resolve(20, 16);
        assert_eq!(geometry.code_size, 16.);
        assert_eq!(geometry.code_line, 23.);
        assert_eq!(geometry.inline_code_scale, 0.8);
    }
}
