//! Client appearance preferences persisted by the native desktop client.
//!
//! Field names, defaults and clamp bounds mirror the app's client settings
//! (`packages/app/src/hooks/use-settings/storage.ts`) so the values keep the
//! same meaning as the app's AsyncStorage document; the two clients persist to
//! separate stores and never write each other's files. Persistence is a single JSON document
//! written atomically (temp file + rename) under the user's platform config
//! directory; a corrupt file is an error surfaced in the settings UI, never
//! silently replaced with defaults.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::locales::Language;

pub const DEFAULT_UI_BASE_FONT_SIZE: i64 = 14;
pub const MIN_UI_BASE_FONT_SIZE: i64 = 10;
pub const MAX_UI_BASE_FONT_SIZE: i64 = 21;
pub const DEFAULT_CONTENT_FONT_SIZE: i64 = 15;
pub const MIN_CONTENT_FONT_SIZE: i64 = 10;
pub const MAX_CONTENT_FONT_SIZE: i64 = 21;
pub const DEFAULT_CODE_FONT_SIZE: i64 = 12;
pub const MIN_CODE_FONT_SIZE: i64 = 9;
pub const MAX_CODE_FONT_SIZE: i64 = 22;
pub const DEFAULT_TERMINAL_SCROLLBACK_LINES: i64 = 10_000;
pub const MIN_TERMINAL_SCROLLBACK_LINES: i64 = 0;
pub const MAX_TERMINAL_SCROLLBACK_LINES: i64 = 1_000_000;
pub const DEFAULT_CONTENT_MAX_WIDTH: i64 = 820;
pub const MIN_CONTENT_MAX_WIDTH: i64 = 600;
pub const MAX_CONTENT_MAX_WIDTH: i64 = 4000;

const MAX_FONT_FAMILY_LENGTH: usize = 200;

/// Authored theme names. `plugin` (a contributed theme) has no native palette
/// yet, so reading it falls back to [`ThemePreference::Auto`] exactly like the
/// app renders its fallback when a contribution is no longer installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemePreference {
    #[default]
    Auto,
    Light,
    Dark,
    Zinc,
    Midnight,
    Claude,
    Ghostty,
    PureBlack,
}

impl ThemePreference {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Zinc => "zinc",
            Self::Midnight => "midnight",
            Self::Claude => "claude",
            Self::Ghostty => "ghostty",
            Self::PureBlack => "pureBlack",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Zinc => "Zinc",
            Self::Midnight => "Midnight",
            Self::Claude => "Claude",
            Self::Ghostty => "Ghostty",
            Self::PureBlack => "Pure black",
        }
    }

    /// The swatch color authored for the theme picker; `None` for `auto`, which
    /// the app renders with a sun/moon/monitor glyph instead.
    pub const fn swatch(self) -> Option<u32> {
        match self {
            Self::Light => Some(0xffffff),
            Self::Dark => Some(0x2d8b62),
            Self::Zinc => Some(0x808080),
            Self::Midnight => Some(0x4a6ba8),
            Self::Claude => Some(0xd97757),
            Self::Ghostty => Some(0x8caaee),
            Self::PureBlack => Some(0x000000),
            Self::Auto => None,
        }
    }

    pub fn parse(value: Option<&str>) -> Option<Self> {
        Some(match value? {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "zinc" => Self::Zinc,
            "midnight" => Self::Midnight,
            "claude" => Self::Claude,
            "ghostty" => Self::Ghostty,
            "pureBlack" => Self::PureBlack,
            "auto" => Self::Auto,
            // A contributed theme the native client cannot render yet.
            "plugin" => Self::Auto,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemePreference,
    /// Client language: follow the system locale or an explicit supported
    /// locale (`docs/i18n.md`); mirrors the app's persisted `language` value.
    pub language: Language,
    /// Empty string means the platform default UI stack.
    pub ui_font_family: String,
    /// Empty string means the platform default mono stack.
    pub mono_font_family: String,
    pub ui_base_font_size: i64,
    pub content_font_size: i64,
    pub code_font_size: i64,
    /// `None` follows the authored default content width.
    pub content_max_width: Option<i64>,
    pub terminal_scrollback_lines: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::Auto,
            language: Language::default(),
            ui_font_family: String::new(),
            mono_font_family: String::new(),
            ui_base_font_size: DEFAULT_UI_BASE_FONT_SIZE,
            content_font_size: DEFAULT_CONTENT_FONT_SIZE,
            code_font_size: DEFAULT_CODE_FONT_SIZE,
            content_max_width: None,
            terminal_scrollback_lines: DEFAULT_TERMINAL_SCROLLBACK_LINES,
        }
    }
}

