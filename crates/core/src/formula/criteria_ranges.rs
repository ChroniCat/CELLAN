//! Declared geometry, aligned reads, and sparse scans for multi-criteria aggregates.

use super::{
    Arg, Calc, CriteriaKey, ErrKind, FValue, FuncId, MAX_ARRAY_CELLS, Node, RangeKey,
    checked_array_len, conditional_index, functions,
};

impl Calc<'_> {
    pub(super) fn ifs_range_key(&mut self, node: &Node, si: usize) -> Option<RangeKey> {
        if let Some(rr) = self.resolve_range_node(node, si) {
            if rr.is_ext() || rr.is_3d() {
                return None;
            }
            let sheet = self.sheet_index(rr.sheet.as_deref(), si).ok()?;
            if rr.r1() < 0 || rr.c1() < 0 || rr.r2() > 1_048_575 || rr.c2() > 16_383 {
                return None;
            }
            return Some((sheet, rr.r1(), rr.c1(), rr.r2(), rr.c2()));
        }
        // These expressions produce references; preserve their full dimensions
        // before evaluating them as arrays trimmed to used cells.
        let range = match node {
            Node::DynRange(..) | Node::Intersect(..) | Node::Name(..) | Node::ScopedName { .. } => {
                self.resolve_node_rect(node, si).ok()?
            }
            Node::Func(name, _)
                if matches!(
                    name.id(),
                    FuncId::Offset | FuncId::Indirect | FuncId::Index | FuncId::Choose
                ) =>
            {
                self.resolve_node_rect(node, si).ok()?
            }
            _ => return None,
        };
        let (sheet, r1, c1, r2, c2) = range;
        if r1 < 0 || c1 < 0 || r2 > 1_048_575 || c2 > 16_383 {
            return None;
        }
        if let Some(watch) = self.deps_watch {
            watch.record_dynamic_watch(
                (self.cur_sheet, self.cur.0, self.cur.1),
                sheet,
                r1,
                c1,
                r2,
                c2,
            );
        }
        Some(range)
    }

    fn criteria_range_arg(&mut self, key: RangeKey) -> Result<Arg, ErrKind> {
        let rows = (key.3 - key.1 + 1) as usize;
        let cols = (key.4 - key.2 + 1) as usize;
        let len = checked_array_len(rows, cols, MAX_ARRAY_CELLS)?;
        let values = self.materialize_range(key, len)?;
        Ok(Arg::Range { values, rows, cols })
    }

    pub(super) fn materialize_ifs_args(&mut self, key: &CriteriaKey) -> Result<Vec<Arg>, ErrKind> {
        let (amounts, conditions) = key.split_ranges().ok_or(ErrKind::Value)?;
        let mut args = Vec::with_capacity(key.ranges.len() + conditions.len());
        if let Some(range) = amounts {
            args.push(self.criteria_range_arg(range)?);
        }
        for (&range, criterion) in conditions.iter().zip(&key.criteria) {
            args.push(self.criteria_range_arg(range)?);
            args.push(Arg::Val(FValue::Text(criterion.as_str().into())));
        }
        Ok(args)
    }

    /// Compare declared shapes before clipping. Retain the same row and column
    /// offsets in every range, even when the sheets have different used bounds.
    pub(super) fn align_ifs_ranges(&mut self, ranges: &mut [RangeKey]) -> Result<u64, ErrKind> {
        let &(_, r1, c1, r2, c2) = ranges.first().ok_or(ErrKind::Value)?;
        let rows = r2 - r1 + 1;
        let cols = c2 - c1 + 1;
        if ranges
            .iter()
            .any(|&(_, r, c, end_r, end_c)| (end_r - r + 1, end_c - c + 1) != (rows, cols))
        {
            return Err(ErrKind::Value);
        }
        let area = rows as u64 * cols as u64;
        if area <= 200_000 && cols != 16_384 {
            return Ok(0);
        }
        let (mut used_rows, mut used_cols) = (1, 1);
        for &(sheet, r, c, _, _) in ranges.iter() {
            let (bottom, right) = self.sheet_bounds(sheet);
            used_rows = used_rows.max((i64::from(bottom) - r + 1).min(rows));
            used_cols = used_cols.max((i64::from(right) - c + 1).min(cols));
        }
        for range in ranges {
            range.3 = range.1 + used_rows - 1;
            range.4 = range.2 + used_cols - 1;
        }
        Ok(area - used_rows as u64 * used_cols as u64)
    }

    /// Sparse two-dimensional ranges can have occupied cells in opposite corners on
    /// different sheets. Match relative positions without materializing their bounding box.
    pub(super) fn scan_sparse_ifs(&mut self, key: &CriteriaKey, area: usize) -> Option<FValue> {
        let (amounts, conditions) = key.split_ranges()?;
        let criteria: Vec<_> = key
            .criteria
            .iter()
            .map(|c| functions::Crit::parse(c))
            .collect();
        let nonblank = criteria.iter().position(|c| !c.matches(&FValue::Empty));
        // Numeric aggregates only need occupied amount cells. COUNTIFS can start
        // at one nonblank condition, or count the untouched blanks as a group.
        let driver = amounts.or_else(|| nonblank.map(|i| conditions[i]));
        let drivers = driver.as_ref().map_or(conditions, std::slice::from_ref);
        let wb = self.wb;
        let mut offsets: Vec<_> = drivers
            .iter()
            .map(|&(sheet, r, c, end_r, end_c)| {
                let (r, c) = (r as u32, c as u32);
                wb.sheets[sheet]
                    .iter_rect_views((r, c, end_r as u32, end_c as u32))
                    .map(move |(pos, _)| (pos.row - r, pos.col - c))
                    .peekable()
            })
            .collect();
        let mut visited = 0usize;
        let mut hits = 0usize;
        let mut total = 0.0;
        let mut best: Option<f64> = None;
        // iter_rect_views yields row-major order, preserving summation and error order.
        while let Some(pos) = offsets.iter_mut().filter_map(|it| it.peek().copied()).min() {
            for it in &mut offsets {
                if it.peek() == Some(&pos) {
                    it.next();
                }
            }
            visited += 1;
            if self.check_abort() {
                return Some(FValue::Error(ErrKind::Value));
            }
            let mut read = |(s, r, c, _, _): RangeKey| {
                self.range_reads += 1;
                self.cell_value(s, r as u32 + pos.0, c as u32 + pos.1)
                    .scalar()
            };
            let matches = conditions
                .iter()
                .zip(&criteria)
                .all(|(&range, crit)| crit.matches(&read(range)));
            if matches {
                match amounts.map(read) {
                    None => hits += 1,
                    Some(FValue::Number(n)) => {
                        hits += 1;
                        match key.function {
                            4 => best = Some(best.map_or(n, |v| v.min(n))),
                            5 => best = Some(best.map_or(n, |v| v.max(n))),
                            _ => total += n,
                        }
                    }
                    Some(FValue::Error(e)) => return Some(FValue::Error(e)),
                    _ => {}
                }
            }
            if self.requested.is_some() {
                return Some(FValue::Error(ErrKind::Ref));
            }
        }
        if driver.is_none() {
            hits += area - visited;
        }
        Some(conditional_index::MatchIndexes::finish_aggregate(
            key.function,
            hits,
            total,
            best,
        ))
    }

    /// Computed arrays already know their dimensions. Compare them to declared
    /// reference rectangles before reading any potentially large reference.
    pub(super) fn mixed_ifs_args(
        &mut self,
        id: FuncId,
        nodes: &[Node],
        si: usize,
    ) -> Result<Vec<Arg>, ErrKind> {
        let mut args = Vec::with_capacity(nodes.len());
        let mut references = Vec::new();
        let mut shape = None;
        let mut mismatch = false;
        for (i, node) in nodes.iter().enumerate() {
            let is_range = if id == FuncId::CountIfs {
                i.is_multiple_of(2)
            } else {
                i == 0 || i % 2 == 1
            };
            let dims = if is_range && let Some(key) = self.ifs_range_key(node, si) {
                references.push((i, key));
                args.push(Arg::Val(FValue::Empty));
                ((key.3 - key.1 + 1) as usize, (key.4 - key.2 + 1) as usize)
            } else {
                let value = self.eval_node(node, si);
                if let FValue::Error(error) = value {
                    return Err(error);
                }
                let arg = Arg::Val(value);
                let dims = functions::range_dims(&arg);
                if !is_range {
                    arg.first().to_text()?;
                }
                args.push(arg);
                dims
            };
            if self.requested.is_some() {
                return Err(ErrKind::Ref);
            }
            if self.aborted {
                return Err(ErrKind::Value);
            }
            if is_range && shape.replace(dims).is_some_and(|prior| prior != dims) {
                mismatch = true;
            }
        }
        if mismatch {
            return Err(ErrKind::Value);
        }
        for (i, key) in references {
            args[i] = self.criteria_range_arg(key)?;
        }
        Ok(args)
    }
}
