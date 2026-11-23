use super::RangeKey;
use crate::store::CellVersion;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub(super) type Index = Arc<HashMap<String, usize>>;
type Key = (RangeKey, bool);

#[derive(Debug)]
struct Entry {
    version: CellVersion,
    key: Key,
    bytes: usize,
    index: Index,
}

/// Owned by the sheet; clones can reuse immutable results, including from another worker.
/// CellVersion distinguishes divergent edits to cloned snapshots, not just a revision counter.
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedLookup(Arc<Mutex<VecDeque<Entry>>>);

impl SharedLookup {
    pub(super) fn get(&self, version: &CellVersion, key: Key) -> Option<Index> {
        let entries = self.0.lock().ok()?;
        entries
            .iter()
            .rev()
            .find(|e| e.version == *version && e.key == key)
            .map(|e| e.index.clone())
    }

    pub(super) fn insert(&self, version: &CellVersion, key: Key, index: Index) {
        const BYTES: usize = 8 * 1024 * 1024;
        let bytes = index.capacity() * (std::mem::size_of::<(String, usize)>() + 2)
            + index.keys().map(String::capacity).sum::<usize>();
        if bytes > BYTES {
            return;
        }
        let Ok(mut entries) = self.0.lock() else {
            return;
        };
        if entries
            .iter()
            .any(|e| e.version == *version && e.key == key)
        {
            return;
        }
        while entries.len() >= 4 || bytes + entries.iter().map(|e| e.bytes).sum::<usize>() > BYTES {
            entries.pop_front();
        }
        entries.push_back(Entry {
            version: version.clone(),
            key,
            bytes,
            index,
        });
    }
}
