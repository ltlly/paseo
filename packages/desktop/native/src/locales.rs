//! Native locale runtime for the GPUI desktop settings surface.
//!
//! The authored app translation resources
//! (`packages/app/src/i18n/resources`) stay the source of truth; the
//! `sync:desktop:native-ui` generator flattens them into the dotted-key JSON
//! catalogs embedded below. Lookup semantics follow the app's i18next
//! configuration (`compatibilityJSON: "v4"`, English fallback, named
//! `{{var}}` interpolation, and the forge `context` mechanism). Plural
//! handling covers integer counts only, which is all the migrated settings
//! copy uses; fractional CLDR forms are out of scope.
//!
//! - candidates for one language resolve as `key_context_category`,
//!   `key_context`, `key_category`, `key`;
//! - the integer plural category is computed per language, so the English
//!   fallback applies English rules (`ar` count 3 falls back to the English
//!   `_other` string, exactly like i18next);
//! - interpolation replaces only the named parameters that are present and
//!   leaves unknown `{{placeholders}}` untouched.

use std::collections::HashMap;
use std::sync::OnceLock;

mod catalogs {
    pub const EN: &str = include_str!("locales/en.json");
    pub const AR: &str = include_str!("locales/ar.json");
    pub const ES: &str = include_str!("locales/es.json");
    pub const FR: &str = include_str!("locales/fr.json");
    pub const JA: &str = include_str!("locales/ja.json");
    pub const KO: &str = include_str!("locales/ko.json");
    pub const PT_BR: &str = include_str!("locales/pt-BR.json");
    pub const RU: &str = include_str!("locales/ru.json");
    pub const ZH_CN: &str = include_str!("locales/zh-CN.json");
}

/// The locales the app ships resources for (`docs/i18n.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Locale {
    Ar,
    En,
    Es,
    Fr,
    Ja,
    Ko,
    PtBr,
    Ru,
    ZhCn,
}

impl Locale {
    pub const ALL: [Locale; 9] = [
        Locale::Ar,
        Locale::En,
        Locale::Es,
        Locale::Fr,
        Locale::Ja,
        Locale::Ko,
        Locale::PtBr,
        Locale::Ru,
        Locale::ZhCn,
    ];

    /// The persisted app language value (`docs/i18n.md`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ar => "ar",
            Self::En => "en",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::PtBr => "pt-BR",
            Self::Ru => "ru",
            Self::ZhCn => "zh-CN",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ar" => Self::Ar,
            "en" => Self::En,
            "es" => Self::Es,
            "fr" => Self::Fr,
            "ja" => Self::Ja,
            "ko" => Self::Ko,
            "pt-BR" => Self::PtBr,
            "ru" => Self::Ru,
            "zh-CN" => Self::ZhCn,
            _ => return None,
        })
    }

    fn catalog(self) -> &'static HashMap<String, String> {
        static CATALOGS: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
        let catalogs = CATALOGS.get_or_init(|| {
            [
                catalogs::AR,
                catalogs::EN,
                catalogs::ES,
                catalogs::FR,
                catalogs::JA,
                catalogs::KO,
                catalogs::PT_BR,
                catalogs::RU,
                catalogs::ZH_CN,
            ]
            .into_iter()
            .map(|source| serde_json::from_str(source).expect("embedded locale catalog is valid"))
            .collect()
        });
        &catalogs[match self {
            Self::Ar => 0,
            Self::En => 1,
            Self::Es => 2,
            Self::Fr => 3,
            Self::Ja => 4,
            Self::Ko => 5,
            Self::PtBr => 6,
            Self::Ru => 7,
            Self::ZhCn => 8,
        }]
    }
}

/// The persisted app language setting: follow the system locale or an explicit
/// locale (`AppLanguage` in `packages/app/src/i18n/locales.ts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    System,
    Explicit(Locale),
}

impl Language {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Explicit(locale) => locale.as_str(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            other => Locale::parse(other).map(Self::Explicit),
        }
    }
}

