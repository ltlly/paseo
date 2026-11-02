//! Code presentation shared by message fences and tool details.
//!
//! Message fences wrap like the markdown renderer's code block. Tool output keeps authored lines
//! and scrolls horizontally with the message style's code size, line height, mono family and
//! palette, so a settings change reaches both the fence and the tool details.
use crate::{message_style::MessageStyle, syntax};
use gpui::{AnyElement, FontWeight, SharedString, StyledText, TextRun, div, prelude::*, px, rgb};
use std::ops::Range;

pub fn mono_run(len: usize, color: u32, bold: bool, style: &MessageStyle) -> TextRun {
    let mut font = gpui::font(style.mono_family.clone());
    if bold {
        font.weight = FontWeight::BOLD;
    }
    TextRun {
        len,
        font,
        color: rgb(color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Runs covering the complete text for a wrapping code block.
pub fn block_runs(text: &str, language: &str, style: &MessageStyle) -> Vec<TextRun> {
    runs_for_range(text, language, 0..text.len(), style)
}

fn runs_for_range(
    text: &str,
    language: &str,
    range: Range<usize>,
    style: &MessageStyle,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut cursor = range.start;
    for (segment, color) in syntax::styled_runs(text, language, &style.palette) {
        if segment.end <= range.start || segment.start >= range.end {
            continue;
        }
        let start = segment.start.max(range.start);
        let end = segment.end.min(range.end);
        if start > cursor {
            runs.push(mono_run(
                start - cursor,
                style.palette.foreground,
                false,
                style,
            ));
        }
        runs.push(mono_run(end - start, color, false, style));
        cursor = end;
    }
    if cursor < range.end {
        runs.push(mono_run(
            range.end - cursor,
            style.palette.foreground,
            false,
            style,
        ));
    }
    runs
}

/// Non-wrapping lines with an optional 1-based gutter, matching the desktop code block.
pub fn lines(
    text: &str,
    language: &str,
    start_line: Option<u32>,
    style: &MessageStyle,
) -> AnyElement {
    let code_size = style.geometry.code_size;
    let code_line = style.geometry.code_line;
    let mut column = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .min_w_full()
        .text_size(px(code_size))
        .line_height(px(code_line));
    let gutter_width = start_line.map(|start| {
        let last = start + text.split('\n').count().saturating_sub(1) as u32;
        let digits = last.to_string().len().max(2);
        (digits * (code_size * 0.62).ceil() as usize + 12) as f32
    });
    let mut offset = 0;
    for (index, line) in text.split('\n').enumerate() {
        let range = offset..offset + line.len();
        let mut row = div().flex().flex_shrink_0().h(px(code_line));
        if let (Some(start), Some(width)) = (start_line, gutter_width) {
            row = row.child(
                div()
                    .flex_shrink_0()
                    .w(px(width))
                    .pr(px(4.))
                    .text_color(rgb(style.palette.foreground_muted))
                    .child(SharedString::from(format!(
                        "{:>width$}",
                        start + index as u32,
                        width = (width as usize) / 8
                    ))),
            );
        }
        let content = if line.is_empty() {
            StyledText::new(SharedString::from("\u{200b}")).with_runs(vec![mono_run(
                "\u{200b}".len(),
                style.palette.foreground,
                false,
                style,
            )])
        } else {
            StyledText::new(SharedString::from(line.to_owned()))
                .with_runs(runs_for_range(text, language, range, style))
        };
        row = row.child(div().flex_shrink_0().whitespace_nowrap().child(content));
        column = column.child(row);
        offset += line.len() + 1;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    div()
        .id(("code-block", hasher.finish()))
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .child(column)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use paseo_desktop::{
        appearance::resolve,
        settings::{Settings, ThemePreference},
    };

    fn style(theme: ThemePreference, code_font_size: i64) -> MessageStyle {
        let mut settings = Settings::default();
        settings.code_font_size = code_font_size;
        MessageStyle::resolve(&settings, resolve(theme, false))
    }

    #[test]
    fn runs_cover_every_byte_of_a_code_block() {
        let style = style(ThemePreference::Dark, 12);
        let code = "fn main() {\n    let x = 1;\n}\n";
        let runs = block_runs(code, "rust", &style);
        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), code.len());
        let mut start = 0;
        for run in &runs {
            assert!(run.len > 0);
            start += run.len;
            assert!(code.is_char_boundary(start));
        }
    }

    #[test]
    fn line_ranges_do_not_split_multi_byte_characters() {
        let style = style(ThemePreference::Dark, 12);
        let code = "中文 = \"值\"\nnext\n";
        let runs = runs_for_range(code, "python", 0.."中文 = \"值\"".len(), &style);
        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            "中文 = \"值\"".len()
        );
    }

    #[test]
    fn code_geometry_and_palette_follow_the_message_style() {
        let light = style(ThemePreference::Light, 16);
        let dark = style(ThemePreference::Dark, 16);
        assert_eq!(light.geometry.code_size, 16.);
        assert_eq!(light.geometry.code_line, 23.);
        let runs = block_runs("let x = 1;", "rust", &light);
        assert!(runs.iter().all(|run| run.len > 0));
        assert_ne!(
            light.palette.foreground, dark.palette.foreground,
            "the same code must not keep the dark foreground in light mode"
        );
        assert_eq!(
            mono_run(1, light.palette.foreground, false, &light)
                .font
                .family,
            light.mono_family
        );
    }
}

pub fn language_for_path(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or("").trim().to_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "ts" => "ts",
        "tsx" => "tsx",
        "js" => "js",
        "jsx" => "jsx",
        "mjs" | "cjs" => "js",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
        "m" | "mm" => "objc",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" | "zsh" => "sh",
        "json" => "json",
        "jsonc" => "jsonc",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" => "ini",
        "nix" => "nix",
        "ex" | "exs" => "elixir",
        "hs" => "haskell",
        "sql" => "sql",
        "lua" => "lua",
        "css" | "scss" | "less" => "css",
        "html" | "htm" => "html",
        "xml" | "svg" => "xml",
        "vue" => "vue",
        "svelte" => "svelte",
        "md" | "mdx" => "markdown",
        "diff" | "patch" => "diff",
        _ => "",
    }
}
