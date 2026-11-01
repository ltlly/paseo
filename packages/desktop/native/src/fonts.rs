use std::borrow::Cow;

pub const TERMINAL_FAMILY: &str = "JetBrains Mono";

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
