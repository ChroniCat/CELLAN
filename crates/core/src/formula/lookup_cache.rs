use super::RangeKey;
use crate::store::CellVersion;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub(super) type Index = Arc<HashMap<String, usize>>;
type Key = (RangeKey, bool);

#[derive(Debug)]
enum Keys {
    Column(Vec<crate::cf::PageSlice>),
    Row(Vec<crate::Cell>),
}

// Extending a sparse slab outside the key range can introduce allocated blank
// slots within it. Those slots do not change a lookup.
fn occupied_keys(
    pages: &[crate::cf::PageSlice],
) -> impl Iterator<Item = ((u32, usize), &crate::Cell)> {
    pages.iter().flat_map(|p| {
        (p.start..p.end).filter_map(move |row| {
            let cell = &p.cells[row];
            (!cell.is_blank()).then_some(((p.page, row), cell))
        })
    })
}

impl Keys {
    fn capture(sheet: &crate::Sheet, key: Key) -> Self {
        let ((_, r1, c1, r2, c2), vertical) = key;
        if vertical {
            Self::Column(sheet.cf_pages((r1 as u32, c1 as u32, r2 as u32, c1 as u32)))
        } else {
            Self::Row(
                (c1..=c2)
                    .map(|c| sheet.get(r1 as u32, c as u32).cloned().unwrap_or_default())
                    .collect(),
            )
        }
    }

    fn matches(&self, sheet: &crate::Sheet, key: Key) -> bool {
        let ((_, r1, c1, r2, c2), _) = key;
        match self {
            Self::Column(old) => {
                let now = sheet.cf_pages((r1 as u32, c1 as u32, r2 as u32, c1 as u32));
                old == &now || occupied_keys(old).eq(occupied_keys(&now))
            }
            Self::Row(old) => (c1..=c2).zip(old).all(|(c, v)| {
                sheet
                    .get(r1 as u32, c as u32)
                    .map_or_else(|| v.is_blank(), |cell| cell == v)
            }),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Column(pages) => pages
                .iter()
                .map(|p| {
                    p.cells.capacity() * size_of::<crate::Cell>()
                        + size_of::<crate::cf::PageSlice>()
                        + p.cells
                            .iter()
                            .map(crate::undo::cell_heap_bytes)
                            .sum::<usize>()
                })
                .sum(),
            Self::Row(cells) => {
                cells.capacity() * size_of::<crate::Cell>()
                    + cells
                        .iter()
                        .map(crate::undo::cell_heap_bytes)
                        .sum::<usize>()
            }
        }
    }
}

#[derive(Debug)]
struct Entry {
    inputs: Keys,
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
    pub(super) fn get(&self, sheet: &crate::Sheet, key: Key) -> Option<Index> {
        let mut entries = self.0.lock().ok()?;
        for entry in entries.iter_mut().rev().filter(|e| e.key == key) {
            if entry.version == *sheet.cell_version() || entry.inputs.matches(sheet, key) {
                entry.version = sheet.cell_version().clone();
                return Some(entry.index.clone());
            }
        }
        None
    }

    pub(super) fn insert(&self, sheet: &crate::Sheet, key: Key, index: Index) {
        const BYTES: usize = 64 * 1024 * 1024;
        let version = sheet.cell_version();
        let inputs = Keys::capture(sheet, key);
        let bytes = inputs.bytes()
            + index.capacity() * (std::mem::size_of::<(String, usize)>() + 2)
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
            inputs,
            version: version.clone(),
            key,
            bytes,
            index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Sheet, Value};

    #[test]
    fn mutations_outside_lookup_keys_reuse_the_index_in_divergent_snapshots() {
        for vertical in [false, true] {
            let value = |v| Cell {
                value: Value::Number(v),
                formula: None,
            };
            let mut sheet = Sheet::new("keys");
            sheet.set(0, 0, value(1.));
            let cache = SharedLookup::default();
            let key = ((0, 0, 0, 100, 3), vertical);
            let index = Index::default();
            cache.insert(&sheet, key, index.clone());
            let mut changed = sheet.clone();
            changed.set(30, 2, value(7.));
            changed.set(150, 0, value(8.));
            assert!(Arc::ptr_eq(&index, &cache.get(&changed, key).unwrap()));
            let (r, c) = if vertical { (30, 0) } else { (0, 2) };
            changed.set(r, c, value(3.));
            assert!(cache.get(&changed, key).is_none());
            assert!(Arc::ptr_eq(&index, &cache.get(&sheet, key).unwrap()));
        }
    }
}
