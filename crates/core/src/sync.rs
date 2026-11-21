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
    /// 错误值单独一类：不能被当成文本同步过去
    Error(String),
}

impl From<&Value> for ValueWire {
    fn from(v: &Value) -> Self {
        match v {
            Value::Empty => ValueWire::Empty,
            Value::Number(n) => ValueWire::Number(*n),
            Value::Text(s) => ValueWire::Text(s.to_string()),
            Value::Bool(b) => ValueWire::Bool(*b),
            Value::Error(e) => ValueWire::Error(e.text().to_string()),
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
            ValueWire::Error(t) => crate::formula::ErrKind::from_text(t)
                .map(Value::Error)
                .unwrap_or_else(|| Value::Text(t.as_str().into())),
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

/// 默认单元格编辑租约时长（毫秒）
pub const DEFAULT_LEASE_TTL_MS: u64 = 20_000;

/// 按行列增删重算单元格坐标；落入被删除行/列的坐标返回 `None`。
pub fn shift_structural_coord(
    r: u32,
    c: u32,
    op: &crate::structural::Structural,
) -> Option<(u32, u32)> {
    use crate::structural::Structural as S;
    let axis = |v: u32, at: u32, n: u32, del: bool| -> Option<u32> {
        if del {
            if v < at {
                Some(v)
            } else if v < at.saturating_add(n) {
                None
            } else {
                Some(v - n)
            }
        } else if v >= at {
            Some(v.saturating_add(n))
        } else {
            Some(v)
        }
    };
    match op {
        S::InsertRows { at, n } => axis(r, *at, *n, false).map(|nr| (nr, c)),
        S::DeleteRows { at, n } => axis(r, *at, *n, true).map(|nr| (nr, c)),
        S::InsertCols { at, n } => axis(c, *at, *n, false).map(|nc| (r, nc)),
        S::DeleteCols { at, n } => axis(c, *at, *n, true).map(|nc| (r, nc)),
    }
}

/// 单元格编辑租约：同一格同一时刻只允许一个站点在编辑。
///
/// 时间用毫秒（调用方传单调时钟），过期由宿主清理——断线/崩溃的人不会把格子永久锁死。
#[derive(Clone, Debug)]
pub struct LockTable {
    map: HashMap<(usize, u32, u32), Holder>,
    /// 租约时长（毫秒）
    pub ttl_ms: u64,
}

impl Default for LockTable {
    fn default() -> Self {
        Self::new(DEFAULT_LEASE_TTL_MS)
    }
}

#[derive(Clone, Debug)]
pub struct Holder {
    pub site: u32,
    pub name: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LockOutcome {
    /// 拿到了
    Granted,
    /// 原本就是自己，续期
    Refreshed,
    /// 被别人占着
    Denied { holder: u32, name: String },
}

impl LockTable {
    pub fn new(ttl_ms: u64) -> Self {
        Self { map: HashMap::new(), ttl_ms }
    }

    pub fn request(
        &mut self,
        sheet: usize,
        row: u32,
        col: u32,
        site: u32,
        name: &str,
        now_ms: u64,
    ) -> LockOutcome {
        let key = (sheet, row, col);
        match self.map.get_mut(&key) {
            Some(h) if h.site == site => {
                h.at_ms = now_ms;
                LockOutcome::Refreshed
            }
            Some(h) if now_ms.saturating_sub(h.at_ms) < self.ttl_ms => LockOutcome::Denied {
                holder: h.site,
                name: h.name.clone(),
            },
            // 过期：直接顶掉
            _ => {
                self.map.insert(
                    key,
                    Holder { site, name: name.to_string(), at_ms: now_ms },
                );
                LockOutcome::Granted
            }
        }
    }

    pub fn release(&mut self, sheet: usize, row: u32, col: u32, site: u32) -> bool {
        let key = (sheet, row, col);
        if self.map.get(&key).map(|h| h.site) == Some(site) {
            self.map.remove(&key);
            true
        } else {
            false
        }
    }

    /// 清掉过期租约，返回被清掉的格子
    pub fn expire(&mut self, now_ms: u64) -> Vec<(usize, u32, u32, u32)> {
        let ttl = self.ttl_ms;
        let dead: Vec<((usize, u32, u32), u32)> = self
            .map
            .iter()
            .filter(|(_, h)| now_ms.saturating_sub(h.at_ms) >= ttl)
            .map(|(k, h)| (*k, h.site))
            .collect();
        for (k, _) in &dead {
            self.map.remove(k);
        }
        dead.into_iter().map(|((s, r, c), site)| (s, r, c, site)).collect()
    }

    pub fn holder(&self, sheet: usize, row: u32, col: u32) -> Option<&Holder> {
        self.map.get(&(sheet, row, col))
    }

    /// 若该格当前由其他站点持有未过期租约，返回持有者信息
    pub fn holder_other(
        &self,
        sheet: usize,
        row: u32,
        col: u32,
        site: u32,
        now_ms: u64,
    ) -> Option<&Holder> {
        self.map.get(&(sheet, row, col)).filter(|h| {
            h.site != site && now_ms.saturating_sub(h.at_ms) < self.ttl_ms
        })
    }

    pub fn is_locked_by_other(&self, sheet: usize, row: u32, col: u32, site: u32) -> bool {
        self.map.get(&(sheet, row, col)).map(|h| h.site != site).unwrap_or(false)
    }

    /// 检查矩形区域内是否存在其他站点的未过期编辑租约（复杂度仅与活跃锁数相关）
    pub fn conflicts_rect(
        &self,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        site: u32,
        now_ms: u64,
    ) -> Option<((u32, u32), &Holder)> {
        let (ra, rb) = (r1.min(r2), r1.max(r2));
        let (ca, cb) = (c1.min(c2), c1.max(c2));
        self.map
            .iter()
            .find(|((sh, r, c), h)| {
                *sh == sheet
                    && *r >= ra
                    && *r <= rb
                    && *c >= ca
                    && *c <= cb
                    && h.site != site
                    && now_ms.saturating_sub(h.at_ms) < self.ttl_ms
            })
            .map(|((_, r, c), h)| ((*r, *c), h))
    }

    /// 检查结构性增删行列是否会删除或平移其他站点正持有的活跃编辑租约
    pub fn conflicts_structural(
        &self,
        sheet: usize,
        op: &crate::structural::Structural,
        site: u32,
        now_ms: u64,
    ) -> Option<((u32, u32), &Holder)> {
        use crate::structural::Structural as S;
        self.map
            .iter()
            .find(|((sh, r, c), h)| {
                if *sh != sheet
                    || h.site == site
                    || now_ms.saturating_sub(h.at_ms) >= self.ttl_ms
                {
                    return false;
                }
                match op {
                    S::InsertRows { at, .. } | S::DeleteRows { at, .. } => *r >= *at,
                    S::InsertCols { at, .. } | S::DeleteCols { at, .. } => *c >= *at,
                }
            })
            .map(|((_, r, c), h)| ((*r, *c), h))
    }

    /// 结构性增删行列后同步平移租约表坐标；被删行/列上的租约一并移除
    pub fn shift_structural(&mut self, sheet: usize, op: &crate::structural::Structural) {
        let old = std::mem::take(&mut self.map);
        for ((sh, r, c), h) in old {
            if sh == sheet {
                if let Some((nr, nc)) = shift_structural_coord(r, c, op) {
                    self.map.insert((sh, nr, nc), h);
                }
            } else {
                self.map.insert((sh, r, c), h);
            }
        }
    }

    /// 释放同一站点除 `keep` 外的其余租约，防止单成员遗留或批量占满整表锁
    pub fn release_all_except(
        &mut self,
        site: u32,
        keep: (usize, u32, u32),
    ) -> Vec<(usize, u32, u32)> {
        let victims: Vec<(usize, u32, u32)> = self
            .map
            .iter()
            .filter(|(k, h)| h.site == site && **k != keep)
            .map(|(k, _)| *k)
            .collect();
        for k in &victims {
            self.map.remove(k);
        }
        victims
    }

    /// 更新指定站点持有租约的显示名称（不重置续期时间戳）
    pub fn rename_site(&mut self, site: u32, name: &str) {
        for h in self.map.values_mut() {
            if h.site == site {
                h.name = name.to_string();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = ((usize, u32, u32), &Holder)> {
        self.map.iter().map(|(k, h)| (*k, h))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Msg {
    /// 连上后宿主先发挑战码，客户端回 Hello 时带上 sha256(nonce + 口令)
    Challenge { nonce: u64 },
    /// 带 salt 的挑战：`salt` 是本连接的随机 salt，`room_salt` 是本次开房的随机 salt。
    /// 客户端先用 `room_salt` 把口令拉伸成房间密钥（PBKDF2），再与 `salt` 派生连接密钥。
    /// 旧版宿主不发 `room_salt`，新客户端据此识别并拒绝（协议 v2 不向下兼容）。
    ChallengeSalt {
        nonce: u64,
        salt: String,
        #[serde(default)]
        room_salt: String,
    },
    Denied { reason: String },
    Hello {
        site: u32,
        name: String,
        doc: String,
        proof: String,
        #[serde(default)]
        auth: String,
        #[serde(default)]
        base_hash: String,
    },
    /// 本机显示名变更（不改变 site；宿主转发给其他参与者）
    Rename { site: u32, name: String },
    Welcome {
        site: u32,
        doc: String,
        peers: Vec<(u32, String, u8)>,
        #[serde(default)]
        delta_sync: bool,
        #[serde(default)]
        clock: u64,
    },
    Snapshot { ops: Vec<Op> },
    /// 大文件分片下发（last=1 表示这是最后一片）
    SnapshotChunk { ops: Vec<Op>, #[serde(default)] last: bool },
    Op(Op),
    CursorMsg(Cursor),
    /// 格式变更（一批格子 + 每个格子的样式内容）
    StyleOp {
        site: u32,
        sheet: usize,
        ts: u64,
        items: Vec<(u32, u32, crate::style::CellStyle)>,
    },
    /// 合并 / 取消合并
    MergeOp {
        site: u32,
        sheet: usize,
        ts: u64,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        on: bool,
    },
    /// 加入后补一份现有样式（key: (sheet,row,col)）
    StyleSnapshot {
        items: Vec<(usize, u32, u32, crate::style::CellStyle)>,
    },
    /// 新加入的人先收到结构改动历史，把本地表格挪到位再收快照
    StructuralLog { items: Vec<(usize, crate::structural::Structural)> },
    /// 插行/删行/插列/删列
    Structural {
        site: u32,
        sheet: usize,
        op: crate::structural::Structural,
    },
    /// 请求编辑某格（同一格同时只允许一个人）
    LockReq { site: u32, name: String, sheet: usize, row: u32, col: u32 },
    /// 宿主批准（广播）
    LockOk { site: u32, name: String, sheet: usize, row: u32, col: u32 },
    /// 宿主拒绝（只回请求者）
    LockDeny { site: u32, sheet: usize, row: u32, col: u32, holder: u32, holder_name: String },
    /// 宿主因他人持有编辑租约拒绝写入/格式/合并/结构改动，并附带权威状态回滚
    WriteDeny {
        holder: u32,
        holder_name: String,
        sheet: usize,
        row: u32,
        col: u32,
        #[serde(default)]
        revert_ops: Vec<(u32, u32, Option<Op>)>,
        #[serde(default)]
        revert_styles: Vec<(u32, u32, Option<crate::style::CellStyle>)>,
        #[serde(default)]
        revert_merge: Option<(u32, u32, u32, u32, bool)>,
    },
    /// 释放（编辑结束/取消/断线）
    LockRelease { site: u32, sheet: usize, row: u32, col: u32 },
    /// 协作者请求宿主将权威合并状态及时写回共享原文件（避免多机同时写同一 SMB 文件冲突）
    SaveReq { site: u32 },
    /// 宿主已将权威状态保存到共享原文件，通知全员清除未保存标记
    Saved { site: u32, #[serde(default)] clock: u64 },
    /// 协作成员宣告自己持有经过官方公钥签名的新版可执行文件（供房间内旧版成员无感接力更新）
    UpdateAnnounce {
        site: u32,
        version: String,
        size: u64,
        sha256: String,
        sig: String,
        #[serde(default)]
        notes: String,
    },
    /// 旧版本成员向房间内持有新版签名包的指定站点请求拉取二进制分片
    UpdatePull {
        from_site: u32,
        to_site: u32,
        version: String,
    },
    /// 定向转发的新版二进制分片（宿主按 `to_site` 单播路由，不向无关成员广播）
    UpdateSlice {
        from_site: u32,
        to_site: u32,
        version: String,
        offset: u64,
        total: u64,
        data_b64: String,
        done: bool,
    },
    Bye { site: u32 },
}

// ---------------------------------------------------------------------------
// 远端报文的边界检查
//
// 协作报文来自网络，坐标、数量、时间戳都不可信：一条 col≈40 亿的操作会让每个
// 参与者为整列分配数 GB 内存；一条 ts=u64::MAX 的操作会让逻辑时钟饱和，之后
// 所有本地编辑都在「后写者胜」里输掉；一条 n=u32::MAX 的插入行会让坐标平移溢出。
// 收到的每条报文先过这里，越界的整条丢弃。
// ---------------------------------------------------------------------------

/// 行数上限（与 Excel 工作表一致）
pub const MAX_ROWS: u32 = 1_048_576;
/// 列数上限（与 Excel 工作表一致，XFD）
pub const MAX_COLS: u32 = 16_384;
/// 工作表序号上限（宽松，只拦离谱值）
pub const MAX_SHEET_INDEX: usize = 4_096;
/// 逻辑时钟上限。正常编辑一辈子也到不了 2^48；远端超过它就丢弃，本地自增也永不溢出。
pub const MAX_LOGICAL_TS: u64 = 1 << 48;
/// 单条报文里批量条目（样式、快照、回滚）的上限
pub const MAX_BATCH_ITEMS: usize = 2_000_000;
/// 名字、文档名等短文本的长度上限（字节）
pub const MAX_SHORT_TEXT: usize = 256;

/// 单元格坐标是否在工作表范围内
pub fn cell_in_bounds(sheet: usize, row: u32, col: u32) -> bool {
    sheet < MAX_SHEET_INDEX && row < MAX_ROWS && col < MAX_COLS
}

fn ts_ok(ts: u64) -> bool {
    ts <= MAX_LOGICAL_TS
}

fn structural_ok(sheet: usize, op: &crate::structural::Structural) -> bool {
    use crate::structural::Structural as S;
    if sheet >= MAX_SHEET_INDEX {
        return false;
    }
    let (at, n, limit) = match *op {
        S::InsertRows { at, n } | S::DeleteRows { at, n } => (at, n, MAX_ROWS),
        S::InsertCols { at, n } | S::DeleteCols { at, n } => (at, n, MAX_COLS),
    };
    n >= 1 && at < limit && (at as u64 + n as u64) <= limit as u64
}

impl Op {
    /// 坐标在表内、时间戳未越界
    pub fn is_valid(&self) -> bool {
        cell_in_bounds(self.sheet, self.row, self.col) && ts_ok(self.ts)
    }
}

impl Msg {
    /// 报文里的坐标、数量、时间戳是否都在合理范围内。越界报文应整条丢弃。
    pub fn is_within_limits(&self) -> bool {
        match self {
            Msg::Hello { name, doc, .. } => name.len() <= MAX_SHORT_TEXT && doc.len() <= 4 * MAX_SHORT_TEXT,
            Msg::Rename { name, .. } => name.len() <= MAX_SHORT_TEXT,
            Msg::Welcome { clock, peers, .. } => ts_ok(*clock) && peers.len() <= 1_024,
            Msg::Snapshot { ops } | Msg::SnapshotChunk { ops, .. } => {
                ops.len() <= MAX_BATCH_ITEMS && ops.iter().all(Op::is_valid)
            }
            Msg::Op(op) => op.is_valid(),
            Msg::CursorMsg(c) => cell_in_bounds(c.sheet, c.row, c.col) && c.name.len() <= MAX_SHORT_TEXT,
            Msg::StyleOp { sheet, ts, items, .. } => {
                *sheet < MAX_SHEET_INDEX
                    && ts_ok(*ts)
                    && items.len() <= MAX_BATCH_ITEMS
                    && items.iter().all(|(r, c, _)| cell_in_bounds(*sheet, *r, *c))
            }
            Msg::MergeOp { sheet, ts, r1, c1, r2, c2, .. } => {
                cell_in_bounds(*sheet, *r1, *c1)
                    && cell_in_bounds(*sheet, *r2, *c2)
                    && r1 <= r2
                    && c1 <= c2
                    && ts_ok(*ts)
            }
            Msg::StyleSnapshot { items } => {
                items.len() <= MAX_BATCH_ITEMS
                    && items.iter().all(|(s, r, c, _)| cell_in_bounds(*s, *r, *c))
            }
            Msg::StructuralLog { items } => {
                items.len() <= MAX_BATCH_ITEMS && items.iter().all(|(s, op)| structural_ok(*s, op))
            }
            Msg::Structural { sheet, op, .. } => structural_ok(*sheet, op),
            Msg::LockReq { sheet, row, col, name, .. } | Msg::LockOk { sheet, row, col, name, .. } => {
                cell_in_bounds(*sheet, *row, *col) && name.len() <= MAX_SHORT_TEXT
            }
            Msg::LockDeny { sheet, row, col, .. } | Msg::LockRelease { sheet, row, col, .. } => {
                cell_in_bounds(*sheet, *row, *col)
            }
            Msg::WriteDeny { sheet, row, col, revert_ops, revert_styles, revert_merge, .. } => {
                cell_in_bounds(*sheet, *row, *col)
                    && revert_ops.len() <= MAX_BATCH_ITEMS
                    && revert_ops.iter().all(|(r, c, op)| {
                        cell_in_bounds(*sheet, *r, *c) && op.as_ref().is_none_or(Op::is_valid)
                    })
                    && revert_styles.len() <= MAX_BATCH_ITEMS
                    && revert_styles.iter().all(|(r, c, _)| cell_in_bounds(*sheet, *r, *c))
                    && revert_merge.is_none_or(|(r1, c1, r2, c2, _)| {
                        cell_in_bounds(*sheet, r1, c1) && cell_in_bounds(*sheet, r2, c2)
                    })
            }
            Msg::Saved { clock, .. } => ts_ok(*clock),
            Msg::UpdateAnnounce {
                version,
                size,
                sha256,
                sig,
                notes,
                ..
            } => {
                !version.is_empty()
                    && version.len() <= 32
                    && *size > 0
                    && *size <= 256 * 1024 * 1024
                    && sha256.len() == 64
                    && sig.len() == 128
                    && notes.len() <= 1024
            }
            Msg::UpdatePull { version, .. } => !version.is_empty() && version.len() <= 32,
            Msg::UpdateSlice {
                version,
                offset,
                total,
                data_b64,
                ..
            } => {
                !version.is_empty()
                    && version.len() <= 32
                    && *total > 0
                    && *total <= 256 * 1024 * 1024
                    && *offset <= *total
                    && data_b64.len() <= 512 * 1024
            }
            Msg::Challenge { .. }
            | Msg::ChallengeSalt { .. }
            | Msg::Denied { .. }
            | Msg::SaveReq { .. }
            | Msg::Bye { .. } => true,
        }
    }
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

    /// 宿主启动时将磁盘基准单元格放入初始快照：所有未改基准格统一赋 `ts = 1`，
    /// 确保任何后加入成员首次编辑（`ts >= 2`）严格新于全部基准格，不会因 `local_set` 自增出数万时间戳而误判乱序丢弃。
    pub fn seed_cell(&mut self, sheet: usize, row: u32, col: u32, cell: &Cell) {
        self.clock = self.clock.max(1);
        let op = Op {
            sheet,
            row,
            col,
            value: ValueWire::from(&cell.value),
            formula: cell.formula.as_deref().map(|f| f.to_string()),
            ts: 1,
            site: self.site,
        };
        self.cells.insert((sheet, row, col), op);
    }

    /// 本地编辑：递增逻辑时钟并返回要广播的操作
    pub fn local_set(&mut self, sheet: usize, row: u32, col: u32, cell: &Cell) -> Op {
        self.clock = self.clock.saturating_add(1);
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
        if !op.is_valid() {
            return false; // 越界坐标或离谱时间戳：不进副本，也不推高时钟
        }
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

    /// 强制以宿主权威版本覆盖单格状态（用于租约冲突被宿主驳回后的本地回滚）
    pub fn force_cell(&mut self, sheet: usize, row: u32, col: u32, op: Option<Op>) {
        let key = (sheet, row, col);
        match op {
            Some(op) if !op.is_valid() => {}
            Some(op) => {
                self.clock = self.clock.max(op.ts);
                self.cells.insert(key, op);
            }
            None => {
                self.cells.remove(&key);
            }
        }
    }

    /// 结构性增删行列后同步平移副本状态坐标
    pub fn shift_structural(&mut self, sheet: usize, op: &crate::structural::Structural) {
        let old = std::mem::take(&mut self.cells);
        for ((sh, r, c), mut cell_op) in old {
            if sh == sheet {
                if let Some((nr, nc)) = shift_structural_coord(r, c, op) {
                    cell_op.row = nr;
                    cell_op.col = nc;
                    self.cells.insert((sh, nr, nc), cell_op);
                }
            } else {
                self.cells.insert((sh, r, c), cell_op);
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
            } else if let Some(sheet) = wb.sheets.get_mut(*si) {
                sheet.set(*r, *c, Cell::default());
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
    fn hostile_coordinates_and_clocks_are_rejected() {
        use crate::structural::Structural;
        let op = |sheet: usize, row: u32, col: u32, ts: u64| Op {
            sheet,
            row,
            col,
            value: ValueWire::Number(1.0),
            formula: None,
            ts,
            site: 9,
        };
        let mut p = Peer::new(1);
        assert!(!p.apply(op(0, 0, 4_000_000_000, 5)), "列号越界");
        assert!(!p.apply(op(0, MAX_ROWS, 0, 5)), "行号越界");
        assert!(!p.apply(op(MAX_SHEET_INDEX, 0, 0, 5)), "表序号越界");
        assert!(!p.apply(op(0, 0, 0, u64::MAX)), "时间戳越界");
        assert!(p.cells.is_empty());
        assert_eq!(p.clock, 0, "被拒的报文不得推高逻辑时钟");
        assert!(p.apply(op(0, MAX_ROWS - 1, MAX_COLS - 1, 5)), "表内最后一格合法");

        // 本地自增永不溢出
        p.clock = u64::MAX;
        let mine = p.local_set(0, 0, 0, &cell_num(1.0));
        assert_eq!(mine.ts, u64::MAX);

        let s = |op: Structural| Msg::Structural { site: 1, sheet: 0, op };
        assert!(s(Structural::InsertRows { at: 5, n: 3 }).is_within_limits());
        assert!(!s(Structural::InsertRows { at: 5, n: u32::MAX }).is_within_limits());
        assert!(!s(Structural::DeleteCols { at: MAX_COLS, n: 1 }).is_within_limits());
        assert!(!s(Structural::InsertCols { at: 0, n: 0 }).is_within_limits());
        let merge = |r2: u32, c2: u32| Msg::MergeOp { site: 1, sheet: 0, ts: 1, r1: 2, c1: 2, r2, c2, on: true };
        assert!(merge(3, 3).is_within_limits());
        assert!(!merge(1, 3).is_within_limits(), "r2 < r1");
        assert!(!merge(3, MAX_COLS).is_within_limits());
        assert!(!Msg::Op(op(0, 0, 0, MAX_LOGICAL_TS + 1)).is_within_limits());
        assert!(!Msg::Rename { site: 1, name: "x".repeat(MAX_SHORT_TEXT + 1) }.is_within_limits());
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
