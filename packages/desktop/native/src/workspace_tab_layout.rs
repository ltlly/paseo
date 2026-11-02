//! Authored workspace-tab shrink/overflow policy, independent of native layout.
#[path = "workspace_tab_presentation.rs"]
pub mod p;

#[derive(Debug)]
pub struct Layout {
    pub widths: Vec<f32>,
    pub show_labels: Vec<bool>,
    pub overflow: bool,
}
pub fn layout(viewport: f32, labels: &[f32], reserved: f32, compact: bool) -> Layout {
    let chrome =
        f64::from(p::ICON_SIZE + p::CONTENT_GAP + p::CHIP_PADDING_X * 2. + p::CLOSE_RESERVED);
    let min = f64::from(p::MIN_WIDTH);
    let max = f64::from(p::MAX_WIDTH);
    let available = ((f64::from(viewport) - f64::from(reserved)).max(0.)
        - f64::from(p::ROW_PADDING_X * 2. + labels.len().saturating_sub(1) as f32 * p::CHIP_GAP))
    .max(0.);
    let natural: Vec<f64> = labels
        .iter()
        .map(|width| (chrome + f64::from(*width)).clamp(min, max))
        .collect();
    let total: f64 = natural.iter().sum();
    let minimum = min * labels.len() as f64;
    let overflow = !labels.is_empty()
        && if compact {
            total > available
        } else {
            available < minimum
        };
    let ratio = if !overflow && !compact && total > available {
        (total - available) / (total - minimum)
    } else {
        0.
    };
    let widths: Vec<f32> = natural
        .into_iter()
        .map(|width| (width - (width - min) * ratio).clamp(min, max).round() as f32)
        .collect();
    let show_labels = widths
        .iter()
        .map(|width| f64::from(*width) > chrome)
        .collect();
    Layout {
        widths,
        show_labels,
        overflow,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_authored_widths_and_overflow_decisions() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/workspace-tabs.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let labels: Vec<_> = case["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_f64().unwrap() as f32)
                .collect();
            let compact = case["compact"].as_bool().unwrap();
            let result = super::layout(
                case["viewport"].as_f64().unwrap() as f32,
                &labels,
                case["reserved"].as_f64().unwrap() as f32,
                compact,
            );
            let actual = serde_json::json!({"items":result.widths.iter().zip(result.show_labels).map(|(width,show)|serde_json::json!({"width":*width as u32,"showLabel":show})).collect::<Vec<_>>(),"closeButtonPolicy":if compact{"none"}else{"all"},"requiresHorizontalScrollFallback":result.overflow});
            assert_eq!(actual, case["expected"], "{case}");
        }
    }
}
