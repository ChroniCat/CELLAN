//! 协作内核：基于「每格最后写入者胜出」的操作同步。
//!
//! 设计取舍：表格的最小冲突单位是单元格，同一格并发写入时 Excel 也只保留一个结果。
//! 因此不需要通用 CRDT（Yjs 那类字符级结构），用 (Lamport 时间戳, 站点号) 决定胜负即可，
//! 收敛性可证：全序唯一，所有副本合并同样的集合得到同样结果。
//!
//! 拓扑：星形。谁开文档谁当宿主，其余人连他；宿主收到操作后转发给其他连接。

use crate::model::{Cell, Sheet, Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ValueWire {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
}

impl From<&Value> for ValueWire {
    fn from(v: &Value) -> Self {
        match v {
            Value::Empty => ValueWire::Empty,
            Value::Number(n) => ValueWire::Number(*n),
            Value::Text(s) => ValueWire::Text(s.to_string()),
            Value::Bool(b) => ValueWire::Bool(*b),
        }
    }
}

impl From<&ValueWire> for Value {
    fn from(v: &ValueWire) -> Self {
        match v {
            ValueWire::Empty => Value::Empty,
            ValueWire::Number(n) => Value::Number(*n),
            ValueWire::Text(s) => Value::Text(s.as_str().into()),
            ValueWire::Bool(b) => Value::Bool(*b),
        }
    }
}

/// 一次单元格写入
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Op {
    /// 工作表下标（0 起）。老版本没有这个字段，反序列化默认 0
    #[serde(default)]
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
    pub value: ValueWire,
    pub formula: Option<String>,
    pub ts: u64,
    pub site: u32,
}

impl Op {
    /// 全序比较：(ts, site)
    fn newer_than(&self, other: &Op) -> bool {
        (self.ts, self.site) > (other.ts, other.site)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cursor {
    pub site: u32,
    pub name: String,
    pub color: u8,
    #[serde(default)]
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Msg {
    /// 连上后宿主先发挑战码，客户端回 Hello 时带上 sha256(nonce + 口令)
    Challenge { nonce: u64 },
    Denied { reason: String },
    Hello { site: u32, name: String, doc: String, proof: String },
    Welcome { site: u32, doc: String, peers: Vec<(u32, String, u8)> },
    Snapshot { ops: Vec<Op> },
    Op(Op),
    CursorMsg(Cursor),
    Bye { site: u32 },
}

/// 一个副本的状态机（与网络无关，便于单元测试）
pub struct Peer {
    pub site: u32,
    pub clock: u64,
    /// 每个格子的当前胜出版本
    pub cells: HashMap<(usize, u32, u32), Op>,
    /// 已知的其他站点（site → 名字、颜色）
    pub peers: HashMap<u32, (String, u8)>,
}

impl Peer {
    pub fn new(site: u32) -> Self {
        Self { site, clock: 0, cells: HashMap::new(), peers: HashMap::new() }
    }

    /// 本地编辑：递增逻辑时钟并返回要广播的操作
    pub fn local_set(&mut self, sheet: usize, row: u32, col: u32, cell: &Cell) -> Op {
        self.clock += 1;
        let op = Op {
            sheet,
            row,
            col,
            value: ValueWire::from(&cell.value),
            formula: cell.formula.as_deref().map(|f| f.to_string()),
            ts: self.clock,
            site: self.site,
        };
        self.cells.insert((sheet, row, col), op.clone());
        op
    }

    /// 应用远端操作；返回是否真的改动了本地状态
    pub fn apply(&mut self, op: Op) -> bool {
        self.clock = self.clock.max(op.ts);
        let key = (op.sheet, op.row, op.col);
        match self.cells.get(&key) {
            Some(cur) if !op.newer_than(cur) => false,
            _ => {
                self.cells.insert(key, op);
                true
            }
        }
    }

    pub fn snapshot(&self) -> Vec<Op> {
        self.cells.values().cloned().collect()
    }

    pub fn merge_snapshot(&mut self, ops: Vec<Op>) -> Vec<Op> {
        let mut applied = Vec::new();
        for op in ops {
            if self.apply(op.clone()) {
                applied.push(op);
            }
        }
        applied
    }

    /// 把当前状态写回表格（清掉已被删除的格子）
    /// 只把指定的这几个格子写回对应工作表（协作增量用，避免整表重写）
    pub fn apply_list_to_workbook(&self, wb: &mut crate::model::Workbook, keys: &[(usize, u32, u32)]) {
        for (si, r, c) in keys {
            if let Some(op) = self.cells.get(&(*si, *r, *c)) {
                if let Some(sheet) = wb.sheets.get_mut(*si) {
                    let cell = Cell { value: Value::from(&op.value), formula: op.formula.as_deref().map(|f| f.into()) };
                    sheet.set(*r, *c, cell);
                }
            }
        }
    }

    /// 整本工作簿写回（加入房间时用）
    pub fn apply_all_to_workbook(&self, wb: &mut crate::model::Workbook) {
        for ((si, r, c), op) in self.cells.iter() {
            if let Some(sheet) = wb.sheets.get_mut(*si) {
                let cell = Cell { value: Value::from(&op.value), formula: op.formula.as_deref().map(|f| f.into()) };
                sheet.set(*r, *c, cell);
            }
        }
    }

    /// 兼容旧调用：只写第 0 张表
    pub fn apply_to_sheet(&self, sheet: &mut Sheet) {
        for ((si, r, c), op) in self.cells.iter() {
            if *si != 0 {
                continue;
            }
            let cell = Cell { value: Value::from(&op.value), formula: op.formula.as_deref().map(|f| f.into()) };
            sheet.set(*r, *c, cell);
        }
    }
}

/// 新加入者需要的完整快照大小上限（防止误传超大文档）
pub const MAX_SNAPSHOT_OPS: usize = 200_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_num(n: f64) -> Cell {
        Cell { value: Value::Number(n), formula: None }
    }

    #[test]
    fn two_peers_converge() {
        let mut a = Peer::new(1);
        let mut b = Peer::new(2);

        let op1 = a.local_set(0, 0, 0, &cell_num(1.0));
        let op2 = b.local_set(0, 0, 1, &cell_num(2.0));

        b.apply(op1.clone());
        a.apply(op2.clone());

        assert_eq!(a.cells.len(), 2);
        assert_eq!(b.cells.len(), 2);
        assert_eq!(a.cells[&(0, 0, 0)].value, b.cells[&(0, 0, 0)].value);
        assert_eq!(a.cells[&(0, 0, 1)].value, b.cells[&(0, 0, 1)].value);
    }

    #[test]
    fn concurrent_same_cell_resolves_deterministically() {
        let mut a = Peer::new(1);
        let mut b = Peer::new(2);
        // 双方都以为自己是第 1 次写入
        let opa = a.local_set(0, 5, 5, &cell_num(10.0));
        let opb = b.local_set(0, 5, 5, &cell_num(20.0));
        assert_eq!(opa.ts, 1);
        assert_eq!(opb.ts, 1);

        a.apply(opb.clone());
        b.apply(opa.clone());
        // 时间戳相同 → 比站点号，2 大，所以 B 的值胜出
        assert_eq!(a.cells[&(0, 5, 5)].value, ValueWire::Number(20.0));
        assert_eq!(b.cells[&(0, 5, 5)].value, ValueWire::Number(20.0));
    }

    #[test]
    fn later_edit_wins_over_earlier() {
        let mut a = Peer::new(1);
        let op1 = a.local_set(0, 0, 0, &cell_num(1.0));
        let op2 = a.local_set(0, 0, 0, &cell_num(2.0));
        let mut b = Peer::new(9);
        b.apply(op2);
        // 迟到的旧操作不能覆盖新值
        assert!(!b.apply(op1));
        assert_eq!(b.cells[&(0, 0, 0)].value, ValueWire::Number(2.0));
    }

    #[test]
    fn snapshot_brings_new_peer_up_to_date() {
        let mut a = Peer::new(1);
        a.local_set(0, 0, 0, &cell_num(1.0));
        a.local_set(0, 1, 0, &cell_num(2.0));
        let mut c = Peer::new(3);
        c.merge_snapshot(a.snapshot());
        assert_eq!(c.cells.len(), 2);
        // 新来者的后续编辑时间戳要大于快照里的最大值
        let op = c.local_set(0, 2, 0, &cell_num(3.0));
        assert!(op.ts > 2);
    }

    #[test]
    fn deletions_sync_as_empty_writes() {
        let mut a = Peer::new(1);
        a.local_set(0, 0, 0, &cell_num(1.0));
        let del = a.local_set(0, 0, 0, &Cell::default());
        let mut b = Peer::new(2);
        b.apply(del);
        assert_eq!(b.cells[&(0, 0, 0)].value, ValueWire::Empty);
    }
}
