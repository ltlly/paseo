//! Authored theme palettes for the native client, ported from the app's
//! `packages/app/src/styles/theme.ts` theme builders. The generated
//! `theme.rs` in the application crate stays the byte-exact sync of the
//! default dark theme; this module resolves every authored theme at runtime
//! from the same tint tables so theme selection can switch without a
//! regeneration step. If the app palettes change, both files change together.
//!
//! Terminal accessors consume [`Palette::terminal`]; the module only supplies
//! resolved values and owns no terminal state.

use crate::settings::ThemePreference;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalPalette {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub cursor_accent: u32,
    /// Selection background includes its alpha byte, like the generated token.
    pub selection_background: u32,
    pub selection_foreground: u32,
    pub ansi: [u32; 16],
}

/// One resolved theme. Field names mirror the generated `theme::colors`
/// tokens one-to-one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub is_dark: bool,
    pub surface0: u32,
    pub surface1: u32,
    pub surface2: u32,
    pub surface3: u32,
    pub surface4: u32,
    pub surface_diff_empty: u32,
    pub surface_sidebar: u32,
    pub surface_sidebar_hover: u32,
    pub surface_sidebar_selected: u32,
    pub surface_workspace: u32,
    pub foreground: u32,
    pub foreground_muted: u32,
    pub foreground_extra_muted: u32,
    pub border: u32,
    pub border_accent: u32,
    pub accent: u32,
    pub accent_bright: u32,
    pub accent_foreground: u32,
    pub destructive: u32,
    pub destructive_foreground: u32,
    pub success: u32,
    pub success_foreground: u32,
    pub background: u32,
    pub popover: u32,
    pub popover_foreground: u32,
    pub primary: u32,
    pub primary_foreground: u32,
    pub secondary: u32,
    pub secondary_foreground: u32,
    pub muted: u32,
    pub muted_foreground: u32,
    pub accent_border: u32,
    pub input: u32,
    pub ring: u32,
    pub diff_addition: u32,
    pub diff_deletion: u32,
    pub status_success: u32,
    pub status_danger: u32,
    pub status_warning: u32,
    pub status_merged: u32,
    pub status_dot_success: u32,
    pub status_dot_danger: u32,
    pub status_dot_warning: u32,
    pub status_dot_running: u32,
    pub terminal: TerminalPalette,
}

const fn hex(value: &str) -> u32 {
    let bytes = value.as_bytes();
    let mut result = 0u32;
    let mut index = 0;
    while index < bytes.len() {
        let digit = match bytes[index] {
            b'0'..=b'9' => bytes[index] - b'0',
            b'a'..=b'f' => bytes[index] - b'a' + 10,
            b'A'..=b'F' => bytes[index] - b'A' + 10,
            _ => panic!("authored palette colors are six-digit hex"),
        };
        result = result * 16 + digit as u32;
        index += 1;
    }
    result
}

/// Dark diff/status bands are shared by every dark variant (authored once in
/// theme.ts, spread into each semantic color set).
const DARK_DIFF_ADDITION: u32 = hex("4ade80");
const DARK_DIFF_DELETION: u32 = hex("ef4444");
const DARK_STATUS_SUCCESS: u32 = hex("6cb17b");
const DARK_STATUS_DANGER: u32 = hex("d8847b");
const DARK_STATUS_WARNING: u32 = hex("c09664");
const DARK_STATUS_MERGED: u32 = hex("a890d5");
const DARK_STATUS_DOT_SUCCESS: u32 = hex("35c264");
const DARK_STATUS_DOT_DANGER: u32 = hex("f7796d");
const DARK_STATUS_DOT_WARNING: u32 = hex("db932e");
const DARK_STATUS_DOT_RUNNING: u32 = hex("5caaf6");
const LIGHT_DIFF_ADDITION: u32 = hex("15803d");
const LIGHT_DIFF_DELETION: u32 = hex("b91c1c");
const LIGHT_STATUS_SUCCESS: u32 = hex("3e704a");
const LIGHT_STATUS_DANGER: u32 = hex("9d433b");
const LIGHT_STATUS_WARNING: u32 = hex("7b5d39");
const LIGHT_STATUS_MERGED: u32 = hex("7347af");
const LIGHT_STATUS_DOT_SUCCESS: u32 = hex("299f51");
const LIGHT_STATUS_DOT_DANGER: u32 = hex("f12e2f");
const LIGHT_STATUS_DOT_WARNING: u32 = hex("b37824");
const LIGHT_STATUS_DOT_RUNNING: u32 = hex("268ae0");