/// The platform's locale preferences in descending order, lowercased like the
/// app's mapping. The whole list is kept because `resolveSupportedLocale`
/// skips preferences the app has no resources for.
pub fn system_locales() -> &'static [String] {
    static SYSTEM: OnceLock<Vec<String>> = OnceLock::new();
    SYSTEM.get_or_init(|| normalize_system_locales(sys_locale::get_locales()))
}

/// Lowercase the platform preference list without dropping entries.
fn normalize_system_locales(locales: impl Iterator<Item = String>) -> Vec<String> {
    locales.map(|locale| locale.to_lowercase()).collect()
}

/// Port of `resolveSupportedLocale` in `packages/app/src/i18n/locales.ts`:
/// bare languages map directly, `pt`/`pt-BR` map to `pt-BR`, `zh`/`zh-CN`/
/// `zh-Hans*` map to `zh-CN`, and everything else falls back to English.
pub fn resolve_supported_locale(language: Language, system_locales: &[String]) -> Locale {
    let Language::Explicit(locale) = language else {
        for locale in system_locales {
            let normalized = locale.to_lowercase();
            let base = normalized.split('-').next().unwrap_or("");
            let regional = match base {
                "ar" => Some(Locale::Ar),
                "en" => Some(Locale::En),
                "es" => Some(Locale::Es),
                "fr" => Some(Locale::Fr),
                "ja" => Some(Locale::Ja),
                "ko" => Some(Locale::Ko),
                "ru" => Some(Locale::Ru),
                _ => None,
            };
            if let Some(locale) = regional {
                return locale;
            }
            if normalized == "pt" || normalized == "pt-br" {
                return Locale::PtBr;
            }
            if normalized == "zh" || normalized == "zh-cn" || normalized.starts_with("zh-hans") {
                return Locale::ZhCn;
            }
        }
        return Locale::En;
    };
    locale
}

/// The locale the client renders in: explicit choices win, otherwise the
/// platform preference list is mapped onto a supported resource.
pub fn active_locale(language: Language) -> Locale {
    active_locale_in(language, system_locales())
}

/// Resolution entrance for a given platform preference list.
fn active_locale_in(language: Language, system_locales: &[String]) -> Locale {
    resolve_supported_locale(language, system_locales)
}

/// Plural category for integer counts, matching i18next's v4 plural rules for
/// the counts the settings surface renders. Fractional counts are unsupported.
pub fn plural_category(locale: Locale, count: i64) -> &'static str {
    let n = count.unsigned_abs();
    match locale {
        Locale::Ja | Locale::Ko | Locale::ZhCn => "other",
        Locale::En => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
        Locale::Es => {
            if n == 1 {
                "one"
            } else if n >= 1_000_000 && n % 1_000_000 == 0 {
                "many"
            } else {
                "other"
            }
        }
        Locale::Fr | Locale::PtBr => {
            if n <= 1 {
                "one"
            } else if n >= 1_000_000 && n % 1_000_000 == 0 {
                "many"
            } else {
                "other"
            }
        }
        Locale::Ru => {
            if n % 10 == 1 && n % 100 != 11 {
                "one"
            } else if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {
                "few"
            } else {
                "many"
            }
        }
        Locale::Ar => {
            if n == 0 {
                "zero"
            } else if n == 1 {
                "one"
            } else if n == 2 {
                "two"
            } else if (3..=10).contains(&(n % 100)) {
                "few"
            } else if (11..=99).contains(&(n % 100)) {
                "many"
            } else {
                "other"
            }
        }
    }
}

/// A translation request: `translate` resolves `key` with the forge `context`
/// mechanism, integer plural handling for `count`, and `{{named}}` parameters.
/// `count` is also available to the template as `{{count}}`.
#[derive(Default)]
pub struct Tx<'a> {
    pub key: &'a str,
    pub context: Option<&'a str>,
    pub count: Option<i64>,
    pub args: &'a [(&'a str, &'a str)],
}

impl<'a> Tx<'a> {
    pub fn key(key: &'a str) -> Self {
        Self {
            key,
            context: None,
            count: None,
            args: &[],
        }
    }

    pub const fn context(mut self, context: &'a str) -> Self {
        self.context = Some(context);
        self
    }

    pub const fn count(mut self, count: i64) -> Self {
        self.count = Some(count);
        self
    }

