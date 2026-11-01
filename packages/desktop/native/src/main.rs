mod assets;
mod chrome;
mod composer_view;
mod conversation;
mod fonts;
mod identity_colors;
mod terminal_view;
mod theme;
mod ui;

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use paseo_desktop::client::{Target, default_home};

fn main() {
    let target = match std::env::var("PASEO_ENDPOINT") {
        Ok(url) => Target::remote(url, std::env::var("PASEO_PASSWORD").ok()),
        Err(_) => default_home().and_then(|home| Target::local(&home)),
    };
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            cx.text_system()
                .add_fonts(fonts::terminal_faces())
                .expect("load bundled terminal fonts");
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Paseo Native".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| ui::Paseo::new(target, cx)),
            )
            .expect("open Paseo window");
            cx.activate(true);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
}
