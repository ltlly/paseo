use std::borrow::Cow;

pub const TERMINAL_FAMILY: &str = "JetBrains Mono";

pub fn ui_family() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        static FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        FAMILY.get_or_init(|| {
            // GPUI's .SystemUIFont is a Zed-specific IBM Plex alias on Linux. Match the
            // browser's system-ui selection through the host's fontconfig settings instead.
            std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "system-ui"])
                .output()
                .ok()
                .filter(|result| result.status.success())
                .and_then(|result| String::from_utf8(result.stdout).ok())
                .filter(|family| !family.trim().is_empty())
                .unwrap_or_else(|| "Noto Sans".into())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        ".SystemUIFont"
    }
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