    pub const fn args(mut self, args: &'a [(&'a str, &'a str)]) -> Self {
        self.args = args;
        self
    }
}

/// Translate `key` for `locale`, falling back to the English catalog and then
/// to the key itself, exactly like the app's i18next fallback chain.
pub fn translate(locale: Locale, tx: Tx) -> String {
    let category = tx.count.map(|count| plural_category(locale, count));
    let english_category = tx.count.map(|count| plural_category(Locale::En, count));
    for (catalog, category) in [
        (locale.catalog(), category),
        (Locale::En.catalog(), english_category),
    ] {
        let mut candidates: [Option<String>; 4] = [const { None }; 4];
        let mut context_key = tx.key.to_string();
        if let Some(context) = tx.context {
            context_key.push('_');
            context_key.push_str(context);
            if let Some(category) = category {
                candidates[0] = Some(format!("{context_key}_{category}"));
            }
            candidates[1] = Some(context_key);
        }
        if let Some(category) = category {
            candidates[2] = Some(format!("{}_{}", tx.key, category));
        }
        candidates[3] = Some(tx.key.to_owned());
        for candidate in candidates.into_iter().flatten() {
            if let Some(template) = catalog.get(&candidate) {
                return interpolate(template, &tx);
            }
        }
    }
    tx.key.to_owned()
}