/// Port of the app's `parseClampedFontSize`: digits that fall outside the
/// bounds clamp instead of rejecting the whole document.
pub fn parse_clamped_font_size(value: &Value, min: i64, max: i64) -> Option<i64> {
    let numeric = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) if !text.trim().is_empty() => text.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    if !numeric.is_finite() {
        return None;
    }
    Some(numeric.floor().clamp(min as f64, max as f64) as i64)
}

/// Port of the app's `parseTerminalScrollbackLines`.
pub fn parse_scrollback_lines(value: &Value) -> Option<i64> {
    parse_clamped_font_size(
        value,
        MIN_TERMINAL_SCROLLBACK_LINES,
        MAX_TERMINAL_SCROLLBACK_LINES,
    )
}

/// Port of the app's `sanitizeFontFamily`. `None` means the value is rejected;
/// the caller keeps the previously committed family.
pub fn sanitize_font_family(value: &Value) -> Option<String> {
    let trimmed = value.as_str()?.trim();
    if trimmed.is_empty() {
        return Some(String::new()); // explicit empty = default stack
    }
    if trimmed.chars().count() > MAX_FONT_FAMILY_LENGTH {
        return None;
    }
    if trimmed.contains([';', '{', '}', '<', '>']) {
        return None;
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return None;
    }
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in trimmed.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if let Some(open) = quote {
            if ch == open {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if matches!(ch, '(' | ')' | '[' | ']' | '/' | '*') {
            return None;
        }
    }
    (quote.is_none() && !escaped).then(|| trimmed.to_owned())
}

fn read_i64(object: &serde_json::Map<String, Value>, key: &str, fallback: i64) -> i64 {
    object.get(key).map_or(fallback, |value| {
        parse_clamped_font_size(value, i64::MIN, i64::MAX).unwrap_or(fallback)
    })
}

impl Settings {
    /// Apply the fields that differ between `base` and `next` onto `target`.
    /// Concurrent writers start from their own snapshot; merging the delta keeps
    /// a change made in another window instead of replacing the whole document.
    pub fn merge_delta(base: &Self, next: &Self, target: &mut Self) {
        if base.theme != next.theme {
            target.theme = next.theme;
        }
        if base.language != next.language {
            target.language = next.language;
        }
        if base.ui_font_family != next.ui_font_family {
            target.ui_font_family = next.ui_font_family.clone();
        }
        if base.mono_font_family != next.mono_font_family {
            target.mono_font_family = next.mono_font_family.clone();
        }
        if base.ui_base_font_size != next.ui_base_font_size {
            target.ui_base_font_size = next.ui_base_font_size;
        }
        if base.content_font_size != next.content_font_size {
            target.content_font_size = next.content_font_size;
        }
        if base.code_font_size != next.code_font_size {
            target.code_font_size = next.code_font_size;
        }
        if base.content_max_width != next.content_max_width {
            target.content_max_width = next.content_max_width;
        }
        if base.terminal_scrollback_lines != next.terminal_scrollback_lines {
            target.terminal_scrollback_lines = next.terminal_scrollback_lines;
        }
    }

    /// Leniently read settings from a parsed JSON document. Unknown fields are
    /// ignored so a document written by the app keeps loading here.
    pub fn from_value(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let content_max_width = object.get("contentMaxWidth").and_then(|value| {
            if value.is_null() {
                Some(None)
            } else {
                parse_clamped_font_size(value, MIN_CONTENT_MAX_WIDTH, MAX_CONTENT_MAX_WIDTH)
                    .map(Some)
            }
        });
        Self {
            theme: object
                .get("theme")
                .and_then(|value| value.as_str())
                .and_then(|theme| ThemePreference::parse(Some(theme)))
                .unwrap_or_default(),
            language: object
                .get("language")
                .and_then(|value| value.as_str())
                .and_then(Language::parse)
                .unwrap_or_default(),
            ui_font_family: object
                .get("uiFontFamily")
                .and_then(sanitize_font_family)
                .unwrap_or_default(),
            mono_font_family: object
                .get("monoFontFamily")
                .and_then(sanitize_font_family)
                .unwrap_or_default(),
            ui_base_font_size: read_i64(object, "uiBaseFontSize", DEFAULT_UI_BASE_FONT_SIZE)
                .clamp(MIN_UI_BASE_FONT_SIZE, MAX_UI_BASE_FONT_SIZE),
            content_font_size: read_i64(object, "contentFontSize", DEFAULT_CONTENT_FONT_SIZE)
                .clamp(MIN_CONTENT_FONT_SIZE, MAX_CONTENT_FONT_SIZE),
            code_font_size: read_i64(object, "codeFontSize", DEFAULT_CODE_FONT_SIZE)
                .clamp(MIN_CODE_FONT_SIZE, MAX_CODE_FONT_SIZE),
            content_max_width: content_max_width.unwrap_or(None),
            terminal_scrollback_lines: read_i64(
                object,
                "terminalScrollbackLines",
                DEFAULT_TERMINAL_SCROLLBACK_LINES,
            )
            .clamp(MIN_TERMINAL_SCROLLBACK_LINES, MAX_TERMINAL_SCROLLBACK_LINES),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "theme": self.theme.as_str(),
            "language": self.language.as_str(),
            "uiFontFamily": self.ui_font_family,
            "monoFontFamily": self.mono_font_family,
            "uiBaseFontSize": self.ui_base_font_size,
            "contentFontSize": self.content_font_size,
            "codeFontSize": self.code_font_size,
            "contentMaxWidth": self.content_max_width,
            "terminalScrollbackLines": self.terminal_scrollback_lines,
        })
    }
}