/// Authored dark ANSI ramp, shared by every dark theme.
const DARK_ANSI: [u32; 16] = [
    0, // black comes from the tint's terminalBlack; patched in `dark_palette`
    hex("e07070"),
    hex("5dba80"),
    hex("d4a44a"),
    hex("6a9de0"),
    hex("b07ad0"),
    hex("4aabb8"),
    hex("d4d4d8"),
    0, // brightBlack comes from the tint
    hex("e89090"),
    hex("7ecf9a"),
    hex("e0be6e"),
    hex("8ab4e8"),
    hex("c49ae0"),
    hex("6ec2cc"),
    hex("f0f0f2"),
];

/// Authored light ANSI ramp. Black and brightBlack come from the tint.
const LIGHT_ANSI: [u32; 16] = [
    0,
    hex("dc2626"),
    hex("16a34a"),
    hex("ca8a04"),
    hex("2563eb"),
    hex("9333ea"),
    hex("0891b2"),
    hex("ffffff"),
    0,
    hex("ef4444"),
    hex("22c55e"),
    hex("f59e0b"),
    hex("3b82f6"),
    hex("a855f7"),
    hex("06b6d4"),
    hex("fafafa"),
];

pub struct DarkTint {
    pub surface0: &'static str,
    pub surface1: &'static str,
    pub surface2: &'static str,
    pub surface3: &'static str,
    pub surface4: &'static str,
    pub surface_diff_empty: &'static str,
    pub surface_sidebar: &'static str,
    pub foreground_muted: &'static str,
    pub foreground_extra_muted: &'static str,
    pub border: &'static str,
    pub border_accent: &'static str,
    pub accent: &'static str,
    pub accent_bright: &'static str,
    pub accent_foreground: Option<&'static str>,
    pub destructive: &'static str,
    pub terminal_black: &'static str,
    pub terminal_bright_black: &'static str,
    pub foreground: &'static str,
    pub ring: &'static str,
}

pub struct LightTint {
    pub surface0: &'static str,
    pub surface1: &'static str,
    pub surface2: &'static str,
    pub surface3: &'static str,
    pub surface4: &'static str,
    pub surface_diff_empty: &'static str,
    pub surface_sidebar: &'static str,
    pub foreground: &'static str,
    pub foreground_muted: &'static str,
    pub foreground_extra_muted: &'static str,
    pub border: &'static str,
    pub border_accent: &'static str,
    pub accent: &'static str,
    pub accent_bright: &'static str,
    pub accent_foreground: &'static str,
    pub primary: &'static str,
    pub primary_foreground: &'static str,
    pub destructive: &'static str,
    pub terminal_black: &'static str,
    pub terminal_bright_black: &'static str,
    pub ring: &'static str,
}

