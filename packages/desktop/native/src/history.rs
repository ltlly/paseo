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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineWindow {
    pub max_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LiveChange {
    Ignored,
    Applied { anchor: u64, inserted: bool },
    Reconcile,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SyncNeed {
    Current,
    After,
    Tail,
}

struct BufferedEvent {
    payload: Value,
    bytes: usize,
}
#[derive(Default)]
pub struct LiveBuffer {
    events: BTreeMap<u64, BufferedEvent>,
    bytes: usize,
    epoch: Option<String>,
    high_seq: u64,
    generation: u64,
    unpositioned: Option<u64>,
}
impl LiveBuffer {
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    const MAX_EVENTS: usize = 128;
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn push(&mut self, payload: &Value) {
        if payload["event"]["type"] != "timeline" {
            return;
        }
        self.generation = self.generation.saturating_add(1);
        let (Some(epoch), Some(seq)) = (payload["epoch"].as_str(), payload["seq"].as_u64()) else {
            self.unpositioned = Some(self.generation);
            return;
        };
        if self.epoch.as_deref() != Some(epoch) {
            self.events.clear();
            self.bytes = 0;
            self.high_seq = 0;
            self.epoch = Some(epoch.into());
        }
        self.high_seq = self.high_seq.max(seq);
        if self.events.contains_key(&seq) {
            return;
        }
        let Some(bytes) = owned_size(payload, Self::MAX_BYTES - 256).map(|size| size + 256) else {
            return;
        };
        while self.bytes + bytes > Self::MAX_BYTES || self.events.len() >= Self::MAX_EVENTS {
            let Some((_, old)) = self.events.pop_first() else {
                break;
            };
            self.bytes -= old.bytes;
        }
        self.events.insert(
            seq,
            BufferedEvent {
                payload: payload.clone(),
                bytes,
            },
        );
        self.bytes += bytes;
    }
    pub fn finish_snapshot(
        &mut self,
        history: &mut History,
        requested_at: u64,
    ) -> Result<SyncNeed> {
        let cursor = history
            .cursor()
            .ok_or_else(|| anyhow::anyhow!("No canonical timeline baseline"))?;
        if self
            .epoch
            .as_deref()
            .is_some_and(|epoch| epoch != cursor.epoch)
        {
            if self.generation > requested_at {
                return Ok(SyncNeed::Tail);
            }
            self.events.clear();
            self.bytes = 0;
            self.high_seq = cursor.seq;
        }
        self.epoch = Some(cursor.epoch.clone());
        while self
            .events
            .first_key_value()
            .is_some_and(|(seq, _)| *seq <= cursor.seq)
        {
            self.bytes -= self.events.pop_first().unwrap().1.bytes;
        }
        if !history.is_current() {
            return Ok(SyncNeed::After);
        }
        while let Some((_, event)) = self.events.first_key_value() {
            match history.apply_live(&event.payload)? {
                LiveChange::Reconcile => return Ok(SyncNeed::After),
                _ => {
                    self.bytes -= self.events.pop_first().unwrap().1.bytes;
                }
            }
        }
        let unpositioned = self
            .unpositioned
            .take()
            .is_some_and(|generation| generation > requested_at);
        Ok(
            if unpositioned || history.cursor().unwrap().seq < self.high_seq {
                SyncNeed::After
            } else {
                SyncNeed::Current
            },
        )
    }
}

// Estimate owned payload storage before cloning. Overflow drops only the transient replay
// buffer; the canonical cursor still requires the omitted events through history catch-up.
fn owned_size(value: &Value, remaining: usize) -> Option<usize> {
    let mut size = 64usize;
    match value {
        Value::String(text) => size = size.checked_add(text.len())?,
        Value::Array(values) => {
            for value in values {
                size = size.checked_add(owned_size(value, remaining.checked_sub(size)?)?)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                size = size.checked_add(key.len() + 64)?;
                size = size.checked_add(owned_size(value, remaining.checked_sub(size)?)?)?;
            }
        }
        _ => {}
    }
    (size <= remaining).then_some(size)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identity {
    Tool(String),
    Plugin(String, String),
}
fn identity(item: &Value) -> Option<Identity> {
    match item["type"].as_str()? {
        "tool_call" => Some(Identity::Tool(item["callId"].as_str()?.into())),
        "plugin" => Some(Identity::Plugin(
            item["pluginId"].as_str()?.into(),
            item["id"].as_str()?.into(),
        )),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize)]
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
    pub window: TimelineWindow,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct History {
    pub epoch: Option<String>,
    pub entries: BTreeMap<u64, Entry>,
    pub start: Option<Cursor>,
    pub end: Option<Cursor>,
    pub has_older: bool,
    live_ready: bool,
    identities: BTreeMap<Identity, u64>,
}

impl History {
    pub fn cursor(&self) -> Option<Cursor> {
        self.epoch.as_ref().map(|epoch| Cursor {
            epoch: epoch.clone(),
            seq: self.end.as_ref().map_or(0, |end| end.seq),
        })
    }
    pub fn is_current(&self) -> bool {
        self.live_ready
    }

    pub fn apply_live(&mut self, payload: &Value) -> Result<LiveChange> {
        let event = &payload["event"];
        if event["type"] != "timeline" {
            return Ok(LiveChange::Ignored);
        }
        let (Some(epoch), Some(seq)) = (payload["epoch"].as_str(), payload["seq"].as_u64()) else {
            return Ok(LiveChange::Reconcile);
        };
        if self.epoch.as_deref() != Some(epoch) {
            return Ok(LiveChange::Reconcile);
        }
        let current = self.end.as_ref().map_or(0, |end| end.seq);
        if seq <= current {
            return Ok(LiveChange::Ignored);
        }
        if !self.live_ready || current.checked_add(1) != Some(seq) {
            return Ok(LiveChange::Reconcile);
        }
        let item = &event["item"];
        let kind = item["type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Timeline event has no item type"))?;
        if matches!(kind, "assistant_message" | "reasoning" | "user_message") {
            ensure!(item["text"].is_string(), "Timeline text must be a string");
        }
        let turn = event["turnId"].as_str().map(str::to_owned);
        let timestamp = payload["timestamp"].as_str().map(str::to_owned);
        let key = identity(item);
        let identity_anchor = key
            .as_ref()
            .and_then(|key| self.identities.get(key))
            .copied()
            .filter(|anchor| {
                self.entries
                    .get(anchor)
                    .is_some_and(|entry| kind != "tool_call" || entry.turn_id == turn)
            });
        let text_anchor = self
            .entries
            .last_key_value()
            .filter(|(_, previous)| {
                matches!(kind, "assistant_message" | "reasoning")
                    && previous.item["type"] == kind
                    && previous.seq_end.checked_add(1) == Some(seq)
                    && previous.turn_id == turn
                    && (kind != "assistant_message"
                        || item.get("messageId").is_none()
                        || previous.item.get("messageId") == item.get("messageId"))
            })
            .map(|(anchor, _)| *anchor);
        let existing = identity_anchor.or(text_anchor);
        let anchor = existing.unwrap_or(seq);
        if let Some(previous) = existing.and_then(|id| self.entries.get_mut(&id)) {
            match kind {
                "assistant_message" | "reasoning" => match &mut previous.item["text"] {
                    Value::String(text) => text.push_str(item["text"].as_str().unwrap()),
                    _ => anyhow::bail!("Retained timeline text must be a string"),
                },
                "tool_call" => previous.item = merge_tool(&previous.item, item),
                _ => {
                    previous.item = item.clone();
                    previous.turn_id = turn;
                }
            }
            previous.seq_end = seq;
            previous.timestamp = timestamp;
            if let Some(last) = previous
                .source_seq_ranges
                .last_mut()
                .filter(|last| last.end_seq.checked_add(1) == Some(seq))
            {
                last.end_seq = seq;
            } else {
                previous.source_seq_ranges.push(SeqRange {
                    start_seq: seq,
                    end_seq: seq,
                });
            }
        } else {
            self.entries.insert(
                seq,
                Entry {
                    seq_start: seq,
                    seq_end: seq,
                    item: item.clone(),
                    timestamp,
                    turn_id: turn,
                    source_seq_ranges: vec![SeqRange {
                        start_seq: seq,
                        end_seq: seq,
                    }],
                },
            );
        }
        if let Some(key) = key {
            self.identities.insert(key, anchor);
        }
        if self.start.is_none() {
            self.start = Some(Cursor {
                epoch: epoch.into(),
                seq,
            });
        }
        self.end = Some(Cursor {
            epoch: epoch.into(),
            seq,
        });
        Ok(LiveChange::Applied {
            anchor,
            inserted: existing.is_none(),
        })
    }

    pub fn apply(&mut self, page: &Page) -> Result<()> {
        ensure!(
            page.error.is_none(),
            "{}",
            page.error.as_deref().unwrap_or("timeline failed")
        );
        for entry in &page.entries {
            ensure!(
                entry.seq_start <= entry.seq_end && entry.seq_end <= page.window.max_seq,
                "Invalid timeline sequence range"
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
            ensure!(
                cursor.epoch == page.epoch && cursor.seq <= page.window.max_seq,
                "timeline cursor epoch or sequence mismatch"
            );
        }
        if page.reset || self.epoch.as_deref() != Some(&page.epoch) {
            *self = Self::default();
        }
        self.epoch = Some(page.epoch.clone());
        for incoming in &page.entries {
            // Projected pages carry complete source-anchored rows. Tool IDs alone are not
            // row identities: providers can reuse one in another run, including late updates.
            let overlapping = self
                .entries
                .values()
                .filter(|old| {
                    old.source_seq_ranges.iter().any(|a| {
                        incoming
                            .source_seq_ranges
                            .iter()
                            .any(|b| a.start_seq <= b.end_seq && b.start_seq <= a.end_seq)
                    })
                })
                .collect::<Vec<_>>();
            if overlapping.iter().any(|old| {
                old.seq_end > incoming.seq_end
                    && incoming.source_seq_ranges.iter().all(|new| {
                        old.source_seq_ranges.iter().any(|prior| {
                            prior.start_seq <= new.start_seq && prior.end_seq >= new.end_seq
                        })
                    })
            }) {
                continue;
            }
            let mut entry = incoming.clone();
            if let Some(old) = self.entries.get(&entry.seq_start).filter(|old| {
                old.item["type"] == entry.item["type"]
                    && identity(&old.item) == identity(&entry.item)
                    && old.turn_id == entry.turn_id
            }) {
                if old.seq_end > entry.seq_end {
                    entry.item = old.item.clone();
                    entry.timestamp = old.timestamp.clone();
                }
                entry.seq_end = entry.seq_end.max(old.seq_end);
                entry
                    .source_seq_ranges
                    .extend(old.source_seq_ranges.iter().cloned());
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
            self.entries.retain(|_, old| {
                !old.source_seq_ranges.iter().any(|a| {
                    entry
                        .source_seq_ranges
                        .iter()
                        .any(|b| a.start_seq <= b.end_seq && b.start_seq <= a.end_seq)
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
        self.identities.clear();
        for (anchor, entry) in &self.entries {
            if let Some(key) = identity(&entry.item) {
                self.identities.insert(key, *anchor);
            }
        }
        self.live_ready = self.end.as_ref().map_or(0, |end| end.seq) >= page.window.max_seq;
        Ok(())
    }
}

fn merge_tool(previous: &Value, incoming: &Value) -> Value {
    let mut merged = previous.as_object().cloned().unwrap_or_default();
    if let Some(fields) = incoming.as_object() {
        merged.extend(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    if previous["detail"]["type"] != "unknown" && incoming["detail"]["type"] == "unknown" {
        merged.insert("detail".into(), previous["detail"].clone());
    }
    if previous["metadata"].is_object() || incoming["metadata"].is_object() {
        let mut metadata = previous["metadata"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(fields) = incoming["metadata"].as_object() {
            metadata.extend(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        merged.insert("metadata".into(), Value::Object(metadata));
    }
    match incoming["status"].as_str() {
        Some("completed" | "canceled") => {
            merged.insert("error".into(), Value::Null);
        }
        Some("failed") => {
            if let Some(error) = incoming.get("error") {
                merged.insert("error".into(), error.clone());
            } else {
                merged.remove("error");
            }
        }
        _ => {}
    }
    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn page(epoch: &str, start: u64, end: u64, text: &str) -> Page {
        serde_json::from_value(json!({"requestId":"r","agentId":"a","epoch":epoch,"reset":false,"entries":[{"seqStart":start,"seqEnd":end,"sourceSeqRanges":[{"startSeq":start,"endSeq":end}],"item":{"type":"assistant_message","text":text}}],"startCursor":{"epoch":epoch,"seq":start},"endCursor":{"epoch":epoch,"seq":end},"window":{"maxSeq":end},"hasOlder":start>0,"hasNewer":false,"error":null})).unwrap()
    }
    fn live(epoch: &str, seq: u64, item: Value) -> Value {
        json!({"agentId":"a","epoch":epoch,"seq":seq,"timestamp":format!("time-{seq}"),"event":{"type":"timeline","provider":"codex","turnId":"turn","item":item}})
    }
    #[test]
    fn contiguous_live_chunks_update_the_existing_message_without_a_history_fetch() {
        let mut h = History::default();
        let mut initial = page("e", 1, 1, "");
        initial.entries[0].item = json!({"type":"user_message","text":"Hi"});
        h.apply(&initial).unwrap();
        assert_eq!(
            h.apply_live(&live(
                "e",
                2,
                json!({"type":"assistant_message","text":"hello"})
            ))
            .unwrap(),
            LiveChange::Applied {
                anchor: 2,
                inserted: true
            }
        );
        assert_eq!(
            h.apply_live(&live(
                "e",
                3,
                json!({"type":"assistant_message","text":" world"})
            ))
            .unwrap(),
            LiveChange::Applied {
                anchor: 2,
                inserted: false
            }
        );
        assert_eq!(h.entries[&2].item["text"], "hello world");
        assert_eq!(h.entries[&2].seq_end, 3);
        assert_eq!(h.end.as_ref().unwrap().seq, 3);
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
        // The daemon's projected after-page carries the complete original row and coverage.
        let mut update = tool.clone();
        update.entries[0].seq_end = 6;
        update.entries[0].source_seq_ranges[1].end_seq = 6;
        update.end_cursor.as_mut().unwrap().seq = 6;
        update.window.max_seq = 6;
        update.entries[0].item = json!({"type":"tool_call","callId":"tool","status":"completed"});
        h.apply(&update).unwrap();
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[&2].item["text"], "assistant");
        assert_eq!(h.entries.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        h.apply(&tool).unwrap();
        assert_eq!(h.entries[&1].item["status"], "completed");
    }

    fn oracle() -> Value {
        serde_json::from_str(include_str!("testdata/timeline-live.json")).unwrap()
    }
    fn assert_entries(h: &History, expected: &Value) {
        let expected = expected.as_array().unwrap();
        assert_eq!(h.entries.len(), expected.len());
        for (actual, expected) in h.entries.values().zip(expected) {
            assert_eq!(actual.seq_start, expected["seqStart"].as_u64().unwrap());
            assert_eq!(actual.seq_end, expected["seqEnd"].as_u64().unwrap());
            assert_eq!(actual.item, expected["item"]);
            assert_eq!(actual.timestamp.as_deref(), expected["timestamp"].as_str());
            assert_eq!(actual.turn_id.as_deref(), expected["turnId"].as_str());
            assert_eq!(
                actual
                    .source_seq_ranges
                    .iter()
                    .map(|r| (r.start_seq, r.end_seq))
                    .collect::<Vec<_>>(),
                expected["sourceSeqRanges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| (
                        r["startSeq"].as_u64().unwrap(),
                        r["endSeq"].as_u64().unwrap()
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn live_projection_matches_real_daemon_after_every_interleaved_event() {
        let fixture = oracle();
        let mut h = History::default();
        h.apply(&serde_json::from_value(fixture["initial"].clone()).unwrap())
            .unwrap();
        assert!(h.is_current());
        assert_eq!(h.cursor().unwrap().seq, 0);
        for snapshot in fixture["snapshots"].as_array().unwrap() {
            assert!(matches!(
                h.apply_live(&snapshot["payload"]).unwrap(),
                LiveChange::Applied { .. }
            ));
            assert_entries(&h, &snapshot["entries"]);
        }
        // An older complete page cannot roll back a newer live row or its timestamp.
        let old = &fixture["snapshots"][2];
        let older: Page=serde_json::from_value(json!({"requestId":"old","agentId":"a","epoch":"e","reset":false,"entries":old["entries"],"window":{"maxSeq":3},"startCursor":{"epoch":"e","seq":1},"endCursor":{"epoch":"e","seq":3},"hasOlder":false,"hasNewer":false,"error":null})).unwrap();
        h.apply(&older).unwrap();
        assert_entries(
            &h,
            &fixture["snapshots"].as_array().unwrap().last().unwrap()["entries"],
        );
    }
    #[test]
    fn incomplete_projection_does_not_downgrade_a_tool_snapshot_from_the_future() {
        let fixture = oracle();
        let f = &fixture["future"];
        let mut h = History::default();
        h.apply(&serde_json::from_value(f["before"].clone()).unwrap())
            .unwrap();
        h.apply(&serde_json::from_value(f["partial"].clone()).unwrap())
            .unwrap();
        assert!(!h.is_current());
        assert_eq!(h.cursor().unwrap().seq, 5);
        assert_eq!(h.entries[&1].item["status"], "completed");
        assert_eq!(
            h.apply_live(&live(
                "e",
                6,
                json!({"type":"tool_call","callId":"reused","status":"running"})
            ))
            .unwrap(),
            LiveChange::Reconcile
        );
        assert_eq!(h.entries[&1].item["status"], "completed");
        h.apply(&serde_json::from_value(f["rest"].clone()).unwrap())
            .unwrap();
        assert!(h.is_current());
        assert_eq!(h.cursor().unwrap().seq, 12);
        assert_entries(&h, &f["final"]["entries"]);
    }
    #[test]
    fn live_gaps_epoch_changes_duplicates_and_bad_text_do_not_corrupt_history() {
        let mut h = History::default();
        assert_eq!(
            h.apply_live(&live(
                "e",
                1,
                json!({"type":"assistant_message","text":"early"})
            ))
            .unwrap(),
            LiveChange::Reconcile
        );
        h.apply(&page("e", 1, 1, "original")).unwrap();
        assert_eq!(
            h.apply_live(&live(
                "e",
                1,
                json!({"type":"assistant_message","text":"duplicate"})
            ))
            .unwrap(),
            LiveChange::Ignored
        );
        assert_eq!(
            h.apply_live(&live(
                "e",
                3,
                json!({"type":"assistant_message","text":"gap"})
            ))
            .unwrap(),
            LiveChange::Reconcile
        );
        assert_eq!(
            h.apply_live(&live(
                "new",
                2,
                json!({"type":"assistant_message","text":"new epoch"})
            ))
            .unwrap(),
            LiveChange::Reconcile
        );
        assert!(
            h.apply_live(&live("e", 2, json!({"type":"assistant_message","text":42})))
                .is_err()
        );
        assert_eq!(
            h.apply_live(&json!({"event":{"type":"turn_completed"}}))
                .unwrap(),
            LiveChange::Ignored
        );
        assert_eq!(h.apply_live(&json!({"event":{"type":"timeline","item":{"type":"assistant_message","text":"unpositioned"}}})).unwrap(),LiveChange::Reconcile);
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[&1].item["text"], "original");
        assert_eq!(h.cursor().unwrap().seq, 1);
    }
    #[test]
    fn buffered_live_suffix_replays_after_initial_and_partial_history_without_duplication() {
        let f = oracle();
        let mut h = History::default();
        let mut b = LiveBuffer::default();
        let requested_at = b.generation();
        for snapshot in &f["snapshots"].as_array().unwrap()[..3] {
            b.push(&snapshot["payload"]);
        }
        h.apply(&serde_json::from_value(f["initial"].clone()).unwrap())
            .unwrap();
        assert_eq!(
            b.finish_snapshot(&mut h, requested_at).unwrap(),
            SyncNeed::Current
        );
        assert_entries(&h, &f["snapshots"][2]["entries"]);
        assert!(b.events.is_empty());
        // A page overlapping the beginning of the buffer consumes only covered source rows.
        let mut h = History::default();
        let mut b = LiveBuffer::default();
        for snapshot in &f["snapshots"].as_array().unwrap()[..3] {
            b.push(&snapshot["payload"]);
        }
        let first:Page=serde_json::from_value(json!({"requestId":"first","agentId":"a","epoch":"e","reset":false,"entries":f["snapshots"][0]["entries"],"window":{"maxSeq":1},"startCursor":{"epoch":"e","seq":1},"endCursor":{"epoch":"e","seq":1},"hasOlder":false,"hasNewer":false,"error":null})).unwrap();
        h.apply(&first).unwrap();
        assert_eq!(b.finish_snapshot(&mut h, 0).unwrap(), SyncNeed::Current);
        assert_entries(&h, &f["snapshots"][2]["entries"]);
    }
    #[test]
    fn replay_budget_or_missing_rows_requires_authoritative_catchup() {
        let mut h = History::default();
        h.apply(&page("e", 1, 1, "base")).unwrap();
        let mut b = LiveBuffer::default();
        for seq in 2..=140 {
            b.push(&live(
                "e",
                seq,
                json!({"type":"assistant_message","text":"x"}),
            ));
        }
        assert!(b.events.len() <= LiveBuffer::MAX_EVENTS);
        assert!(b.bytes <= LiveBuffer::MAX_BYTES);
        assert_eq!(b.finish_snapshot(&mut h, 0).unwrap(), SyncNeed::After);
        assert_eq!(h.cursor().unwrap().seq, 1);
        let mut complete = page("e", 1, 140, "complete");
        complete.entries[0].turn_id = Some("turn".into());
        h.apply(&complete).unwrap();
        assert_eq!(
            b.finish_snapshot(&mut h, b.generation()).unwrap(),
            SyncNeed::Current
        );
        let giant = live(
            "e",
            141,
            json!({"type":"assistant_message","text":"x".repeat(LiveBuffer::MAX_BYTES)}),
        );
        b.push(&giant);
        assert!(b.events.is_empty());
        assert_eq!(b.bytes, 0);
        assert_eq!(
            b.finish_snapshot(&mut h, b.generation()).unwrap(),
            SyncNeed::After
        );
    }
    #[test]
    fn a_new_epoch_during_fetch_gets_a_new_snapshot_and_old_buffer_can_be_retired() {
        let mut h = History::default();
        h.apply(&page("e", 1, 1, "base")).unwrap();
        let mut b = LiveBuffer::default();
        let requested_at = b.generation();
        b.push(&live(
            "new",
            1,
            json!({"type":"assistant_message","text":"new"}),
        ));
        assert_eq!(
            b.finish_snapshot(&mut h, requested_at).unwrap(),
            SyncNeed::Tail
        );
        h.apply(&page("new", 1, 1, "new")).unwrap();
        assert_eq!(
            b.finish_snapshot(&mut h, b.generation()).unwrap(),
            SyncNeed::Current
        );
        b.push(&live(
            "retired",
            1,
            json!({"type":"assistant_message","text":"old frame"}),
        ));
        let requested_at = b.generation();
        assert_eq!(
            b.finish_snapshot(&mut h, requested_at).unwrap(),
            SyncNeed::Current
        );
        assert_eq!(h.entries[&1].item["text"], "new");
    }
}
