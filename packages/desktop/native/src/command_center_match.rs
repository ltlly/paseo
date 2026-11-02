//! Non-fuzzy Command Center matcher. Indices follow the authored JavaScript
//! UTF-16 contract, including supplementary characters before a match.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Score {
    pub tier: usize,
    pub offset: usize,
    pub spread: usize,
}
fn whitespace(unit: u16) -> bool {
    matches!(unit,9..=13|32|160|5760|8192..=8202|8232|8233|8239|8287|12288|65279)
}
pub fn query_is_empty(query: &str) -> bool {
    query
        .chars()
        .all(|c| (c as u32) <= 65535 && whitespace(c as u16))
}
fn boundary(unit: u16) -> bool {
    !(unit >= b'a' as u16 && unit <= b'z' as u16 || unit >= b'0' as u16 && unit <= b'9' as u16)
}
fn matched(query: &str, text: &str) -> Option<Score> {
    let q: Vec<_> = query.to_lowercase().encode_utf16().collect();
    let t: Vec<_> = text.to_lowercase().encode_utf16().collect();
    if q.is_empty() || q == t {
        return Some(Score {
            tier: 0,
            offset: 0,
            spread: 0,
        });
    }
    let mut best = None;
    if q.len() <= t.len() {
        for pos in 0..=t.len() - q.len() {
            if t[pos..pos + q.len()] != q {
                continue;
            }
            let start = pos == 0 || boundary(t[pos - 1]);
            let end = pos + q.len() == t.len() || boundary(t[pos + q.len()]);
            let score = Score {
                tier: if start && end {
                    1
                } else if pos == 0 {
                    2
                } else if start {
                    3
                } else {
                    4
                },
                offset: pos,
                spread: 0,
            };
            best = Some(best.map_or(score, |b: Score| b.min(score)));
        }
    }
    if best.is_some() {
        return best;
    }
    let (mut index, mut first) = (0, 0);
    for (pos, unit) in t.iter().enumerate() {
        if whitespace(*unit) {
            index = 0;
            first = 0;
            continue;
        }
        if *unit != q[index] {
            continue;
        }
        if index == 0 {
            first = pos;
        }
        index += 1;
        if index == q.len() {
            return Some(Score {
                tier: 5,
                offset: first,
                spread: pos - first + 1,
            });
        }
    }
    None
}
pub fn score(query: &str, fields: &[&str]) -> Option<Score> {
    let mut result = Score {
        tier: 0,
        offset: 0,
        spread: 0,
    };
    let normalized = query.to_lowercase();
    for token in normalized
        .split(|c: char| (c as u32) <= 65535 && whitespace(c as u16))
        .filter(|token| !token.is_empty())
    {
        let best = fields
            .iter()
            .filter_map(|field| matched(token, field))
            .min()?;
        result.tier += best.tier;
        result.offset += best.offset;
        result.spread += if best.spread == 0 {
            token.encode_utf16().count()
        } else {
            best.spread
        };
    }
    Some(result)
}
pub fn score_fields(query: &str, visible: &[&str], hidden: &[String]) -> Option<(usize, Score)> {
    if let Some(score) = score(query, visible) {
        return Some((0, score));
    }
    let mut fields = visible.to_vec();
    fields.extend(hidden.iter().map(String::as_str));
    score(query, &fields).map(|score| (1, score))
}

/// Ranges use UTF-16 offsets like the authored matcher; rendering converts
/// them back to UTF-8 boundaries without slicing a Unicode scalar.
pub fn highlight_ranges(query: &str, text: &str) -> Vec<std::ops::Range<usize>> {
    let normalized = query.to_lowercase();
    let lowered: Vec<_> = text.to_lowercase().encode_utf16().collect();
    let mut ranges = vec![];
    for token in normalized
        .split(|c: char| (c as u32) <= 65535 && whitespace(c as u16))
        .filter(|token| !token.is_empty())
    {
        let Some(score) = matched(token, text) else {
            continue;
        };
        if score.tier == 5 {
            let units: Vec<_> = token.encode_utf16().collect();
            let mut index = 0;
            for (offset, unit) in lowered.iter().enumerate().skip(score.offset) {
                if *unit == units[index] {
                    ranges.push(offset..offset + 1);
                    index += 1;
                    if index == units.len() {
                        break;
                    }
                }
            }
        } else {
            ranges.push(if score.tier == 0 {
                0..text.encode_utf16().count()
            } else {
                score.offset..score.offset + token.encode_utf16().count()
            });
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = vec![];
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

pub fn byte_highlights(query: &str, text: &str) -> Vec<std::ops::Range<usize>> {
    let mut units = 0;
    let mut boundaries = vec![(0, 0)];
    for (byte, ch) in text.char_indices() {
        units += ch.len_utf16();
        boundaries.push((units, byte + ch.len_utf8()));
    }
    highlight_ranges(query, text)
        .into_iter()
        .filter_map(|range| {
            let start = boundaries
                .iter()
                .take_while(|(unit, _)| *unit <= range.start)
                .last()
                .unwrap()
                .1;
            let end = boundaries
                .iter()
                .find(|(unit, _)| *unit >= range.end)
                .map(|(_, byte)| *byte)
                .unwrap_or(text.len());
            (end > start).then_some(start..end)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    #[test]
    fn utf16_highlights_map_to_complete_utf8_scalars() {
        let text = "🦀 中 alpha.rs";
        let ranges = super::byte_highlights("🦀 alpha", text);
        assert_eq!(
            ranges.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(),
            ["🦀", "alpha"]
        );
        let ranges = super::byte_highlights("中", text);
        assert_eq!(&text[ranges[0].clone()], "中");
        assert!(super::query_is_empty(" \u{feff}\u{a0}\t"));
        assert!(!super::query_is_empty("\u{10020}"));
    }

    #[test]
    fn matches_the_authored_non_fuzzy_matcher() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/command-center-match.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let fields: Vec<_> = case["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect();
            let actual = super::score(case["query"].as_str().unwrap(), &fields);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                case["score"],
                "{case}"
            );
            for (index, text) in fields.iter().enumerate() {
                let actual: Vec<_> = super::highlight_ranges(case["query"].as_str().unwrap(), text)
                    .iter()
                    .map(|range| serde_json::json!({"start":range.start,"length":range.len()}))
                    .collect();
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    case["highlights"][index],
                    "{case}"
                );
            }
        }
    }
}