pub fn dark_palette(tint: &DarkTint) -> Palette {
    let mut ansi = DARK_ANSI;
    ansi[0] = hex(tint.terminal_black);
    ansi[8] = hex(tint.terminal_bright_black);
    Palette {
        is_dark: true,
        surface0: hex(tint.surface0),
        surface1: hex(tint.surface1),
        surface2: hex(tint.surface2),
        surface3: hex(tint.surface3),
        surface4: hex(tint.surface4),
        surface_diff_empty: hex(tint.surface_diff_empty),
        surface_sidebar: hex(tint.surface_sidebar),
        surface_sidebar_hover: hex(tint.surface1),
        surface_sidebar_selected: hex(tint.surface2),
        surface_workspace: hex(tint.surface1),
        foreground: hex(tint.foreground),
        foreground_muted: hex(tint.foreground_muted),
        foreground_extra_muted: hex(tint.foreground_extra_muted),
        border: hex(tint.border),
        border_accent: hex(tint.border_accent),
        accent: hex(tint.accent),
        accent_bright: hex(tint.accent_bright),
        accent_foreground: tint.accent_foreground.map(hex).unwrap_or(0xffffff),
        destructive: hex(tint.destructive),
        destructive_foreground: 0xffffff,
        success: hex(tint.accent),
        success_foreground: 0xffffff,
        background: hex(tint.surface0),
        popover: hex(tint.surface2),
        popover_foreground: hex(tint.foreground),
        primary: hex(tint.foreground),
        primary_foreground: hex(tint.surface0),
        secondary: hex(tint.surface2),
        secondary_foreground: hex(tint.foreground),
        muted: hex(tint.surface2),
        muted_foreground: hex(tint.foreground_muted),
        accent_border: hex(tint.border_accent),
        input: hex(tint.surface2),
        ring: hex(tint.ring),
        diff_addition: DARK_DIFF_ADDITION,
        diff_deletion: DARK_DIFF_DELETION,
        status_success: DARK_STATUS_SUCCESS,
        status_danger: DARK_STATUS_DANGER,
        status_warning: DARK_STATUS_WARNING,
        status_merged: DARK_STATUS_MERGED,
        status_dot_success: DARK_STATUS_DOT_SUCCESS,
        status_dot_danger: DARK_STATUS_DOT_DANGER,
        status_dot_warning: DARK_STATUS_DOT_WARNING,
        status_dot_running: DARK_STATUS_DOT_RUNNING,
        terminal: TerminalPalette {
            background: hex(tint.surface0),
            foreground: hex(tint.foreground),
            cursor: hex(tint.foreground),
            cursor_accent: hex(tint.surface0),
            selection_background: 0xffffff33,
            selection_foreground: hex(tint.foreground),
            ansi,
        },
    }
}

pub fn light_palette(tint: &LightTint) -> Palette {
    let mut ansi = LIGHT_ANSI;
    ansi[0] = hex(tint.terminal_black);
    ansi[8] = hex(tint.terminal_bright_black);
    Palette {
        is_dark: false,
        surface0: hex(tint.surface0),
        surface1: hex(tint.surface1),
        surface2: hex(tint.surface2),
        surface3: hex(tint.surface3),
        surface4: hex(tint.surface4),
        surface_diff_empty: hex(tint.surface_diff_empty),
        surface_sidebar: hex(tint.surface_sidebar),
        surface_sidebar_hover: hex(tint.surface3),
        surface_sidebar_selected: hex(tint.surface4),
        surface_workspace: hex(tint.surface0),
        foreground: hex(tint.foreground),
        foreground_muted: hex(tint.foreground_muted),
        foreground_extra_muted: hex(tint.foreground_extra_muted),
        border: hex(tint.border),
        border_accent: hex(tint.border_accent),
        accent: hex(tint.accent),
        accent_bright: hex(tint.accent_bright),
        accent_foreground: hex(tint.accent_foreground),
        destructive: hex(tint.destructive),
        destructive_foreground: hex(tint.surface0),
        success: hex(tint.accent),
        success_foreground: hex(tint.surface0),
        background: hex(tint.surface0),
        popover: hex(tint.surface0),
        popover_foreground: hex(tint.foreground),
        primary: hex(tint.primary),
        primary_foreground: hex(tint.primary_foreground),
        secondary: hex(tint.surface2),
        secondary_foreground: hex(tint.foreground),
        muted: hex(tint.surface2),
        muted_foreground: hex(tint.foreground_muted),
        accent_border: hex(tint.border_accent),
        input: hex(tint.surface2),
        ring: hex(tint.ring),
        diff_addition: LIGHT_DIFF_ADDITION,
        diff_deletion: LIGHT_DIFF_DELETION,
        status_success: LIGHT_STATUS_SUCCESS,
        status_danger: LIGHT_STATUS_DANGER,
        status_warning: LIGHT_STATUS_WARNING,
        status_merged: LIGHT_STATUS_MERGED,
        status_dot_success: LIGHT_STATUS_DOT_SUCCESS,
        status_dot_danger: LIGHT_STATUS_DOT_DANGER,
        status_dot_warning: LIGHT_STATUS_DOT_WARNING,
        status_dot_running: LIGHT_STATUS_DOT_RUNNING,
        terminal: TerminalPalette {
            background: hex(tint.surface0),
            foreground: hex(tint.foreground),
            cursor: hex(tint.foreground),
            cursor_accent: hex(tint.surface0),
            selection_background: 0x00000026,
            selection_foreground: hex(tint.foreground),
            ansi,
        },
    }
}

