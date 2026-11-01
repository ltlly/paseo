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
