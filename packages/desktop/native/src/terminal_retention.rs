use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub struct TerminalCache<T> {
    capacity: usize,
    entries: BTreeMap<String, T>,
    order: VecDeque<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct TerminalBusy;
impl<T> TerminalCache<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &T)> {
        self.entries.iter()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.entries.retain(|id, _| keep(id));
        self.order.retain(|id| self.entries.contains_key(id));
    }
}
impl<T: Clone> TerminalCache<T> {
    pub fn open(
        &mut self,
        id: &str,
        protected: &BTreeSet<String>,
        make: impl FnOnce() -> T,
    ) -> Result<T, TerminalBusy> {
        if let Some(value) = self.entries.get(id) {
            self.order.retain(|old| old != id);
            self.order.push_back(id.into());
            return Ok(value.clone());
        }
        let evict = if self.entries.len() >= self.capacity {
            Some(
                self.order
                    .iter()
                    .find(|old| !protected.contains(*old))
                    .cloned()
                    .ok_or(TerminalBusy)?,
            )
        } else {
            None
        };
        let value = make();
        if let Some(id) = evict {
            self.entries.remove(&id);
            self.order.retain(|old| old != &id);
        }
        self.entries.insert(id.into(), value.clone());
        self.order.push_back(id.into());
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_tabs_reuse_state_and_evict_the_least_recent_view_at_the_limit() {
        let mut cache = TerminalCache::new(3);
        let pending = BTreeSet::new();
        for (id, value) in [("a", 1), ("b", 2), ("c", 3)] {
            assert_eq!(cache.open(id, &pending, || value), Ok(value));
        }
        assert_eq!(
            cache.open("a", &pending, || panic!("a must be reused")),
            Ok(1)
        );
        assert_eq!(cache.open("d", &pending, || 4), Ok(4));
        assert_eq!(
            cache
                .iter()
                .map(|(id, value)| (id.as_str(), *value))
                .collect::<Vec<_>>(),
            vec![("a", 1), ("c", 3), ("d", 4)]
        );
    }
    #[test]
    fn opening_or_unsent_terminals_are_not_evicted_and_a_full_busy_cache_is_unchanged() {
        let mut cache = TerminalCache::new(2);
        let mut protected = BTreeSet::new();
        cache.open("a", &protected, || 1).unwrap();
        cache.open("b", &protected, || 2).unwrap();
        protected.insert("a".into());
        protected.insert("b".into());
        assert_eq!(
            cache.open("c", &protected, || panic!(
                "must not create a view when busy"
            )),
            Err(TerminalBusy)
        );
        assert_eq!(
            cache
                .iter()
                .map(|(id, value)| (id.as_str(), *value))
                .collect::<Vec<_>>(),
            vec![("a", 1), ("b", 2)]
        );
        protected.remove("a");
        cache.open("c", &protected, || 3).unwrap();
        assert_eq!(
            cache.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
    }
    #[test]
    fn authoritative_removal_and_workspace_clear_release_retained_membership() {
        let mut cache = TerminalCache::new(3);
        for id in ["a", "b", "c"] {
            cache.open(id, &BTreeSet::new(), || id.to_owned()).unwrap();
        }
        cache.retain(|id| id != "b");
        assert_eq!(cache.values().count(), 2);
        cache
            .open("b", &BTreeSet::new(), || "new b".into())
            .unwrap();
        assert_eq!(
            cache.open("b", &BTreeSet::new(), || panic!()),
            Ok("new b".into())
        );
        cache.clear();
        assert_eq!(cache.values().count(), 0);
        assert_eq!(
            cache.open("a", &BTreeSet::new(), || "new a".into()),
            Ok("new a".into())
        );
    }
}
