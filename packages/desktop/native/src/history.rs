use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub seq_start: u64,
    pub seq_end: u64,
    pub item: Value,
    pub timestamp: Option<String>,
    pub turn_id: Option<String>,
    pub source_seq_ranges: Vec<SeqRange>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeqRange {
    pub start_seq: u64,
    pub end_seq: u64,
}

impl Entry {
    fn identity(&self) -> Option<(&str, Option<&str>)> {
        match self.item["type"].as_str() {
            Some("tool_call") => Some((self.item["callId"].as_str()?, None)),
            Some("plugin") => Some((
                self.item["pluginId"].as_str()?,
                Some(self.item["id"].as_str()?),
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub request_id: String,
    pub agent_id: String,
    pub epoch: String,
    pub reset: bool,
    pub entries: Vec<Entry>,
    pub start_cursor: Option<Cursor>,
    pub end_cursor: Option<Cursor>,
    pub has_older: bool,
    pub has_newer: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct History {
    pub epoch: Option<String>,
    pub entries: BTreeMap<u64, Entry>,
    pub start: Option<Cursor>,
    pub end: Option<Cursor>,
    pub has_older: bool,
}

impl History {
    pub fn apply(&mut self, page: &Page) -> Result<()> {
        ensure!(
            page.error.is_none(),
            "{}",
            page.error.as_deref().unwrap_or("timeline failed")
        );
        for entry in &page.entries {
            ensure!(
                entry.seq_start <= entry.seq_end,
                "invalid timeline sequence range"
            );
            ensure!(
                !entry.source_seq_ranges.is_empty(),
                "timeline entry has no source coverage"
            );
            for range in &entry.source_seq_ranges {
                ensure!(
                    range.start_seq <= range.end_seq
                        && range.start_seq >= entry.seq_start
                        && range.end_seq <= entry.seq_end,
                    "invalid timeline source coverage"
                );
            }
        }
        for cursor in [&page.start_cursor, &page.end_cursor].into_iter().flatten() {
            ensure!(cursor.epoch == page.epoch, "timeline cursor epoch mismatch");
        }
        if page.reset || self.epoch.as_deref() != Some(&page.epoch) {
            *self = Self::default();
        }
        self.epoch = Some(page.epoch.clone());
        for incoming in &page.entries {
            let mut entry = incoming.clone();
            if let Some(id) = entry.identity() {
                if let Some(existing) = self.entries.values().find(|old| old.identity() == Some(id))
                {
                    if existing.seq_end > entry.seq_end {
                        entry.item = existing.item.clone();
                    }
                    entry.seq_start = entry.seq_start.min(existing.seq_start);
                    entry.seq_end = entry.seq_end.max(existing.seq_end);
                    entry
                        .source_seq_ranges
                        .extend(existing.source_seq_ranges.iter().cloned());
                    entry.source_seq_ranges.sort_by_key(|r| r.start_seq);
                    let mut ranges: Vec<SeqRange> = Vec::new();
                    for range in entry.source_seq_ranges {
                        if let Some(last) = ranges
                            .last_mut()
                            .filter(|last| range.start_seq <= last.end_seq.saturating_add(1))
                        {
                            last.end_seq = last.end_seq.max(range.end_seq);
                        } else {
                            ranges.push(range);
                        }
                    }
                    entry.source_seq_ranges = ranges;
                }
            } else if self.entries.values().any(|old| {
                old.seq_end > entry.seq_end
                    && entry.source_seq_ranges.iter().all(|new| {
                        old.source_seq_ranges.iter().any(|prior| {
                            prior.start_seq <= new.start_seq && prior.end_seq >= new.end_seq
                        })
                    })
            }) {
                continue;
            }
            self.entries.retain(|_, existing| {
                let same_identity = entry
                    .identity()
                    .is_some_and(|id| Some(id) == existing.identity());
                !same_identity
                    && !existing.source_seq_ranges.iter().any(|old| {
                        entry
                            .source_seq_ranges
                            .iter()
                            .any(|new| old.start_seq <= new.end_seq && new.start_seq <= old.end_seq)
                    })
            });
            self.entries.insert(entry.seq_start, entry);
        }
        if let Some(start) = &page.start_cursor {
            if self.start.as_ref().is_none_or(|old| start.seq <= old.seq) {
                self.start = Some(start.clone());
                self.has_older = page.has_older;
            }
        }
        if let Some(end) = &page.end_cursor {
            if self.end.as_ref().is_none_or(|old| end.seq >= old.seq) {
                self.end = Some(end.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn page(epoch: &str, start: u64, end: u64, text: &str) -> Page {
        serde_json::from_value(json!({"requestId":"r","agentId":"a","epoch":epoch,"reset":false,"entries":[{"seqStart":start,"seqEnd":end,"sourceSeqRanges":[{"startSeq":start,"endSeq":end}],"item":{"type":"assistant_message","text":text}}],"startCursor":{"epoch":epoch,"seq":start},"endCursor":{"epoch":epoch,"seq":end},"hasOlder":start>0,"hasNewer":false,"error":null})).unwrap()
    }
    #[test]
    fn projected_updates_replace_overlapping_ranges_without_duplicate_text() {
        let mut h = History::default();
        h.apply(&page("e", 3, 4, "hello")).unwrap();
        h.apply(&page("e", 3, 7, "hello world")).unwrap();
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[&3].item["text"], "hello world");
        h.apply(&page("e", 0, 2, "earlier")).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert!(!h.has_older);
        assert_eq!(h.end.as_ref().unwrap().seq, 7);
        h.apply(&page("e", 3, 4, "stale partial")).unwrap();
        assert_eq!(h.entries[&3].item["text"], "hello world");
    }
    #[test]
    fn new_epoch_discards_old_rows_and_bad_pages_cannot_mutate_history() {
        let mut h = History::default();
        h.apply(&page("old", 0, 4, "old")).unwrap();
        h.apply(&page("new", 0, 1, "new")).unwrap();
        assert_eq!(h.entries.len(), 1);
        let mut invalid = page("other", 0, 1, "invalid");
        invalid.end_cursor.as_mut().unwrap().epoch = "wrong".into();
        assert!(h.apply(&invalid).is_err());
        assert_eq!(h.epoch.as_deref(), Some("new"));
    }

    #[test]
    fn interleaved_tool_updates_preserve_assistant_rows_and_original_order() {
        let mut h = History::default();
        let mut tool = page("e", 1, 5, "unused");
        tool.entries[0].item = json!({"type":"tool_call","callId":"tool","status":"running"});
        tool.entries[0].source_seq_ranges = vec![
            SeqRange {
                start_seq: 1,
                end_seq: 1,
            },
            SeqRange {
                start_seq: 5,
                end_seq: 5,
            },
        ];
        h.apply(&tool).unwrap();
        h.apply(&page("e", 2, 4, "assistant")).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        let mut update = page("e", 6, 6, "unused");
        update.entries[0].item = json!({"type":"tool_call","callId":"tool","status":"completed"});
        h.apply(&update).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[&2].item["text"], "assistant");
        assert_eq!(h.entries.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        h.apply(&tool).unwrap();
        assert_eq!(h.entries[&1].item["status"], "completed");
    }
}