/// Serializes whole-document writes inside one process. Native windows each own
/// a store over the same path; the lock keeps their read-merge-write windows
/// from interleaving even though the rename itself is atomic.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Owns one settings document on disk. Loads and stores are whole-document,
/// so callers never read-merge-write a partially persisted file.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Platform config location for native client preferences:
    /// `%APPDATA%\paseo-desktop`, `~/Library/Application Support/paseo-desktop`,
    /// or `$XDG_CONFIG_HOME/paseo-desktop` (default `~/.config`).
    pub fn default_path() -> Result<PathBuf> {
        let base = if cfg!(target_os = "windows") {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .context("APPDATA is not set")
        } else if cfg!(target_os = "macos") {
            std::env::var_os("HOME")
                .map(|home| {
                    PathBuf::from(home)
                        .join("Library")
                        .join("Application Support")
                })
                .context("HOME is not set")
        } else {
            match std::env::var_os("XDG_CONFIG_HOME") {
                Some(config) if Path::new(&config).is_absolute() => Ok(PathBuf::from(config)),
                _ => std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".config"))
                    .context("HOME is not set"),
            }
        }?;
        Ok(base.join("paseo-desktop").join("settings.json"))
    }

    /// Missing file yields defaults; unreadable or corrupt documents are
    /// errors the caller must surface.
    pub fn load(&self) -> Result<Settings> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => {
                return Err(error).context("read desktop settings");
            }
        };
        let value: Value =
            serde_json::from_str(raw.trim()).context("desktop settings are not valid JSON")?;
        Ok(Settings::from_value(&value))
    }

    /// Merge a single window's edit into the document currently on disk. The
    /// newest on-disk document wins for every field the edit did not touch, so
    /// two windows editing different settings both keep their change. A corrupt
    /// document is an error: the edit never replaces an unreadable file.
    pub fn update(&self, base: &Settings, next: &Settings) -> Result<Settings> {
        let _guard = WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut merged = self.load()?;
        Settings::merge_delta(base, next, &mut merged);
        self.write_locked(&merged)?;
        Ok(merged)
    }

    /// Write atomically: serialize into a unique temp file in the destination
    /// directory, fsync it, then rename over the target. The unique name keeps
    /// concurrent writers from sharing a temp path, and the failed write is
    /// removed instead of being left behind.
    pub fn store(&self, settings: &Settings) -> Result<()> {
        let _guard = WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.write_locked(settings)
    }

    fn write_locked(&self, settings: &Settings) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("desktop settings path has no parent directory")?;
        fs::create_dir_all(parent).context("create desktop settings directory")?;
        let mut document = serde_json::to_vec_pretty(&settings.to_value())
            .context("serialize desktop settings")?;
        document.push(b'\n');
        let temp = self.temp_path();
        let written = (|| {
            use std::io::Write as _;
            let mut file = fs::File::create(&temp).context("create desktop settings temp file")?;
            file.write_all(&document)
                .and_then(|_| file.flush())
                .and_then(|_| file.sync_all())
                .context("write desktop settings temp file")
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temp, &self.path) {
            let _ = fs::remove_file(&temp);
            return Err(error).context("replace desktop settings");
        }
        // The rename is durable once the directory entry is synced. Unix is the
        // only platform whose standard library exposes a directory handle.
        #[cfg(unix)]
        {
            let _ = fs::File::open(parent).and_then(|directory| directory.sync_all());
        }
        Ok(())
    }

    fn temp_path(&self) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "settings.json".into());
        self.path
            .with_file_name(format!(".{name}.{}.{counter}.tmp", std::process::id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_the_app_client_settings() {
        let settings = Settings::default();
        assert_eq!(settings.theme, ThemePreference::Auto);
        assert_eq!(settings.language, Language::System);
        assert_eq!(settings.ui_base_font_size, 14);
        assert_eq!(settings.content_font_size, 15);
        assert_eq!(settings.code_font_size, 12);
        assert_eq!(settings.terminal_scrollback_lines, 10_000);
        assert_eq!(settings.content_max_width, None);
        assert_eq!(settings.ui_font_family, "");
        assert_eq!(settings.mono_font_family, "");
    }

    #[test]
    fn reads_the_app_document_shape_and_ignores_unknown_fields() {
        let settings = Settings::from_value(&json!({
            "theme": "midnight",
            "language": "pt-BR",
            "uiFontFamily": "Inter",
            "monoFontFamily": "FiraCode Nerd Font",
            "uiBaseFontSize": 16,
            "contentFontSize": 17,
            "codeFontSize": 13,
            "contentMaxWidth": 960,
            "terminalScrollbackLines": 5000,
            "sendBehavior": "queue",
            "chatDisplay": "full"
        }));
        assert_eq!(settings.theme, ThemePreference::Midnight);
        assert_eq!(
            settings.language,
            Language::Explicit(crate::locales::Locale::PtBr)
        );
        assert_eq!(settings.ui_font_family, "Inter");
        assert_eq!(settings.mono_font_family, "FiraCode Nerd Font");
        assert_eq!(settings.ui_base_font_size, 16);
        assert_eq!(settings.content_font_size, 17);
        assert_eq!(settings.code_font_size, 13);
        assert_eq!(settings.content_max_width, Some(960));
        assert_eq!(settings.terminal_scrollback_lines, 5000);
    }

    #[test]
    fn clamps_sizes_and_scrollback_like_the_app() {
        let settings = Settings::from_value(&json!({
            "uiBaseFontSize": 99,
            "contentFontSize": 0,
            "codeFontSize": 8,
            "terminalScrollbackLines": 2_000_000,
            "contentMaxWidth": 100
        }));
        assert_eq!(settings.ui_base_font_size, MAX_UI_BASE_FONT_SIZE);
        assert_eq!(settings.content_font_size, MIN_CONTENT_FONT_SIZE);
        assert_eq!(settings.code_font_size, MIN_CODE_FONT_SIZE);
        assert_eq!(
            settings.terminal_scrollback_lines,
            MAX_TERMINAL_SCROLLBACK_LINES
        );
        assert_eq!(settings.content_max_width, Some(MIN_CONTENT_MAX_WIDTH));
    }

    #[test]
    fn contributed_plugin_theme_falls_back_to_auto() {
        assert_eq!(
            Settings::from_value(&json!({"theme": "plugin"})).theme,
            ThemePreference::Auto
        );
        assert_eq!(
            Settings::from_value(&json!({"theme": "not-a-theme"})).theme,
            ThemePreference::Auto
        );
    }

    #[test]
    fn unknown_language_falls_back_to_system_and_roundtrips() {
        let settings = Settings::from_value(&json!({"language": "de"}));
        assert_eq!(settings.language, Language::System);
        assert_eq!(
            serde_json::to_value(&settings.to_value()["language"]).unwrap(),
            json!("system")
        );
        let mut explicit = Settings::default();
        explicit.language = crate::locales::Language::Explicit(crate::locales::Locale::ZhCn);
        let roundtrip = Settings::from_value(&explicit.to_value());
        assert_eq!(roundtrip, explicit);
    }

    #[test]
    fn font_family_sanitizing_matches_the_app() {
        let ok = |value: &str| sanitize_font_family(&json!(value));
        assert_eq!(ok(" JetBrains Mono "), Some("JetBrains Mono".into()));
        assert_eq!(ok(""), Some(String::new()));
        assert_eq!(ok("42"), Some("42".into()));
        assert_eq!(ok("a;drop"), None);
        assert_eq!(ok(&"a".repeat(201)), None);
        assert_eq!(ok("'JetBrains'"), Some("'JetBrains'".into()));
        assert_eq!(ok("'JetBrains"), None);
        assert_eq!(ok("url(x)"), None);
        assert_eq!(ok("back\u{5c}"), None);
        assert_eq!(ok("a\u{5c}\"b\"\u{5c}"), None);
    }

    #[test]
    fn missing_file_yields_defaults_and_roundtrip_preserves_values() {
        let path = std::env::temp_dir().join(format!(
            "paseo-native-settings-{}-settings.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.load().unwrap(), Settings::default());
        let mut settings = Settings::default();
        settings.theme = ThemePreference::PureBlack;
        settings.terminal_scrollback_lines = 250_000;
        settings.content_max_width = Some(1200);
        store.store(&settings).unwrap();
        assert_eq!(store.load().unwrap(), settings);
        let temp_prefix = format!(
            ".{}.{}.",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        );
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(&temp_prefix) && name.ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0, "a committed write leaves no temp file");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn update_merges_edits_from_two_windows_over_one_document() {
        let path = std::env::temp_dir().join(format!(
            "paseo-native-settings-{}-merge.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let first = SettingsStore::new(path.clone());
        let second = SettingsStore::new(path.clone());
        let base = Settings::default();
        first.store(&base).unwrap();

        let mut theme_edit = base.clone();
        theme_edit.theme = ThemePreference::Midnight;
        let mut size_edit = base.clone();
        size_edit.code_font_size = 16;

        let after_first = first.update(&base, &theme_edit).unwrap();
        assert_eq!(after_first.theme, ThemePreference::Midnight);
        let after_second = second.update(&base, &size_edit).unwrap();
        assert_eq!(after_second.theme, ThemePreference::Midnight);
        assert_eq!(after_second.code_font_size, 16);
        assert_eq!(first.load().unwrap(), after_second);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn update_refuses_to_replace_a_corrupt_document() {
        let path = std::env::temp_dir().join(format!(
            "paseo-native-settings-{}-merge-corrupt.json",
            std::process::id()
        ));
        fs::write(&path, "{not json").unwrap();
        let store = SettingsStore::new(path.clone());
        let base = Settings::default();
        let mut next = base.clone();
        next.theme = ThemePreference::Light;
        assert!(store.update(&base, &next).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn corrupt_document_is_an_error_not_silent_defaults() {
        let path = std::env::temp_dir().join(format!(
            "paseo-native-settings-{}-corrupt.json",
            std::process::id()
        ));
        fs::write(&path, "{not json").unwrap();
        let store = SettingsStore::new(path.clone());
        assert!(store.load().is_err());
        // Storing again recovers the file.
        store.store(&Settings::default()).unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_document_written_by_the_app_keeps_loading() {
        let path = std::env::temp_dir().join(format!(
            "paseo-native-settings-{}-shared.json",
            std::process::id()
        ));
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "theme": "ghostty",
                "codeFontSize": 14,
                "sidebarRowItems": {"branch": true},
                "usage": {"mode": "session"}
            }))
            .unwrap(),
        )
        .unwrap();
        let store = SettingsStore::new(path.clone());
        let settings = store.load().unwrap();
        assert_eq!(settings.theme, ThemePreference::Ghostty);
        assert_eq!(settings.code_font_size, 14);
        fs::remove_file(&path).unwrap();
    }
}