/// Replace `{{name}}` tokens that have a matching parameter; unknown
/// placeholders stay in the output like i18next leaves them.
fn interpolate(template: &str, tx: &Tx) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                let value = if name == "count" {
                    tx.count.map(|count| count.to_string())
                } else {
                    tx.args
                        .iter()
                        .copied()
                        .find(|(arg, _)| *arg == name)
                        .map(|(_, value)| value.to_owned())
                };
                match value {
                    Some(value) => out.push_str(&value),
                    None => out.push_str(&rest[start..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Port of `LANGUAGE_NATIVE_NAMES` and `LANGUAGE_NAMES_BY_LOCALE` from
/// `packages/app/src/i18n/locales.ts`: each language's own name and its name
/// in every UI locale.
const NATIVE_NAMES: [(Locale, &str); 9] = [
    (Locale::Ar, "العربية"),
    (Locale::En, "English"),
    (Locale::Es, "Español"),
    (Locale::Fr, "Français"),
    (Locale::Ja, "日本語"),
    (Locale::Ko, "한국어"),
    (Locale::PtBr, "Português brasileiro"),
    (Locale::Ru, "Русский"),
    (Locale::ZhCn, "简体中文"),
];
const NAMES_BY_UI_LOCALE: [(Locale, [(Locale, &str); 9]); 9] = [
    (
        Locale::Ar,
        [
            (Locale::Ar, "العربية"),
            (Locale::En, "الإنجليزية"),
            (Locale::Es, "الإسبانية"),
            (Locale::Fr, "الفرنسية"),
            (Locale::Ja, "اليابانية"),
            (Locale::Ko, "الكورية"),
            (Locale::PtBr, "البرتغالية البرازيلية"),
            (Locale::Ru, "الروسية"),
            (Locale::ZhCn, "الصينية المبسطة"),
        ],
    ),
    (
        Locale::En,
        [
            (Locale::Ar, "Arabic"),
            (Locale::En, "English"),
            (Locale::Es, "Spanish"),
            (Locale::Fr, "French"),
            (Locale::Ja, "Japanese"),
            (Locale::Ko, "Korean"),
            (Locale::PtBr, "Brazilian Portuguese"),
            (Locale::Ru, "Russian"),
            (Locale::ZhCn, "Simplified Chinese"),
        ],
    ),
    (
        Locale::Es,
        [
            (Locale::Ar, "árabe"),
            (Locale::En, "inglés"),
            (Locale::Es, "español"),
            (Locale::Fr, "francés"),
            (Locale::Ja, "japonés"),
            (Locale::Ko, "coreano"),
            (Locale::PtBr, "portugués brasileño"),
            (Locale::Ru, "ruso"),
            (Locale::ZhCn, "chino simplificado"),
        ],
    ),
    (
        Locale::Fr,
        [
            (Locale::Ar, "arabe"),
            (Locale::En, "anglais"),
            (Locale::Es, "espagnol"),
            (Locale::Fr, "français"),
            (Locale::Ja, "japonais"),
            (Locale::Ko, "coréen"),
            (Locale::PtBr, "portugais brésilien"),
            (Locale::Ru, "russe"),
            (Locale::ZhCn, "chinois simplifié"),
        ],
    ),
    (
        Locale::Ja,
        [
            (Locale::Ar, "アラビア語"),
            (Locale::En, "英語"),
            (Locale::Es, "スペイン語"),
            (Locale::Fr, "フランス語"),
            (Locale::Ja, "日本語"),
            (Locale::Ko, "韓国語"),
            (Locale::PtBr, "ブラジルポルトガル語"),
            (Locale::Ru, "ロシア語"),
            (Locale::ZhCn, "簡体字中国語"),
        ],
    ),
    (
        Locale::Ko,
        [
            (Locale::Ar, "아랍어"),
            (Locale::En, "영어"),
            (Locale::Es, "스페인어"),
            (Locale::Fr, "프랑스어"),
            (Locale::Ja, "일본어"),
            (Locale::Ko, "한국어"),
            (Locale::PtBr, "브라질 포르투갈어"),
            (Locale::Ru, "러시아어"),
            (Locale::ZhCn, "중국어 간체"),
        ],
    ),
    (
        Locale::PtBr,
        [
            (Locale::Ar, "árabe"),
            (Locale::En, "inglês"),
            (Locale::Es, "espanhol"),
            (Locale::Fr, "francês"),
            (Locale::Ja, "japonês"),
            (Locale::Ko, "coreano"),
            (Locale::PtBr, "Português brasileiro"),
            (Locale::Ru, "russo"),
            (Locale::ZhCn, "chinês simplificado"),
        ],
    ),
    (
        Locale::Ru,
        [
            (Locale::Ar, "арабский"),
            (Locale::En, "английский"),
            (Locale::Es, "испанский"),
            (Locale::Fr, "французский"),
            (Locale::Ja, "японский"),
            (Locale::Ko, "корейский"),
            (Locale::PtBr, "бразильский португальский"),
            (Locale::Ru, "русский"),
            (Locale::ZhCn, "упрощенный китайский"),
        ],
    ),
    (
        Locale::ZhCn,
        [
            (Locale::Ar, "阿拉伯语"),
            (Locale::En, "英语"),
            (Locale::Es, "西班牙语"),
            (Locale::Fr, "法语"),
            (Locale::Ja, "日语"),
            (Locale::Ko, "韩语"),
            (Locale::PtBr, "巴西葡萄牙语"),
            (Locale::Ru, "俄语"),
            (Locale::ZhCn, "简体中文"),
        ],
    ),
];

/// Port of `formatLanguageOptionLabel` in `packages/app/src/i18n/locales.ts`:
/// the native name plus the active-locale name, collapsed to one label when
/// they match.
pub fn language_option_label(option: Locale, active: Locale) -> String {
    let native = NATIVE_NAMES
        .iter()
        .find(|(locale, _)| *locale == option)
        .map(|(_, name)| *name)
        .unwrap_or_default();
    let active_name = NAMES_BY_UI_LOCALE
        .iter()
        .find(|(locale, _)| *locale == active)
        .and_then(|(_, names)| names.iter().find(|(locale, _)| *locale == option))
        .map(|(_, name)| *name)
        .unwrap_or_default();
    if native == active_name {
        native.to_owned()
    } else {
        format!("{native} - {active_name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(locale: Locale, key: &str) -> String {
        translate(locale, Tx::key(key))
    }

    #[test]
    fn dotted_keys_resolve_from_the_embedded_catalogs() {
        assert_eq!(t(Locale::En, "common.actions.retry"), "Retry");
        assert_eq!(t(Locale::En, "settings.general.language.label"), "Language");
        assert_eq!(t(Locale::Fr, "common.back"), "Retour");
        assert_eq!(t(Locale::Ja, "common.actions.copy"), "コピー");
    }

    #[test]
    fn missing_keys_return_the_key_itself() {
        assert_eq!(
            t(Locale::En, "definitely.not.a.key"),
            "definitely.not.a.key"
        );
        assert_eq!(t(Locale::Ru, "also.missing"), "also.missing");
    }

    #[test]
    fn interpolation_replaces_named_parameters_and_keeps_unknown_ones() {
        assert_eq!(
            translate(
                Locale::En,
                Tx {
                    key: "common.states.copiedLabel",
                    args: &[("label", "Branch name")],
                    ..Tx::key("")
                }
            ),
            "Copied Branch name"
        );
        // i18next leaves an unmatched placeholder in the output.
        assert_eq!(
            translate(Locale::En, Tx::key("message.dictation.failed")),
            "Dictation failed: {{error}}"
        );
        assert_eq!(
            translate(
                Locale::En,
                Tx {
                    key: "message.dictation.failed",
                    args: &[("error", "no mic")],
                    ..Tx::key("")
                }
            ),
            "Dictation failed: no mic"
        );
    }

    #[test]
    fn forge_context_falls_back_to_the_base_key() {
        assert_eq!(
            translate(
                Locale::En,
                Tx::key("composer.attachments.addIssueOrPr").context("mr")
            ),
            "Add issue or MR"
        );
        assert_eq!(
            translate(Locale::En, Tx::key("composer.attachments.addIssueOrPr")),
            "Add issue or PR"
        );
        assert_eq!(
            translate(
                Locale::Fr,
                Tx::key("composer.attachments.addIssueOrPr").context("unknown")
            ),
            translate(Locale::Fr, Tx::key("composer.attachments.addIssueOrPr"))
        );
    }

    #[test]
    fn plural_categories_match_integer_counts() {
        let cases: &[(Locale, i64, &str)] = &[
            (Locale::En, 0, "other"),
            (Locale::En, 1, "one"),
            (Locale::En, 2, "other"),
            (Locale::Es, 0, "other"),
            (Locale::Es, 1, "one"),
            (Locale::Es, 1_000_000, "many"),
            (Locale::Es, 1_000_001, "other"),
            (Locale::Fr, 0, "one"),
            (Locale::Fr, 1, "one"),
            (Locale::Fr, 2, "other"),
            (Locale::Fr, 1_000_000, "many"),
            (Locale::PtBr, 0, "one"),
            (Locale::PtBr, 1, "one"),
            (Locale::PtBr, 2, "other"),
            (Locale::Ru, 1, "one"),
            (Locale::Ru, 2, "few"),
            (Locale::Ru, 5, "many"),
            (Locale::Ru, 11, "many"),
            (Locale::Ru, 21, "one"),
            (Locale::Ru, 22, "few"),
            (Locale::Ru, 25, "many"),
            (Locale::Ru, 101, "one"),
            (Locale::Ar, 0, "zero"),
            (Locale::Ar, 1, "one"),
            (Locale::Ar, 2, "two"),
            (Locale::Ar, 3, "few"),
            (Locale::Ar, 11, "many"),
            (Locale::Ar, 100, "other"),
            (Locale::Ar, 103, "few"),
            (Locale::Ar, 111, "many"),
            (Locale::Ja, 1, "other"),
            (Locale::Ko, 1, "other"),
            (Locale::ZhCn, 1, "other"),
        ];
        for (locale, count, category) in cases {
            assert_eq!(
                plural_category(*locale, *count),
                *category,
                "{locale:?} count {count}"
            );
        }
    }

    #[test]
    fn plural_lookup_uses_the_language_own_category_with_english_fallback() {
        // Arabic count 3 is "few"; the Arabic catalog only ships one/other
        // forms, so i18next-style fallback continues into English where the
        // count resolves with English rules to `_other`.
        assert_eq!(
            translate(
                Locale::Ar,
                Tx::key("workspaceLabels.manage.deleteMessage").count(3)
            ),
            "This removes the label from 3 workspaces on this host."
        );
        assert_eq!(
            translate(
                Locale::Ar,
                Tx::key("workspaceLabels.manage.deleteMessage").count(1)
            ),
            "سيؤدي هذا إلى إزالة التسمية من 1 مساحة عمل على هذا المضيف."
        );
        // A base key without suffixed forms serves every count.
        assert_eq!(
            translate(
                Locale::Ar,
                Tx::key("modelSelector.modelCountPlural").count(3)
            ),
            "3 نماذج"
        );
        assert_eq!(
            translate(
                Locale::En,
                Tx::key("modelSelector.modelCountPlural").count(5)
            ),
            "5 models"
        );
    }

    #[test]
    fn count_is_available_as_the_count_parameter() {
        assert_eq!(
            translate(Locale::En, Tx::key("agentList.badges.pending").count(4)),
            "4 pending"
        );
    }

    #[test]
    fn language_parses_the_persisted_wire_values() {
        for locale in Locale::ALL {
            assert_eq!(
                Language::parse(locale.as_str()),
                Some(Language::Explicit(locale))
            );
            assert_eq!(Language::Explicit(locale).as_str(), locale.as_str());
        }
        assert_eq!(Language::parse("system"), Some(Language::System));
        assert_eq!(Language::System.as_str(), "system");
        assert_eq!(Language::parse("de"), None);
        assert_eq!(Language::parse(""), None);
    }

    #[test]
    fn system_locale_mapping_matches_the_app() {
        let resolve = |locales: &[&str]| {
            resolve_supported_locale(
                Language::System,
                &locales
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(resolve(&["ar-EG"]), Locale::Ar);
        assert_eq!(resolve(&["en-US"]), Locale::En);
        assert_eq!(resolve(&["es-MX"]), Locale::Es);
        assert_eq!(resolve(&["fr-CA"]), Locale::Fr);
        assert_eq!(resolve(&["ja-JP"]), Locale::Ja);
        assert_eq!(resolve(&["ko", "ko-KR"]), Locale::Ko);
        assert_eq!(resolve(&["pt", "pt-BR"]), Locale::PtBr);
        assert_eq!(resolve(&["ru-RU"]), Locale::Ru);
        assert_eq!(resolve(&["pt-PT"]), Locale::En);
        assert_eq!(resolve(&["pt-AO"]), Locale::En);
        assert_eq!(resolve(&["zh"]), Locale::ZhCn);
        assert_eq!(resolve(&["zh-CN"]), Locale::ZhCn);
        assert_eq!(resolve(&["zh-Hans-US"]), Locale::ZhCn);
        assert_eq!(resolve(&["zh-TW"]), Locale::En);
        assert_eq!(resolve(&["zh-Hant"]), Locale::En);
        assert_eq!(resolve(&["zh-HK"]), Locale::En);
        assert_eq!(resolve(&["de-DE"]), Locale::En);
        assert_eq!(resolve(&[]), Locale::En);
        // The first supported system locale wins.
        assert_eq!(resolve(&["en-US", "es-GB"]), Locale::En);
        // Case is normalized like the app's lowercased comparison.
        assert_eq!(resolve(&["PT-br"]), Locale::PtBr);
    }

    #[test]
    fn cached_system_preferences_reach_later_supported_locales() {
        // A Linux `LANGUAGE=de:fr` list and a macOS preference list with
        // German ahead of French both start with a language the app has no
        // resources for. The entrance keeps every preference, so the resolver
        // lands on French instead of defaulting to English off the first entry.
        let platform = normalize_system_locales(["de-DE", "fr-FR"].into_iter().map(str::to_owned));
        assert_eq!(platform, ["de-de", "fr-fr"]);
        assert_eq!(active_locale_in(Language::System, &platform), Locale::Fr);
        assert_eq!(
            active_locale_in(Language::Explicit(Locale::Ja), &platform),
            Locale::Ja
        );
    }

    #[test]
    fn explicit_languages_ignore_the_system_locale() {
        assert_eq!(
            resolve_supported_locale(Language::Explicit(Locale::Ko), &["en-US".to_owned()]),
            Locale::Ko
        );
    }

    #[test]
    fn resolution_matches_i18next_for_sampled_requests() {
        // Expected values captured from the real i18next instance configured
        // like the app (`packages/app/src/i18n/i18next.ts`) over the same
        // generated catalogs.
        let cases: &[(
            Locale,
            &str,
            Option<&str>,
            Option<i64>,
            &[(&str, &str)],
            &str,
        )] = &[
            (
                Locale::En,
                "composer.attachments.addIssueOrPr",
                None,
                None,
                &[],
                "Add issue or PR",
            ),
            (
                Locale::En,
                "composer.attachments.addIssueOrPr",
                Some("mr"),
                None,
                &[],
                "Add issue or MR",
            ),
            (
                Locale::Ar,
                "composer.attachments.addIssueOrPr",
                Some("mr"),
                None,
                &[],
                "أضف مشكلة أو MR",
            ),
            (
                Locale::Ru,
                "workspaceLabels.manage.deleteMessage",
                None,
                Some(21),
                &[],
                "Метка будет снята с 21 рабочего пространства на этом хосте.",
            ),
            (
                Locale::Ru,
                "workspaceLabels.manage.deleteMessage",
                None,
                Some(22),
                &[],
                "This removes the label from 22 workspaces on this host.",
            ),
            (
                Locale::PtBr,
                "workspaceLabels.manage.deleteMessage",
                None,
                Some(0),
                &[],
                "Isso remove a etiqueta de 0 workspace neste host.",
            ),
            (
                Locale::Fr,
                "workspaceLabels.manage.deleteMessage",
                None,
                Some(2),
                &[],
                "Cela retire l’étiquette de 2 espaces de travail sur cet hôte.",
            ),
            (
                Locale::Es,
                "modelSelector.modelCountPlural",
                None,
                Some(1),
                &[],
                "1 modelos",
            ),
            (
                Locale::ZhCn,
                "settings.general.language.options.zhCN",
                None,
                None,
                &[],
                "简体中文",
            ),
            (
                Locale::Ja,
                "workspace.git.pr.approvals",
                None,
                None,
                &[("given", "2"), ("required", "3")],
                "2 / 3 承認",
            ),
            (
                Locale::Ko,
                "common.states.copiedLabel",
                None,
                None,
                &[("label", "x")],
                "x 복사됨",
            ),
            (
                Locale::Ar,
                "agentList.badges.pending",
                None,
                Some(11),
                &[],
                "11معلق",
            ),
        ];
        for (locale, key, context, count, args, expected) in cases {
            let actual = translate(
                *locale,
                Tx {
                    key,
                    context: *context,
                    count: *count,
                    args,
                },
            );
            assert_eq!(actual, *expected, "{locale:?} {key} {count:?}");
        }
    }

    #[test]
    fn language_option_labels_match_the_app() {
        assert_eq!(
            language_option_label(Locale::Ar, Locale::En),
            "العربية - Arabic"
        );
        assert_eq!(
            language_option_label(Locale::Ja, Locale::En),
            "日本語 - Japanese"
        );
        assert_eq!(
            language_option_label(Locale::PtBr, Locale::En),
            "Português brasileiro - Brazilian Portuguese"
        );
        assert_eq!(
            language_option_label(Locale::ZhCn, Locale::ZhCn),
            "简体中文"
        );
        assert_eq!(
            language_option_label(Locale::Ar, Locale::ZhCn),
            "العربية - 阿拉伯语"
        );
        assert_eq!(language_option_label(Locale::Ja, Locale::Ja), "日本語");
        // UI locales whose translated name differs from the language's own
        // name in the app table (`packages/app/src/i18n/locales.ts`).
        assert_eq!(
            language_option_label(Locale::Fr, Locale::PtBr),
            "Français - francês"
        );
        assert_eq!(
            language_option_label(Locale::Es, Locale::Ru),
            "Español - испанский"
        );
        assert_eq!(
            language_option_label(Locale::Fr, Locale::Ru),
            "Français - французский"
        );
        assert_eq!(
            language_option_label(Locale::Ru, Locale::ZhCn),
            "Русский - 俄语"
        );
    }

    #[test]
    fn catalogs_parse_lazily_and_are_shared() {
        assert!(!Locale::En.catalog().is_empty());
        assert_eq!(Locale::En.catalog().len(), Locale::En.catalog().len());
        assert_eq!(Locale::ALL.len(), 9);
    }
}
