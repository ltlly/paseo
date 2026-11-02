use gpui::{App, DevicePixels, RenderImage, SvgSize, Window, prelude::*, px, size};
use std::{collections::BTreeMap, sync::Arc};

/// One raster per canonical icon at the current display scale, shared by rows
/// in this surface rather than by each filename or previous DPI setting.
#[derive(Default)]
pub struct FileIconCache {
    pixels: i32,
    images: BTreeMap<&'static str, Arc<RenderImage>>,
}
impl FileIconCache {
    pub fn render(
        &mut self,
        file_name: &str,
        logical_size: f32,
        window: &Window,
        cx: &App,
    ) -> gpui::AnyElement {
        let pixels = (logical_size * window.scale_factor())
            .ceil()
            .clamp(1., 128.) as i32;
        if self.pixels != pixels {
            self.images.clear();
            self.pixels = pixels;
        }
        let (name, svg) = paseo_desktop::file_icons::lookup(file_name);
        if !self.images.contains_key(name) {
            let renderer = cx.svg_renderer();
            if let Ok(image) = renderer.parse_svg(svg.as_bytes()).and_then(|svg| {
                renderer.render_parsed(
                    &svg,
                    SvgSize::ExactSize(size(DevicePixels(pixels), DevicePixels(pixels))),
                )
            }) {
                self.images.insert(name, image);
            }
        }
        if let Some(image) = self.images.get(name) {
            gpui::img(image.clone())
                .size(px(logical_size))
                .into_any_element()
        } else {
            crate::chrome::icon("file-text", logical_size).into_any_element()
        }
    }
}