/// The authored Paseo default dark tint.
pub const PASEO_DARK: DarkTint = DarkTint {
    surface0: "181B1A",
    surface1: "1E2120",
    surface2: "272A29",
    surface3: "434645",
    surface4: "595B5B",
    surface_diff_empty: "252827",
    surface_sidebar: "141716",
    foreground_muted: "A1A5A4",
    foreground_extra_muted: "717574",
    border: "252B2A",
    border_accent: "2F3534",
    accent: "20744A",
    accent_bright: "7ccba0",
    accent_foreground: None,
    destructive: "c64f43",
    terminal_black: "141716",
    terminal_bright_black: "434645",
    foreground: "fafafa",
    ring: "d4d4d8",
};

/// Authored dark variant tints, one per `THEME_OPTIONS` entry.
pub fn variant_tint(preference: ThemePreference) -> Option<&'static DarkTint> {
    match preference {
        ThemePreference::Zinc => Some(&DarkTint {
            surface0: "18181b",
            surface1: "1f1f22",
            surface2: "27272a",
            surface3: "3f3f46",
            surface4: "52525b",
            surface_diff_empty: "242427",
            surface_sidebar: "131316",
            foreground_muted: "a1a1aa",
            foreground_extra_muted: "71717a",
            border: "27272a",
            border_accent: "303036",
            accent: "e4e4e7",
            accent_bright: "fafafa",
            accent_foreground: Some("18181b"),
            destructive: "c44a4a",
            terminal_black: "131316",
            terminal_bright_black: "3f3f46",
            foreground: "fafafa",
            ring: "d4d4d8",
        }),
        ThemePreference::Midnight => Some(&DarkTint {
            surface0: "161820",
            surface1: "1c1e27",
            surface2: "252731",
            surface3: "3c3e4c",
            surface4: "535564",
            surface_diff_empty: "222430",
            surface_sidebar: "121420",
            foreground_muted: "9a9db0",
            foreground_extra_muted: "6b6e82",
            border: "242636",
            border_accent: "2e3040",
            accent: "3b6fcf",
            accent_bright: "7eaaeb",
            accent_foreground: None,
            destructive: "c44a52",
            terminal_black: "121420",
            terminal_bright_black: "3c3e4c",
            foreground: "fafafa",
            ring: "d4d4d8",
        }),
        ThemePreference::Claude => Some(&DarkTint {
            surface0: "1f1f1e",
            surface1: "262523",
            surface2: "2f2d2b",
            surface3: "4a4745",
            surface4: "605d5b",
            surface_diff_empty: "2a2826",
            surface_sidebar: "1a1918",
            foreground_muted: "ada9a5",
            foreground_extra_muted: "78746f",
            border: "2c2a27",
            border_accent: "36332f",
            accent: "d97757",
            accent_bright: "e89a7f",
            accent_foreground: None,
            destructive: "cf513e",
            terminal_black: "1a1918",
            terminal_bright_black: "4a4745",
            foreground: "fafafa",
            ring: "d4d4d8",
        }),
        ThemePreference::Ghostty => Some(&DarkTint {
            surface0: "282c34",
            surface1: "2f333d",
            surface2: "383c48",
            surface3: "4a4f5e",
            surface4: "5b6175",
            surface_diff_empty: "323643",
            surface_sidebar: "21252d",
            foreground_muted: "c8ccd8",
            foreground_extra_muted: "a0a4b2",
            border: "353a47",
            border_accent: "3f4454",
            accent: "89b4fa",
            accent_bright: "b4d0fc",
            accent_foreground: None,
            destructive: "c44a55",
            terminal_black: "21252d",
            terminal_bright_black: "4a4f5e",
            foreground: "fafafa",
            ring: "d4d4d8",
        }),
        ThemePreference::PureBlack => Some(&DarkTint {
            surface0: "000000",
            surface1: "0a0a0a",
            surface2: "111111",
            surface3: "202020",
            surface4: "2d2d2d",
            surface_diff_empty: "0c0c0c",
            surface_sidebar: "000000",
            foreground_muted: "a1a1aa",
            foreground_extra_muted: "71717a",
            border: "1c1c1c",
            border_accent: "242424",
            accent: "20744A",
            accent_bright: "7ccba0",
            accent_foreground: None,
            destructive: "c44a4a",
            terminal_black: "595959",
            terminal_bright_black: "8a8a8a",
            foreground: "fafafa",
            ring: "d4d4d8",
        }),
        _ => None,
    }
}

