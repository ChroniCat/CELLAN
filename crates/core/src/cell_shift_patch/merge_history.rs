use super::*;
use crate::merges::{Merges, groups_overlap};

type Positions = Vec<(Option<usize>, Option<usize>)>;

/// An imported document may contain overlapping merges whose position in
/// the list decides which one wins. Ordinary merges keep only changed slots.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct MergeHistory {
    positions: Positions,
    overlapping: Option<(Vec<Rect>, Vec<Rect>)>,
}

impl MergeHistory {
    pub(super) fn capture(
        before: &Merges,
        after: &Merges,
        items: &[ShiftPoint<Option<Rect>>],
    ) -> Self {
        let positions = |merges: &Merges| -> HashMap<_, _> {
            merges
                .iter()
                .enumerate()
                .map(|(i, r)| ((r.0, r.1), i))
                .collect()
        };
        let (a, b) = (positions(before), positions(after));
        let changed: BTreeSet<_> = items.iter().map(|p| (p.row, p.col)).collect();
        let overlaps_unchanged = |merges: &Merges| {
            groups_overlap(
                merges
                    .iter()
                    .copied()
                    .filter(|r| !changed.contains(&(r.0, r.1))),
                merges
                    .iter()
                    .copied()
                    .filter(|r| changed.contains(&(r.0, r.1))),
            )
        };
        if before.iter().chain(after.iter()).any(|r| !valid_rect(r))
            || a.len() != before.len()
            || b.len() != after.len()
            || overlaps_unchanged(before)
            || overlaps_unchanged(after)
        {
            return Self {
                positions: Vec::new(),
                overlapping: Some((before.to_vec(), after.to_vec())),
            };
        }
        Self {
            positions: items
                .iter()
                .map(|p| {
                    (
                        a.get(&(p.row, p.col)).copied(),
                        b.get(&(p.row, p.col)).copied(),
                    )
                })
                .collect(),
            overlapping: None,
        }
    }

    pub(super) fn extra_entries(&self) -> usize {
        self.overlapping
            .as_ref()
            .map_or(0, |(a, b)| a.len() + b.len())
    }

    pub(super) fn valid(&self, changes: usize) -> bool {
        if let Some((a, b)) = &self.overlapping {
            self.positions.is_empty() && a.iter().chain(b).all(valid_rect)
        } else {
            self.positions.len() == changes
        }
    }

    pub(super) fn matches(
        &self,
        sh: &Sheet,
        items: &[ShiftPoint<Option<Rect>>],
        forward: bool,
    ) -> bool {
        if let Some((a, b)) = &self.overlapping {
            return sh.merges == *pick(a, b, !forward);
        }
        if items.is_empty() {
            return true;
        }
        if !map_matches(&merge_map(sh), items, forward) {
            return false;
        }
        let changed: BTreeSet<_> = items.iter().map(|p| (p.row, p.col)).collect();
        !groups_overlap(
            sh.merges
                .iter()
                .copied()
                .filter(|r| !changed.contains(&(r.0, r.1))),
            items
                .iter()
                .filter_map(|p| *pick(&p.before, &p.after, forward)),
        )
    }

    pub(super) fn install_merges(
        &self,
        merges: &mut Merges,
        items: &[ShiftPoint<Option<Rect>>],
        forward: bool,
    ) {
        if let Some((a, b)) = &self.overlapping {
            *merges = pick(a, b, forward).clone().into();
        } else {
            if items.is_empty() {
                return;
            }
            let changed: BTreeSet<_> = items.iter().map(|p| (p.row, p.col)).collect();
            let mut kept = merges
                .iter()
                .copied()
                .filter(|r| !changed.contains(&(r.0, r.1)));
            let mut place: Vec<_> = items
                .iter()
                .zip(&self.positions)
                .filter_map(|(p, (a, b))| {
                    Some((
                        (*pick(a, b, forward))?,
                        (*pick(&p.before, &p.after, forward))?,
                    ))
                })
                .collect();
            place.sort_unstable_by_key(|(i, _)| *i);
            let mut out = Vec::with_capacity(merges.len());
            for (i, r) in place {
                out.extend((&mut kept).take(i.saturating_sub(out.len())));
                out.push(r);
            }
            out.extend(kept);
            *merges = out.into();
        }
    }

    pub(super) fn install(
        &self,
        sh: &mut Sheet,
        items: &[ShiftPoint<Option<Rect>>],
        forward: bool,
    ) {
        self.install_merges(&mut sh.merges, items, forward);
        sh.merges_dirty = true;
    }
}
