//! Line diff for tool edits that carry only old/new strings.
//!
//! The reference uses a full LCS line diff. Native trims the common prefix and suffix first and
//! then reports the replaced middle: edits are local, so this keeps the output small without a
//! quadratic table over large files.

use paseo_desktop::appearance::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Context,
    Added,
    Removed,
}

/// The authored diff band is the diff color at 10% alpha.
pub const BAND_ALPHA: u32 = 0x1a;

/// Foreground and optional line band for one diff row. Context rows keep the body foreground
/// and no band, so the same helper serves both the unified-diff and inline-diff paths.
pub fn colors(palette: &Palette, kind: Kind) -> (u32, Option<u32>) {
    let band = |color: u32| Some(color << 8 | BAND_ALPHA);
    match kind {
        Kind::Added => (palette.diff_addition, band(palette.diff_addition)),
        Kind::Removed => (palette.diff_deletion, band(palette.diff_deletion)),
        Kind::Context => (palette.foreground, None),
    }
}

pub fn diff(old: &str, new: &str) -> Vec<(Kind, String)> {
    let old_lines = old.split('\n').collect::<Vec<_>>();
    let new_lines = new.split('\n').collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut lines = Vec::with_capacity(old_lines.len() + new_lines.len());
    for line in &old_lines[..prefix] {
        lines.push((Kind::Context, (*line).to_owned()));
    }
    for line in &old_lines[prefix..old_lines.len() - suffix] {
        lines.push((Kind::Removed, (*line).to_owned()));
    }
    for line in &new_lines[prefix..new_lines.len() - suffix] {
        lines.push((Kind::Added, (*line).to_owned()));
    }
    for line in &old_lines[old_lines.len() - suffix..] {
        lines.push((Kind::Context, (*line).to_owned()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use paseo_desktop::{appearance::resolve, settings::ThemePreference};

    #[test]
    fn diff_bands_follow_the_active_palette() {
        let light = resolve(ThemePreference::Light, false);
        let dark = resolve(ThemePreference::Dark, false);
        assert_eq!(
            colors(&dark, Kind::Added),
            (
                dark.diff_addition,
                Some(dark.diff_addition << 8 | BAND_ALPHA)
            )
        );
        assert_eq!(
            colors(&light, Kind::Removed),
            (
                light.diff_deletion,
                Some(light.diff_deletion << 8 | BAND_ALPHA)
            )
        );
        assert_eq!(colors(&dark, Kind::Context), (dark.foreground, None));
        assert_ne!(light.diff_addition, dark.diff_addition);
        assert_ne!(light.diff_deletion, dark.diff_deletion);
    }

    #[test]
    fn local_edits_keep_only_the_replaced_lines() {
        let lines = diff("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            lines,
            vec![
                (Kind::Context, "a".into()),
                (Kind::Removed, "b".into()),
                (Kind::Added, "B".into()),
                (Kind::Context, "c".into()),
                (Kind::Context, "".into()),
            ]
        );
        assert_eq!(diff("same", "same"), vec![(Kind::Context, "same".into())]);
        assert_eq!(
            diff("", "new"),
            vec![(Kind::Removed, "".into()), (Kind::Added, "new".into())]
        );
    }
}
