//! The receiver expands a sort against its own immutable source. Only the exact
//! operations (including ownership timestamps, cached results and styles) whose
//! digest matches the sender are accepted. A miss requests ordinary cell frames.
use sha2::{Digest, Sha256};
use sheetcore::style::CellStyle;
use sheetcore::sync::{Op, SortPermutation, ValueWire};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SortTarget {
    pub document: u64,
    pub sheet: usize,
    pub identity: u64,
    pub coordinates: u64,
}

impl SortTarget {
    pub fn valid(&self, document: u64, workbook: &sheetcore::Workbook) -> bool {
        self.document == document
            && workbook.sheets.get(self.sheet).is_some_and(|sheet| {
                self.identity == sheet.identity && self.coordinates == sheet.coordinate_revision()
            })
    }
}

type StyleItem = (u32, u32, CellStyle);

#[derive(Debug)]
pub(crate) struct SortBatch {
    pub plan: SortPermutation,
    pub ops: Vec<Arc<Op>>,
    pub styles: Vec<StyleItem>,
    checksum: std::sync::OnceLock<[u8; 32]>,
}

impl SortBatch {
    pub fn new(plan: SortPermutation, ops: Vec<Arc<Op>>, styles: Vec<StyleItem>) -> Self {
        Self {
            plan,
            ops,
            styles,
            checksum: Default::default(),
        }
    }

    pub fn offer(&self) -> SortPermutation {
        let mut plan = self.plan.clone();
        plan.digest = *self.checksum.get_or_init(|| digest(&self.ops, &self.styles));
        plan
    }

    pub fn expand(
        plan: &SortPermutation,
        mut read: impl FnMut(u32, u32) -> (ValueWire, Option<String>, Arc<CellStyle>),
    ) -> Option<Self> {
        if !plan.valid() {
            return None;
        }
        let (r1, c1, r2, c2) = plan.region;
        let (first, start, end) = if plan.horizontal {
            (c1, r1, r2)
        } else {
            (r1, c1, c2)
        };
        let coords = |line, offset| {
            if plan.horizontal {
                (offset, line)
            } else {
                (line, offset)
            }
        };
        let mut ops = Vec::new();
        let mut styles = Vec::new();
        for (i, &src) in plan.order.iter().enumerate() {
            let dst = first + i as u32;
            if dst == src {
                continue;
            }
            for offset in start..=end {
                let (r, c) = coords(src, offset);
                let (row, col) = coords(dst, offset);
                let (value, formula, style) = read(r, c);
                let (before, prev_formula, prev_style) = read(row, col);
                let formula = formula.map(|f| {
                    let shift = i64::from(dst) - i64::from(src);
                    sheetcore::refs::translate(
                        &f,
                        if plan.horizontal { 0 } else { shift },
                        if plan.horizontal { shift } else { 0 },
                        &plan.name,
                    )
                });
                if before == value && formula == prev_formula && style == prev_style {
                    continue;
                }
                if style != prev_style {
                    styles.push((row, col, (*style).clone()));
                }
                let ts = plan.first_ts.checked_add(ops.len() as u64)?;
                if ts > sheetcore::sync::MAX_LOGICAL_TS {
                    return None;
                }
                ops.push(Arc::new(Op {
                    sheet: plan.sheet,
                    row,
                    col,
                    value,
                    formula,
                    ts,
                    site: plan.site,
                }));
            }
        }
        (digest(&ops, &styles) == plan.digest).then(|| Self {
            plan: plan.clone(),
            ops,
            styles,
            checksum: plan.digest.into(),
        })
    }
}

fn digest(ops: &[Arc<Op>], styles: &[StyleItem]) -> [u8; 32] {
    fn bytes(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    let mut hash = Sha256::new();
    hash.update((ops.len() as u64).to_le_bytes());
    for op in ops {
        hash.update((op.sheet as u64).to_le_bytes());
        hash.update(op.row.to_le_bytes());
        hash.update(op.col.to_le_bytes());
        hash.update(op.site.to_le_bytes());
        hash.update(op.ts.to_le_bytes());
        match &op.value {
            ValueWire::Empty => hash.update([0]),
            ValueWire::Number(n) => {
                hash.update([1]);
                hash.update(n.to_bits().to_le_bytes());
            }
            ValueWire::Text(s) => {
                hash.update([2]);
                bytes(&mut hash, s.as_bytes());
            }
            ValueWire::Bool(b) => hash.update([3, *b as u8]),
            ValueWire::Error(s) => {
                hash.update([4]);
                bytes(&mut hash, s.as_bytes());
            }
        }
        hash.update([u8::from(op.formula.is_some())]);
        if let Some(f) = &op.formula {
            bytes(&mut hash, f.as_bytes());
        }
    }
    for (r, c, style) in styles {
        hash.update(r.to_le_bytes());
        hash.update(c.to_le_bytes());
        bytes(&mut hash, &serde_json::to_vec(style).expect("serializable style"));
    }
    hash.finalize().into()
}

pub(crate) fn from_sheet(
    sh: &sheetcore::Sheet,
    palette: &sheetcore::style::StyleTable,
    plan: &SortPermutation,
) -> Option<SortBatch> {
    let mut styles = std::collections::HashMap::new();
    SortBatch::expand(plan, |r, c| {
        let style = styles
            .entry(sh.style_of(r, c))
            .or_insert_with_key(|id| Arc::new(palette.get(*id).clone()));
        let cell = sh.get(r, c);
        (
            cell.map_or(ValueWire::Empty, |v| (&v.value).into()),
            cell.and_then(|v| v.formula.as_deref().map(str::to_owned)),
            style.clone(),
        )
    })
}
