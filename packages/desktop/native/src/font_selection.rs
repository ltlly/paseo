/// Match a GTK/Pango description to an installed family, keeping style and point size
/// out of the family name. The caller still selects each control's CSS weight.
pub fn description_family<'a>(description: &str, families: &'a [String]) -> Option<&'a str> {
    let description = description.trim();
    let description = description
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| {
            description
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
        })
        .unwrap_or(description);
    let description = description.replace("\\'", "'").replace("\\\"", "\"");
    let normalized = description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    families
        .iter()
        .filter(|family| {
            let name = family.to_lowercase();
            normalized
                .strip_prefix(&name)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with([',', ' ']))
        })
        .max_by_key(|family| family.len())
        .map(String::as_str)
}

pub fn gtk_settings_font(settings: &str) -> Option<&str> {
    let mut active = false;
    for line in settings.lines().map(str::trim) {
        if line.starts_with(['#', ';']) || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            active = line == "[Settings]";
            continue;
        }
        if active
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "gtk-font-name"
            && !value.trim().is_empty()
        {
            return Some(value.trim());
        }
    }
    None
}

/// Split a CSS `font-family` list into family names. Commas inside quotes stay
/// part of the name, and a backslash escapes the next character; CSS requires
/// quotes around names containing whitespace or punctuation.
pub fn parse_family_list(css: &str) -> Vec<String> {
    let mut families = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in css.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if Some(ch) == quote {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && ch == ',' {
            let family = current.trim();
            if !family.is_empty() {
                families.push(family.to_owned());
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    let family = current.trim();
    if !family.is_empty() {
        families.push(family.to_owned());
    }
    families
}

/// CSS generic names are not font family names in native text backends; map
/// them to the system alias the caller resolved for that kind of text.
fn generic_family<'a>(name: &str, mono: &'a str, sans: &'a str) -> Option<&'a str> {
    match name.to_ascii_lowercase().as_str() {
        "monospace" | "ui-monospace" => Some(mono),
        "system-ui" | "sans-serif" | "ui-sans-serif" => Some(sans),
        _ => None,
    }
}

/// Resolve a user-supplied CSS stack to the families this machine can actually
/// load, in preference order. The first result is safe to use as a primary
/// family: native text backends resolve a missing primary through their global
/// fallback stack and never reach the font's own glyph fallbacks.
pub fn custom_families(css: &str, available: &[String], mono: &str, sans: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for family in parse_family_list(css) {
        let resolved = generic_family(&family, mono, sans)
            .map(str::to_owned)
            .or_else(|| {
                available
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(&family))
                    .cloned()
            });
        if let Some(name) = resolved
            && !result
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            result.push(name);
        }
    }
    result
}

/// Resolve the authored CSS stack against system fonts before bundled fallback
/// faces are registered. Bundling a face must not change the user's default.
pub fn terminal_families(preferred: &[&str], available: &[String], generic: &str) -> Vec<String> {
    let mut result = Vec::new();
    for family in preferred {
        let resolved = if *family == "monospace" {
            Some(generic)
        } else {
            available
                .iter()
                .find(|name| name.eq_ignore_ascii_case(family))
                .map(String::as_str)
        };
        if let Some(name) = resolved
            && !result
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(name))
        {
            result.push(name.to_owned());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_defaults_preserve_css_preference_and_resolve_the_generic_family() {
        let available = [
            "Consolas".into(),
            "Cascadia Code".into(),
            "Noto Sans Mono".into(),
        ];
        assert_eq!(
            terminal_families(
                &["Cascadia Mono", "Cascadia Code", "Consolas", "monospace"],
                &available,
                "Consolas"
            ),
            ["Cascadia Code", "Consolas"]
        );
        assert_eq!(
            terminal_families(
                &["JetBrains Mono", "Missing", "monospace"],
                &available,
                "Noto Sans Mono"
            ),
            ["Noto Sans Mono"]
        );
        assert_eq!(
            terminal_families(
                &["consolas", "CASCADIA CODE", "monospace"],
                &available,
                "Consolas"
            ),
            ["Consolas", "Cascadia Code"]
        );
    }

    #[test]
    fn css_family_lists_parse_quotes_escapes_and_commas() {
        assert_eq!(
            parse_family_list("\"Fira Code\", 'JetBrains Mono', monospace"),
            ["Fira Code", "JetBrains Mono", "monospace"]
        );
        assert_eq!(
            parse_family_list("\"Font, With Comma\", Inter"),
            ["Font, With Comma", "Inter"]
        );
        assert_eq!(
            parse_family_list("D\\'Angelo Sans, Inter"),
            ["D'Angelo Sans", "Inter"]
        );
        assert_eq!(parse_family_list(" , , Inter "), ["Inter"]);
        assert!(parse_family_list("").is_empty());
        assert!(parse_family_list(" , ").is_empty());
    }

    #[test]
    fn custom_stacks_pick_the_first_installed_family_or_generic() {
        let available = [
            "JetBrains Mono".into(),
            "Consolas".into(),
            "Noto Sans".into(),
        ];
        assert_eq!(
            custom_families(
                "\"Not Installed\", JetBrains Mono, monospace",
                &available,
                "Noto Sans Mono",
                "Noto Sans"
            ),
            ["JetBrains Mono", "Noto Sans Mono"]
        );
        assert_eq!(
            custom_families(
                "Missing, monospace",
                &available,
                "Noto Sans Mono",
                "Noto Sans"
            ),
            ["Noto Sans Mono"]
        );
        assert_eq!(
            custom_families(
                "consolas, CASCADIA CODE, monospace",
                &available,
                "Consolas",
                "Noto Sans"
            ),
            ["Consolas"]
        );
        assert_eq!(
            custom_families(
                "system-ui, Inter",
                &available,
                "Noto Sans Mono",
                "Noto Sans"
            ),
            ["Noto Sans"]
        );
        assert!(custom_families("", &available, "Noto Sans Mono", "Noto Sans").is_empty());
        assert!(custom_families("Missing", &available, "Noto Sans Mono", "Noto Sans").is_empty());
    }

    #[test]
    fn gtk_family_wins_over_an_unrelated_fontconfig_system_ui_alias() {
        let available = ["Noto Sans".into(), "SF Pro Text".into()];
        assert_eq!(
            description_family("'SF Pro Text  10'", &available),
            Some("SF Pro Text")
        );
        assert_eq!(
            description_family("SF Pro Text,  10", &available),
            Some("SF Pro Text")
        );
    }

    #[test]
    fn style_size_and_prefix_collisions_do_not_change_the_family() {
        let available = [
            "Noto Sans".into(),
            "Noto Sans Mono".into(),
            "Sans".into(),
            "D'Angelo Sans".into(),
            "思源黑体".into(),
        ];
        assert_eq!(
            description_family("Noto Sans Mono Bold Italic 11", &available),
            Some("Noto Sans Mono")
        );
        assert_eq!(
            description_family("'D\\'Angelo Sans 12'", &available),
            Some("D'Angelo Sans")
        );
        assert_eq!(
            description_family("思源黑体, 10", &available),
            Some("思源黑体")
        );
        assert_eq!(description_family("Sansation 11", &available), None);
        assert_eq!(description_family("Missing 10", &available), None);
    }

    #[test]
    fn gtk_file_reads_only_the_active_settings_section() {
        let settings = ";gtk-font-name=Wrong 11\n[Other]\ngtk-font-name=Wrong 10\n[Settings]\n# note\ngtk-font-name = SF Pro Text,  10\n[Other]\ngtk-font-name=Wrong 12";
        assert_eq!(gtk_settings_font(settings), Some("SF Pro Text,  10"));
        assert_eq!(gtk_settings_font("gtk-font-name=Wrong 11"), None);
        assert_eq!(gtk_settings_font("[Settings]\ngtk-font-name=\n"), None);
    }
}