pub const LIGHT: LightTint = LightTint {
    surface0: "ffffff",
    surface1: "fafafa",
    surface2: "f4f4f5",
    surface3: "e4e4e7",
    surface4: "d4d4d8",
    surface_diff_empty: "f6f6f6",
    surface_sidebar: "f4f4f5",
    foreground: "1a1a1e",
    foreground_muted: "71717a",
    foreground_extra_muted: "a1a1aa",
    border: "e4e4e7",
    border_accent: "ececf1",
    accent: "20744A",
    accent_bright: "239956",
    accent_foreground: "ffffff",
    primary: "18181b",
    primary_foreground: "fafafa",
    destructive: "b04138",
    terminal_black: "1a1a1e",
    terminal_bright_black: "3f3f46",
    ring: "18181b",
};

/// Resolve a theme preference to its concrete palette. `auto` follows the
/// operating system appearance; every other choice is its authored palette.
pub fn resolve(preference: ThemePreference, system_dark: bool) -> Palette {
    match preference {
        ThemePreference::Light => light_palette(&LIGHT),
        ThemePreference::Dark => dark_palette(&PASEO_DARK),
        ThemePreference::Auto => {
            if system_dark {
                dark_palette(&PASEO_DARK)
            } else {
                light_palette(&LIGHT)
            }
        }
        other => dark_palette(variant_tint(other).unwrap_or(&PASEO_DARK)),
    }
}

