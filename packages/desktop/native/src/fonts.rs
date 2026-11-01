use std::borrow::Cow;

pub const TERMINAL_FAMILY: &str = "JetBrains Mono";
static TERMINAL_FONT: std::sync::OnceLock<gpui::Font> = std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
static UI_FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn configure(cx: &mut gpui::App) {
    let available = cx.text_system().all_font_names();
    let preferred = if cfg!(target_os = "windows") {
        crate::theme::terminal::WINDOWS_FONT_FAMILIES
    } else {
        crate::theme::terminal::FONT_FAMILIES
    };
    let mut families =
        paseo_desktop::font_selection::terminal_families(preferred, &available, code_family());
    let primary = families.remove(0);
    let mut face = gpui::font(primary);
    face.fallbacks = Some(gpui::FontFallbacks::from_fonts(families));
    let _ = TERMINAL_FONT.set(face);
    #[cfg(target_os = "linux")]
    {
        use paseo_desktop::font_selection::{description_family, gtk_settings_font};
        let families = cx.text_system().all_font_names();
        UI_FAMILY.get_or_init(|| {
            // Chromium's Linux system-ui comes from GTK. Fontconfig's literal "system-ui"
            // alias can resolve to a different family (SF Pro Text vs Noto Sans in QA).
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".config"))
                });
            if let Some(config) = config {
                for version in ["gtk-3.0", "gtk-4.0"] {
                    if let Ok(settings) =
                        std::fs::read_to_string(config.join(version).join("settings.ini"))
                        && let Some(description) = gtk_settings_font(&settings)
                        && let Some(family) = description_family(description, &families)
                    {
                        return family.to_owned();
                    }
                }
            }
            if let Ok(result) = std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "font-name"])
                .output()
                && result.status.success()
                && let Ok(description) = String::from_utf8(result.stdout)
                && let Some(family) = description_family(&description, &families)
            {
                return family.to_owned();
            }
            std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "system-ui"])
                .output()
                .ok()
                .filter(|result| result.status.success())
                .and_then(|result| String::from_utf8(result.stdout).ok())
                .filter(|family| !family.trim().is_empty())
                .unwrap_or_else(|| ".SystemUIFont".into())
        });
        // The composited Electron UI uses grayscale coverage. The Linux GPUI default
        // introduces RGB fringes that change the weight and shape of the same font.
        cx.set_text_rendering_mode(gpui::TextRenderingMode::Grayscale);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cx;
    }
}

pub fn terminal_font() -> gpui::Font {
    TERMINAL_FONT
        .get()
        .expect("configure platform fonts before opening a terminal")
        .clone()
}

pub fn ui_family() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        UI_FAMILY
            .get()
            .map(String::as_str)
            .unwrap_or(".SystemUIFont")
    }
    #[cfg(not(target_os = "linux"))]
    {
        ".SystemUIFont"
    }
}

// Match the integer ascent/descent strut used by Chromium's Linux UI font. GPUI's
// inherited 20px chrome line box otherwise makes a spinner toast two pixels taller.
// Fonts with extra line gap and the other platform backends still need matched QA.
pub fn ui_strut(window: &gpui::Window, size: f32) -> gpui::Pixels {
    let system = window.text_system();
    let id = system.resolve_font(&gpui::font(ui_family()));
    system.ascent(id, gpui::px(size)).round() + system.descent(id, gpui::px(size)).abs().round()
}

// Reuse the licensed font assets shipped by the mobile terminal. A CSS generic
// family such as "monospace" is not a font family name in native text backends.
pub fn terminal_faces() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(include_bytes!(
            "../../../app/modules/paseo-terminal-fonts/android/src/main/assets/paseo-terminal-fonts/JetBrainsMono-Regular.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../../app/modules/paseo-terminal-fonts/android/src/main/assets/paseo-terminal-fonts/JetBrainsMono-Bold.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../../app/modules/paseo-terminal-fonts/android/src/main/assets/paseo-terminal-fonts/JetBrainsMono-Italic.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../../app/modules/paseo-terminal-fonts/android/src/main/assets/paseo-terminal-fonts/JetBrainsMono-BoldItalic.ttf"
        )),
    ]
}

pub fn code_family() -> &'static str {
    static FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FAMILY.get_or_init(|| {
        #[cfg(target_os = "linux")]
        if let Ok(output) = std::process::Command::new("fc-match")
            .args(["-f", "%{family[0]}", "monospace"])
            .output()
            && output.status.success()
            && let Ok(name) = String::from_utf8(output.stdout)
            && !name.is_empty()
        {
            return name;
        }
        TERMINAL_FAMILY.into()
    })
}