/// Authored UI font ramp scaling from the app's `applyAppearance`: the ramp
/// scales proportionally from the requested interface base size, rounded to
/// whole pixels. Code and content sizes are absolute, never scaled.
pub fn scale_ui_font(authored: f32, ui_base_font_size: i64) -> f32 {
    (authored * ui_base_font_size as f32 / 14.).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dark_palette_matches_the_generated_tokens() {
        let palette = resolve(ThemePreference::Dark, false);
        assert_eq!(palette.surface0, 0x181b1a);
        assert_eq!(palette.surface1, 0x1e2120);
        assert_eq!(palette.surface2, 0x272a29);
        assert_eq!(palette.surface3, 0x434645);
        assert_eq!(palette.surface4, 0x595b5b);
        assert_eq!(palette.surface_diff_empty, 0x252827);
        assert_eq!(palette.surface_sidebar, 0x141716);
        assert_eq!(palette.surface_sidebar_hover, 0x1e2120);
        assert_eq!(palette.surface_sidebar_selected, 0x272a29);
        assert_eq!(palette.surface_workspace, 0x1e2120);
        assert_eq!(palette.foreground, 0xfafafa);
        assert_eq!(palette.foreground_muted, 0xa1a5a4);
        assert_eq!(palette.foreground_extra_muted, 0x717574);
        assert_eq!(palette.border, 0x252b2a);
        assert_eq!(palette.border_accent, 0x2f3534);
        assert_eq!(palette.accent, 0x20744a);
        assert_eq!(palette.accent_bright, 0x7ccba0);
        assert_eq!(palette.accent_foreground, 0xffffff);
        assert_eq!(palette.destructive, 0xc64f43);
        assert_eq!(palette.destructive_foreground, 0xffffff);
        assert_eq!(palette.success, 0x20744a);
        assert_eq!(palette.success_foreground, 0xffffff);
        assert_eq!(palette.background, 0x181b1a);
        assert_eq!(palette.popover, 0x272a29);
        assert_eq!(palette.popover_foreground, 0xfafafa);
        assert_eq!(palette.primary, 0xfafafa);
        assert_eq!(palette.primary_foreground, 0x181b1a);
        assert_eq!(palette.secondary, 0x272a29);
        assert_eq!(palette.secondary_foreground, 0xfafafa);
        assert_eq!(palette.muted, 0x272a29);
        assert_eq!(palette.muted_foreground, 0xa1a5a4);
        assert_eq!(palette.accent_border, 0x2f3534);
        assert_eq!(palette.input, 0x272a29);
        assert_eq!(palette.ring, 0xd4d4d8);
        assert_eq!(palette.diff_addition, 0x4ade80);
        assert_eq!(palette.diff_deletion, 0xef4444);
        assert_eq!(palette.status_success, 0x6cb17b);
        assert_eq!(palette.status_danger, 0xd8847b);
        assert_eq!(palette.status_warning, 0xc09664);
        assert_eq!(palette.status_merged, 0xa890d5);
        assert_eq!(palette.status_dot_success, 0x35c264);
        assert_eq!(palette.status_dot_danger, 0xf7796d);
        assert_eq!(palette.status_dot_warning, 0xdb932e);
        assert_eq!(palette.status_dot_running, 0x5caaf6);
    }

    #[test]
    fn default_dark_terminal_palette_matches_the_generated_tokens() {
        let terminal = resolve(ThemePreference::Dark, false).terminal;
        assert_eq!(terminal.background, 0x181b1a);
        assert_eq!(terminal.foreground, 0xfafafa);
        assert_eq!(terminal.cursor, 0xfafafa);
        assert_eq!(terminal.cursor_accent, 0x181b1a);
        assert_eq!(terminal.selection_background, 0xffffff33);
        assert_eq!(terminal.selection_foreground, 0xfafafa);
        assert_eq!(
            terminal.ansi,
            [
                0x141716, 0xe07070, 0x5dba80, 0xd4a44a, 0x6a9de0, 0xb07ad0, 0x4aabb8, 0xd4d4d8,
                0x434645, 0xe89090, 0x7ecf9a, 0xe0be6e, 0x8ab4e8, 0xc49ae0, 0x6ec2cc, 0xf0f0f2,
            ]
        );
    }

    #[test]
    fn light_palette_matches_the_authored_light_theme() {
        let palette = resolve(ThemePreference::Light, false);
        assert_eq!(palette.surface0, 0xffffff);
        assert_eq!(palette.surface1, 0xfafafa);
        assert_eq!(palette.surface2, 0xf4f4f5);
        assert_eq!(palette.surface3, 0xe4e4e7);
        assert_eq!(palette.surface4, 0xd4d4d8);
        assert_eq!(palette.surface_sidebar, 0xf4f4f5);
        // Light sidebars hover/derive from tint surface3/surface4, not surface1/2.
        assert_eq!(palette.surface_sidebar_hover, 0xe4e4e7);
        assert_eq!(palette.surface_sidebar_selected, 0xd4d4d8);
        assert_eq!(palette.foreground, 0x1a1a1e);
        assert_eq!(palette.foreground_muted, 0x71717a);
        assert_eq!(palette.border, 0xe4e4e7);
        assert_eq!(palette.accent, 0x20744a);
        assert_eq!(palette.accent_bright, 0x239956);
        assert_eq!(palette.destructive, 0xb04138);
        assert_eq!(palette.primary, 0x18181b);
        assert_eq!(palette.primary_foreground, 0xfafafa);
        assert_eq!(palette.popover, 0xffffff);
        assert_eq!(palette.status_success, 0x3e704a);
        assert_eq!(palette.diff_addition, 0x15803d);
        assert_eq!(palette.terminal.selection_background, 0x00000026);
        assert_eq!(palette.terminal.ansi[1], 0xdc2626);
        assert_eq!(palette.terminal.ansi[0], 0x1a1a1e);
        assert_eq!(palette.terminal.ansi[8], 0x3f3f46);
    }

    #[test]
    fn auto_follows_the_system_appearance() {
        assert_eq!(
            resolve(ThemePreference::Auto, true),
            resolve(ThemePreference::Dark, false)
        );
        assert_eq!(
            resolve(ThemePreference::Auto, false),
            resolve(ThemePreference::Light, false)
        );
    }

    #[test]
    fn variant_tints_match_the_authored_values() {
        let zinc = resolve(ThemePreference::Zinc, false);
        assert_eq!(zinc.surface0, 0x18181b);
        assert_eq!(zinc.accent, 0xe4e4e7);
        assert_eq!(zinc.accent_foreground, 0x18181b);
        assert_eq!(zinc.destructive, 0xc44a4a);

        let midnight = resolve(ThemePreference::Midnight, false);
        assert_eq!(midnight.surface0, 0x161820);
        assert_eq!(midnight.accent, 0x3b6fcf);
        assert_eq!(midnight.foreground_muted, 0x9a9db0);

        let claude = resolve(ThemePreference::Claude, false);
        assert_eq!(claude.surface0, 0x1f1f1e);
        assert_eq!(claude.accent, 0xd97757);
        assert_eq!(claude.destructive, 0xcf513e);

        let ghostty = resolve(ThemePreference::Ghostty, false);
        assert_eq!(ghostty.surface0, 0x282c34);
        assert_eq!(ghostty.accent, 0x89b4fa);
        assert_eq!(ghostty.border, 0x353a47);

        let pure_black = resolve(ThemePreference::PureBlack, false);
        assert_eq!(pure_black.surface0, 0x000000);
        assert_eq!(pure_black.surface1, 0x0a0a0a);
        assert_eq!(pure_black.surface_sidebar, 0x000000);
        assert_eq!(pure_black.terminal.ansi[0], 0x595959);
        assert_eq!(pure_black.terminal.ansi[8], 0x8a8a8a);

        // Dark variants share the authored dark bands.
        for preference in [
            ThemePreference::Zinc,
            ThemePreference::Midnight,
            ThemePreference::Claude,
            ThemePreference::Ghostty,
            ThemePreference::PureBlack,
        ] {
            let palette = resolve(preference, false);
            assert_eq!(palette.status_danger, 0xd8847b);
            assert_eq!(palette.status_dot_running, 0x5caaf6);
            assert_eq!(palette.diff_deletion, 0xef4444);
        }
    }

    #[test]
    fn ui_ramp_scales_from_the_authored_sizes() {
        assert_eq!(scale_ui_font(14., 14), 14.);
        assert_eq!(scale_ui_font(12., 16), 14.);
        assert_eq!(scale_ui_font(14., 10), 10.);
        // Code and content sizes are absolute; callers never scale them.
    }
}
