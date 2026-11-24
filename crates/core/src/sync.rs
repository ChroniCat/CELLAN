//! 协作内核：基于「每格最后写入者胜出」的操作同步。
//!
//! 设计取舍：表格的最小冲突单位是单元格，同一格并发写入时 Excel 也只保留一个结果。
//! 因此不需要通用 CRDT（Yjs 那类字符级结构），用 (Lamport 时间戳, 站点号) 决定胜负即可，
//! 收敛性可证：全序唯一，所有副本合并同样的集合得到同样结果。
//!
//! 拓扑：星形。谁开文档谁当宿主，其余人连他；宿主收到操作后转发给其他连接。

use crate::model::{Cell, Sheet, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

mod arc_str_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(v: &Arc<str>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<str>, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Arc::from(s))
    }
}

mod opt_arc_str_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(v: &Option<Arc<str>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(text) => s.serialize_some(&**text),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<str>>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        Ok(opt.map(Arc::from))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ValueWire {
    Empty,
    Number(f64),
    Text(#[serde(with = "arc_str_serde")] Arc<str>),
    Bool(bool),
    /// 错误值单独一类：不能被当成文本同步过去
    Error(#[serde(with = "arc_str_serde")] Arc<str>),
}

impl From<&Value> for ValueWire {
    fn from(v: &Value) -> Self {
        match v {
            Value::Empty => ValueWire::Empty,
            Value::Number(n) => ValueWire::Number(*n),
            Value::Text(s) => ValueWire::Text(Arc::clone(s)),
            Value::Bool(b) => ValueWire::Bool(*b),
            Value::Error(e) => ValueWire::Error(Arc::from(e.text())),
        }
    }
}

impl From<&ValueWire> for Value {
    fn from(v: &ValueWire) -> Self {
        match v {
            ValueWire::Empty => Value::Empty,
            ValueWire::Number(n) => Value::Number(*n),
            ValueWire::Text(s) => Value::Text(Arc::clone(s)),
            ValueWire::Bool(b) => Value::Bool(*b),
            ValueWire::Error(t) => crate::formula::ErrKind::from_text(t)
                .map(Value::Error)
                .unwrap_or_else(|| Value::Text(Arc::clone(t))),
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
    #[serde(default, with = "opt_arc_str_serde")]
    pub formula: Option<Arc<str>>,
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

/// A checked destination-to-source permutation. Its digest covers the exact
/// LWW operations and style changes the sender would otherwise transmit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SortPermutation {
    pub structural_revision: usize,
    pub sheet: usize,
    pub name: String,
    pub region: (u32, u32, u32, u32),
    pub horizontal: bool,
    pub order: Vec<u32>,
    pub first_ts: u64,
    pub site: u32,
    pub digest: [u8; 32],
}

impl SortPermutation {
    pub fn valid(&self) -> bool {
        let (r1, c1, r2, c2) = self.region;
        if !cell_in_bounds(self.sheet, r2, c2) || r1 > r2 || c1 > c2
            || self.name.len() > 1024 || self.first_ts == 0 || !ts_ok(self.first_ts) {
            return false;
        }
        // Bound candidate reconstruction even for a syntactically valid sparse rectangle.
        let area = u64::from(r2-r1+1) * u64::from(c2-c1+1);
        let (first, last) = if self.horizontal { (c1, c2) } else { (r1, r2) };
        if area > 16_000_000 || self.order.len() != (last-first+1) as usize { return false; }
        let mut seen = vec![false; self.order.len()];
        self.order.iter().all(|&line| (first..=last).contains(&line)
            && !std::mem::replace(&mut seen[(line-first) as usize], true))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SortResponse {
    Accepted,
    NeedCells,
    Locked { structural_revision: usize },
}

/// Only the rejected writer receives these corrections. A correction applies
/// if the optimistic sort still owns that coordinate at the given timestamp.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SortRepair {
    pub rejected_ts: u64,
    pub row: u32,
    pub col: u32,
    pub cell: Option<Op>,
    pub style: Option<(crate::style::CellStyle, Option<crate::style::CellStyle>)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AxisStylePatch {
    #[serde(default)]
    pub rows: Vec<(u32, Option<crate::style::CellStyle>)>,
    #[serde(default)]
    pub cols: Vec<(u32, Option<crate::style::CellStyle>)>,
}

impl AxisStylePatch {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.cols.is_empty()
    }

    pub fn is_within_limits(&self) -> bool {
        self.rows.len() <= MAX_BATCH_ITEMS
            && self.cols.len() <= MAX_BATCH_ITEMS
            && self.rows.iter().all(|(r, _)| *r < MAX_ROWS)
            && self.cols.iter().all(|(c, _)| *c < MAX_COLS)
    }
}

/// 零深拷贝序列化快照分片（直接借用 `&Op` 引用避免 `Arc::unwrap_or_clone` 复制百万节点）
#[derive(Serialize)]
pub struct SnapshotRef<'a> {
    pub t: &'static str,
    pub ops: &'a [&'a Op],
}

#[derive(Serialize)]
pub struct SnapshotChunkRef<'a> {
    pub t: &'static str,
    pub ops: &'a [&'a Op],
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub last: bool,
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
        #[serde(default)]
        sort_permutation: bool,
        #[serde(default)]
        palette_styles: bool,
        #[serde(default)]
        resume_room: String,
        #[serde(default)]
        resume_watermark: u64,
        #[serde(default)]
        resume_struct_rev: usize,
        #[serde(default)]
        binary_v3: bool,
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
        batch_ops: bool,
        #[serde(default)]
        sort_permutation: bool,
        #[serde(default)]
        structural_ack: bool,
        #[serde(default)]
        clock: u64,
        #[serde(default)]
        palette_styles: bool,
        #[serde(default)]
        room_id: String,
        #[serde(default)]
        watermark: u64,
        #[serde(default)]
        structure_revision: usize,
        #[serde(default)]
        binary_v3: bool,
    },
    Snapshot { ops: Vec<Op> },
    /// 大文件分片下发（last=1 表示这是最后一片）
    SnapshotChunk { ops: Vec<Op>, #[serde(default)] last: bool },
    Op(Op),
    Ops { ops: Vec<Op> },
    SortPermutation(SortPermutation),
    SortData { first_ts: u64, items: Vec<(Op,Option<crate::style::CellStyle>)> },
    SortCommitted { first_ts: u64 },
    SortRepairDone { first_ts: u64 },
    SortReply { first_ts: u64, outcome: SortResponse },
    SortRepairRequest { sheet: usize, first_ts: u64, structural_revision: usize, keys: Vec<(u32,u32,Option<crate::style::CellStyle>)> },
    SortRepairs { sheet: usize, first_ts: u64, items: Vec<SortRepair> },
    CursorMsg(Cursor),
    /// 格式变更（一批格子 + 每个格子的样式内容）
    StyleOp {
        site: u32,
        sheet: usize,
        ts: u64,
        items: Vec<(u32, u32, crate::style::CellStyle)>,
    },
    /// 字典去重格式变更（避免 20 万格重复序列化相同 CellStyle）
    StylePaletteOp {
        site: u32,
        sheet: usize,
        ts: u64,
        palette: Vec<crate::style::CellStyle>,
        items: Vec<(u32, u32, u16)>,
    },
    /// 整行 / 整列默认样式变更
    AxisStyleOp {
        site: u32,
        sheet: usize,
        ts: u64,
        patch: AxisStylePatch,
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
    /// 字典去重样式快照分片
    StylePaletteSnapshot {
        palette: Vec<crate::style::CellStyle>,
        items: Vec<(usize, u32, u32, u16)>,
    },
    /// 加入或重连时同步整行/整列样式
    AxisStyleSnapshot {
        sheet: usize,
        patch: AxisStylePatch,
    },
    /// 宿主单调进度水位（供客户端闪断重连增量恢复）
    Watermark {
        seq: u64,
        #[serde(default)]
        structure_revision: usize,
    },
    /// 新加入的人先收到结构改动历史，把本地表格挪到位再收快照
    StructuralLog { items: Vec<(usize, crate::structural::Structural)> },
    /// 插行/删行/插列/删列
    Structural {
        site: u32,
        sheet: usize,
        op: crate::structural::Structural,
    },
    /// Guests keep their original coordinate system until the host echoes Structural.
    StructuralRequest { site: u32, sheet: usize, revision: usize, op: crate::structural::Structural },
    StructuralRejected { reason: String },
    StructureRevision { revision: usize },
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
pub const LIVE_OP_BATCH: usize = 512;
pub const SNAPSHOT_CHUNK_OPS: usize = 2_000;
pub const STYLE_PALETTE_CHUNK_ITEMS: usize = 4_096;
/// 名字、文档名等短文本的长度上限（字节）
pub const MAX_SHORT_TEXT: usize = 256;

pub fn cell_style_hash(s: &crate::style::CellStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.bold.hash(&mut h);
    s.italic.hash(&mut h);
    s.underline.hash(&mut h);
    s.double_underline.hash(&mut h);
    s.vertical_text.hash(&mut h);
    s.strikethrough.hash(&mut h);
    s.rotation.hash(&mut h);
    s.font.hash(&mut h);
    s.size.to_bits().hash(&mut h);
    s.color.hash(&mut h);
    s.fill.hash(&mut h);
    s.halign.hash(&mut h);
    s.valign.hash(&mut h);
    s.wrap.hash(&mut h);
    s.shrink_to_fit.hash(&mut h);
    s.indent.hash(&mut h);
    s.format.hash(&mut h);
    s.border.hash(&mut h);
    if let Some(d) = &s.diagonal {
        1u8.hash(&mut h);
        d.down.hash(&mut h);
        d.up.hash(&mut h);
        d.style.hash(&mut h);
        d.color.hash(&mut h);
    } else {
        0u8.hash(&mut h);
    }
    s.locked.hash(&mut h);
    s.hidden.hash(&mut h);
    h.finish()
}

fn intern_palette_style(
    palette: &mut Vec<crate::style::CellStyle>,
    buckets: &mut HashMap<u64, Vec<u16>>,
    st: &crate::style::CellStyle,
) -> u16 {
    let h = cell_style_hash(st);
    if let Some(cands) = buckets.get(&h) {
        for &id in cands {
            if &palette[id as usize] == st {
                return id;
            }
        }
    }
    let id = u16::try_from(palette.len()).unwrap_or(u16::MAX);
    if (id as usize) == palette.len() {
        palette.push(st.clone());
        buckets.entry(h).or_default().push(id);
    }
    id
}

pub fn encode_style_palette(
    items: &[(u32, u32, crate::style::CellStyle)],
) -> (Vec<crate::style::CellStyle>, Vec<(u32, u32, u16)>) {
    let mut palette: Vec<crate::style::CellStyle> = Vec::new();
    let mut buckets: HashMap<u64, Vec<u16>> = HashMap::new();
    let mut encoded = Vec::with_capacity(items.len());
    for (r, c, st) in items {
        let idx = intern_palette_style(&mut palette, &mut buckets, st);
        encoded.push((*r, *c, idx));
    }
    (palette, encoded)
}

pub fn decode_style_palette(
    palette: &[crate::style::CellStyle],
    items: &[(u32, u32, u16)],
) -> Option<Vec<(u32, u32, crate::style::CellStyle)>> {
    let mut out = Vec::with_capacity(items.len());
    for &(r, c, idx) in items {
        let st = palette.get(idx as usize)?.clone();
        out.push((r, c, st));
    }
    Some(out)
}

pub fn encode_style_snapshot_palette(
    items: &[(usize, u32, u32, crate::style::CellStyle)],
) -> (Vec<crate::style::CellStyle>, Vec<(usize, u32, u32, u16)>) {
    let mut palette: Vec<crate::style::CellStyle> = Vec::new();
    let mut buckets: HashMap<u64, Vec<u16>> = HashMap::new();
    let mut encoded = Vec::with_capacity(items.len());
    for (s, r, c, st) in items {
        let idx = intern_palette_style(&mut palette, &mut buckets, st);
        encoded.push((*s, *r, *c, idx));
    }
    (palette, encoded)
}

pub fn decode_style_snapshot_palette(
    palette: &[crate::style::CellStyle],
    items: &[(usize, u32, u32, u16)],
) -> Option<Vec<(usize, u32, u32, crate::style::CellStyle)>> {
    let mut out = Vec::with_capacity(items.len());
    for &(s, r, c, idx) in items {
        let st = palette.get(idx as usize)?.clone();
        out.push((s, r, c, st));
    }
    Some(out)
}

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
            Msg::Hello { name, doc, resume_room, resume_watermark, .. } => {
                name.len() <= MAX_SHORT_TEXT
                    && doc.len() <= 4 * MAX_SHORT_TEXT
                    && resume_room.len() <= MAX_SHORT_TEXT
                    && ts_ok(*resume_watermark)
            }
            Msg::Rename { name, .. } => name.len() <= MAX_SHORT_TEXT,
            Msg::Welcome { clock, peers, room_id, watermark, .. } => {
                ts_ok(*clock)
                    && ts_ok(*watermark)
                    && room_id.len() <= MAX_SHORT_TEXT
                    && peers.len() <= 1_024
            }
            Msg::Snapshot { ops } | Msg::SnapshotChunk { ops, .. } => {
                ops.len() <= MAX_BATCH_ITEMS && ops.iter().all(Op::is_valid)
            }
            Msg::SortPermutation(p) => p.valid(),
            Msg::SortCommitted { first_ts } | Msg::SortRepairDone { first_ts } => ts_ok(*first_ts),
            Msg::SortData { first_ts,items } => ts_ok(*first_ts) && items.len() <= LIVE_OP_BATCH
                && items.iter().all(|(op,_)| op.is_valid()),
            Msg::SortRepairRequest { sheet, first_ts, keys, .. } => {
                keys.len() <= LIVE_OP_BATCH && ts_ok(first_ts.saturating_add(keys.len().saturating_sub(1) as u64))
                    && keys.iter().all(|(r,c,_)| cell_in_bounds(*sheet,*r,*c))
            }
            Msg::SortRepairs { sheet, first_ts, items } => {
                items.len() <= LIVE_OP_BATCH && ts_ok(first_ts.saturating_add(items.len().saturating_sub(1) as u64))
                    && items.iter().all(|v| ts_ok(v.rejected_ts) && cell_in_bounds(*sheet,v.row,v.col) && v.cell.as_ref().is_none_or(|op|
                        op.is_valid() && (op.sheet,op.row,op.col) == (*sheet,v.row,v.col)))
            }
            Msg::SortReply { first_ts, .. } => ts_ok(*first_ts),
            Msg::Op(op) => op.is_valid(),
            Msg::Ops { ops } => !ops.is_empty() && ops.len() <= LIVE_OP_BATCH && ops.iter().all(Op::is_valid),
            Msg::CursorMsg(c) => cell_in_bounds(c.sheet, c.row, c.col) && c.name.len() <= MAX_SHORT_TEXT,
            Msg::StyleOp { sheet, ts, items, .. } => {
                *sheet < MAX_SHEET_INDEX
                    && ts_ok(*ts)
                    && items.len() <= MAX_BATCH_ITEMS
                    && items.iter().all(|(r, c, _)| cell_in_bounds(*sheet, *r, *c))
            }
            Msg::StylePaletteOp { sheet, ts, palette, items, .. } => {
                *sheet < MAX_SHEET_INDEX
                    && ts_ok(*ts)
                    && palette.len() <= u16::MAX as usize + 1
                    && items.len() <= MAX_BATCH_ITEMS
                    && items.iter().all(|(r, c, idx)| {
                        cell_in_bounds(*sheet, *r, *c) && (*idx as usize) < palette.len()
                    })
            }
            Msg::AxisStyleOp { sheet, ts, patch, .. } => {
                *sheet < MAX_SHEET_INDEX && ts_ok(*ts) && patch.is_within_limits()
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
            Msg::StylePaletteSnapshot { palette, items } => {
                palette.len() <= u16::MAX as usize + 1
                    && items.len() <= MAX_BATCH_ITEMS
                    && items.iter().all(|(s, r, c, idx)| {
                        cell_in_bounds(*s, *r, *c) && (*idx as usize) < palette.len()
                    })
            }
            Msg::AxisStyleSnapshot { sheet, patch } => {
                *sheet < MAX_SHEET_INDEX && patch.is_within_limits()
            }
            Msg::Watermark { seq, structure_revision } => {
                ts_ok(*seq) && u64::try_from(*structure_revision).is_ok_and(ts_ok)
            }
            Msg::StructuralLog { items } => {
                items.len() <= MAX_BATCH_ITEMS && items.iter().all(|(s, op)| structural_ok(*s, op))
            }
            Msg::Structural { sheet, op, .. } | Msg::StructuralRequest { sheet, op, .. } => structural_ok(*sheet, op),
            Msg::StructureRevision { revision } => u64::try_from(*revision).is_ok_and(ts_ok),
            Msg::StructuralRejected { reason } => reason.len() <= MAX_SHORT_TEXT,
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

/// 按矩形范围稀疏遍历工作表非空单元格（跳过空白页与空白列块）
pub fn iter_sheet_rect(
    sheet: &Sheet,
    rect: (u32, u32, u32, u32),
) -> impl Iterator<Item = (crate::model::CellRef, std::borrow::Cow<'_, Cell>)> {
    sheet.cells().iter_rect(rect)
}

/// 宿主开房时的零拷贝 CoW 基准工作簿快照（基于 `Sheet::search_snapshot` 列块共享）
#[derive(Clone, Debug)]
pub struct BaselineBook {
    pub site: u32,
    pub sheets: Vec<Sheet>,
    pub styles: Arc<crate::style::StyleTable>,
}

impl BaselineBook {
    pub fn from_workbook(site: u32, wb: &crate::model::Workbook) -> Self {
        Self {
            site,
            sheets: wb.sheets.iter().map(Sheet::search_snapshot).collect(),
            styles: Arc::new(wb.styles.clone()),
        }
    }

    pub fn has_cell(&self, sheet: usize, row: u32, col: u32) -> bool {
        self.sheets
            .get(sheet)
            .and_then(|sh| sh.get(row, col))
            .is_some()
    }

    pub fn cell_op(&self, sheet: usize, row: u32, col: u32) -> Option<Op> {
        let cell = self.sheets.get(sheet)?.get(row, col)?;
        Some(Op {
            sheet,
            row,
            col,
            value: ValueWire::from(&cell.value),
            formula: cell.formula.clone(),
            ts: 1,
            site: self.site,
        })
    }

    pub fn cell_style(&self, sheet: usize, row: u32, col: u32) -> Option<crate::style::CellStyle> {
        let sh = self.sheets.get(sheet)?;
        let sid = sh.explicit_style_of(row, col)?;
        Some(self.styles.get(sid).clone())
    }

    pub fn for_each_cell_in_rect(
        &self,
        sheet: usize,
        rect: (u32, u32, u32, u32),
        mut f: impl FnMut(u32, u32, &Cell),
    ) {
        let Some(sh) = self.sheets.get(sheet) else { return };
        for (k, cell) in sh.cells().iter_rect(rect) {
            f(k.row, k.col, &cell);
        }
    }

    pub fn for_each_style_in_rect(
        &self,
        sheet: usize,
        rect: (u32, u32, u32, u32),
        mut f: impl FnMut(u32, u32, &crate::style::CellStyle),
    ) {
        let Some(sh) = self.sheets.get(sheet) else { return };
        for (k, sid) in sh.styles.iter_rect(rect) {
            f(k.row, k.col, self.styles.get(sid));
        }
    }

    pub fn shift_structural(&mut self, target_sheet: usize, op: &crate::structural::Structural) {
        let names: Vec<String> = self.sheets.iter().map(|s| s.name.clone()).collect();
        let shifts = structural_reference_shifts(&names, target_sheet, op);
        if let Some(sh) = self.sheets.get_mut(target_sheet) {
            use crate::structural::Structural as S;
            match *op {
                S::InsertRows { at, n } => {
                    sh.remap_cell_axis(true, at, |r| {
                        Some(if r >= at { r.saturating_add(n) } else { r })
                    });
                    let moved: Vec<_> = sh
                        .row_styles
                        .extract_if(|&r, _| r >= at)
                        .map(|(r, id)| (r.saturating_add(n), id))
                        .collect();
                    sh.row_styles.extend(moved);
                }
                S::DeleteRows { at, n } => {
                    sh.remap_cell_axis(true, at, |r| {
                        if r < at {
                            Some(r)
                        } else if r < at.saturating_add(n) {
                            None
                        } else {
                            Some(r - n)
                        }
                    });
                    let moved: Vec<_> = sh
                        .row_styles
                        .extract_if(|&r, _| r >= at)
                        .filter_map(|(r, id)| (r >= at.saturating_add(n)).then_some((r - n, id)))
                        .collect();
                    sh.row_styles.extend(moved);
                }
                S::InsertCols { at, n } => {
                    sh.remap_cell_axis(false, at, |c| {
                        Some(if c >= at { c.saturating_add(n) } else { c })
                    });
                    let moved: Vec<_> = sh
                        .col_styles
                        .extract_if(|&c, _| c >= at)
                        .map(|(c, id)| (c.saturating_add(n), id))
                        .collect();
                    sh.col_styles.extend(moved);
                }
                S::DeleteCols { at, n } => {
                    sh.remap_cell_axis(false, at, |c| {
                        if c < at {
                            Some(c)
                        } else if c < at.saturating_add(n) {
                            None
                        } else {
                            Some(c - n)
                        }
                    });
                    let moved: Vec<_> = sh
                        .col_styles
                        .extract_if(|&c, _| c >= at)
                        .filter_map(|(c, id)| (c >= at.saturating_add(n)).then_some((c - n, id)))
                        .collect();
                    sh.col_styles.extend(moved);
                }
            }
        }
        for (si, sh) in self.sheets.iter_mut().enumerate() {
            if let Some(shift) = shifts.get(si) {
                sh.rewrite_references(shift);
            }
        }
    }
}

/// 一个副本的状态机（与网络无关，便于单元测试）
pub struct Peer {
    pub site: u32,
    pub clock: u64,
    /// 每个格子的当前胜出版本
    pub cells: CellOps,
    /// 已知的其他站点（site → 名字、颜色）
    pub peers: HashMap<u32, (String, u8)>,
}

/// Discard deleted coordinates and relocate only keys affected by the edit.
/// Reinsert moved entries after extraction so shifted keys cannot collide with old ones.
pub fn shift_cell_map<V>(cells: &mut std::collections::BTreeMap<(usize, u32, u32), V>, sheet: usize,
                         op: &crate::structural::Structural, mut relocate: impl FnMut(&mut V, u32, u32)) {
    let mut moved = Vec::new();
    use crate::structural::Structural::*;
    let mut extract = |map: &mut std::collections::BTreeMap<_, V>, first, last| {
        for ((_, r, c), mut value) in map.extract_if(first..=last, |_, _| true) {
            if let Some((row, col)) = shift_structural_coord(r, c, op) {
                relocate(&mut value, row, col);
                moved.push(((sheet, row, col), value));
            }
        }
    };
    match *op {
        InsertRows { at, .. } | DeleteRows { at, .. } => {
            extract(cells, (sheet, at, 0), (sheet, u32::MAX, u32::MAX));
        }
        InsertCols { at: 0, .. } | DeleteCols { at: 0, .. } => {
            extract(cells, (sheet, 0, 0), (sheet, u32::MAX, u32::MAX));
        }
        InsertCols { at, .. } | DeleteCols { at, .. } => {
            // Seek directly to each occupied row's affected suffix.
            let mut start = 0;
            while let Some((&(_, row, _), _)) = cells.range((sheet,start,0)..=(sheet,u32::MAX,u32::MAX)).next() {
                extract(cells, (sheet, row, at), (sheet, row, u32::MAX));
                let Some(next) = row.checked_add(1) else { break };
                start = next;
            }
        }
    }
    cells.extend(moved);
}

pub fn shift_coord_set(
    coords: &mut BTreeSet<(usize, u32, u32)>,
    sheet: usize,
    op: &crate::structural::Structural,
) {
    let mut moved = Vec::new();
    use crate::structural::Structural::*;
    let mut extract = |set: &mut BTreeSet<(usize, u32, u32)>, first, last| {
        for (_, r, c) in set.extract_if(first..=last, |_| true) {
            if let Some((row, col)) = shift_structural_coord(r, c, op) {
                moved.push((sheet, row, col));
            }
        }
    };
    match *op {
        InsertRows { at, .. } | DeleteRows { at, .. } => {
            extract(coords, (sheet, at, 0), (sheet, u32::MAX, u32::MAX));
        }
        InsertCols { at: 0, .. } | DeleteCols { at: 0, .. } => {
            extract(coords, (sheet, 0, 0), (sheet, u32::MAX, u32::MAX));
        }
        InsertCols { at, .. } | DeleteCols { at, .. } => {
            let mut start = 0;
            while let Some(&(_, row, _)) =
                coords.range((sheet, start, 0)..=(sheet, u32::MAX, u32::MAX)).next()
            {
                extract(coords, (sheet, row, at), (sheet, row, u32::MAX));
                let Some(next) = row.checked_add(1) else { break };
                start = next;
            }
        }
    }
    coords.extend(moved);
}

#[test]
fn sort_permutation_validates_bounds_and_defaults_old_hello_to_legacy() {
    let mut sort = SortPermutation { structural_revision: 0, sheet: 1, name: "one".into(), region: (3,7,6,9),
        horizontal: true, order: vec![9,7,8], first_ts: 10, site: 3, digest: [0;32] };
    assert!(sort.valid());
    for bad in [vec![9,8],vec![9,8,9],vec![9,8,6],vec![9,8,10]] {
        sort.order = bad;
        assert!(!sort.valid());
    }
    sort.region = (0,0,MAX_ROWS-1,MAX_COLS-1);
    sort.horizontal = false;
    assert!(!sort.valid());
    let hello: Msg = serde_json::from_str(r#"{"t":"Hello","site":1,"name":"a","doc":"d","proof":"p"}"#).unwrap();
    assert!(matches!(hello,Msg::Hello { sort_permutation: false, palette_styles: false, resume_watermark: 0, binary_v3: false, .. }));
    let welcome: Msg = serde_json::from_str(r#"{"t":"Welcome","site":2,"doc":"d","peers":[]}"#).unwrap();
    assert!(matches!(welcome, Msg::Welcome { binary_v3: false, .. }));
}

#[test]
fn shifting_sparse_column_suffixes_preserves_other_rows_and_sheets() {
    use crate::structural::Structural;
    let mut map = std::collections::BTreeMap::from([
        ((0,0,19), 1), ((1,0,3), 2), ((1,0,9), 3), ((1,0,12), 4),
        ((1,64,1), 5), ((1,999_999,14), 6), ((2,0,10), 7)
    ]);
    let mut moved = Vec::new();
    shift_cell_map(&mut map,1,&Structural::DeleteCols { at:9, n:2 }, |v,r,c| moved.push((*v,r,c)));
    assert_eq!(moved, [(4,0,10),(6,999_999,12)]);
    assert_eq!(map.keys().copied().collect::<Vec<_>>(),
        [(0,0,19),(1,0,3),(1,0,10),(1,64,1),(1,999_999,12),(2,0,10)]);
    shift_cell_map(&mut map,1,&Structural::InsertCols { at:0, n:2 },|_,_,_| {});
    assert_eq!(map.keys().copied().collect::<Vec<_>>(),
        [(0,0,19),(1,0,5),(1,0,12),(1,64,3),(1,999_999,14),(2,0,10)]);
}

pub type CellOpMap = BTreeMap<(usize, u32, u32), Arc<Op>>;

#[derive(Clone, Debug)]
pub enum OpView<'a> {
    Shared(&'a Arc<Op>),
    Owned(Op),
}

impl std::ops::Deref for OpView<'_> {
    type Target = Op;
    fn deref(&self) -> &Op {
        match self {
            Self::Shared(arc) => arc,
            Self::Owned(op) => op,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CellOps {
    overlay: CellOpMap,
    formula_keys: BTreeSet<(usize, u32, u32)>,
    removed_baseline: BTreeSet<(usize, u32, u32)>,
    baseline: Option<BaselineBook>,
}

impl CellOps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_baseline(&mut self, baseline: BaselineBook) {
        self.overlay.clear();
        self.formula_keys.clear();
        self.removed_baseline.clear();
        self.baseline = Some(baseline);
    }

    pub fn baseline(&self) -> Option<&BaselineBook> {
        self.baseline.as_ref()
    }

    pub fn clear(&mut self) {
        self.overlay.clear();
        self.formula_keys.clear();
        self.removed_baseline.clear();
        self.baseline = None;
    }

    pub fn is_empty(&self) -> bool {
        if !self.overlay.is_empty() {
            return false;
        }
        match &self.baseline {
            None => true,
            Some(base) => base.sheets.iter().enumerate().all(|(si, sh)| {
                sh.iter()
                    .all(|(k, _)| self.removed_baseline.contains(&(si, k.row, k.col)))
            }),
        }
    }

    pub fn len(&self) -> usize {
        match &self.baseline {
            None => self.overlay.len(),
            Some(base) => {
                let mut count = self.overlay.len();
                for (si, sh) in base.sheets.iter().enumerate() {
                    for (k, _) in sh.iter() {
                        let key = (si, k.row, k.col);
                        if !self.overlay.contains_key(&key)
                            && !self.removed_baseline.contains(&key)
                        {
                            count += 1;
                        }
                    }
                }
                count
            }
        }
    }

    pub fn get(&self, key: &(usize, u32, u32)) -> Option<OpView<'_>> {
        if let Some(op) = self.overlay.get(key) {
            return Some(OpView::Shared(op));
        }
        if self.removed_baseline.contains(key) {
            return None;
        }
        let base = self.baseline.as_ref()?;
        base.cell_op(key.0, key.1, key.2).map(OpView::Owned)
    }

    pub fn get_overlay(&self, key: &(usize, u32, u32)) -> Option<&Arc<Op>> {
        self.overlay.get(key)
    }

    pub fn insert(&mut self, key: (usize, u32, u32), op: Arc<Op>) -> Option<Arc<Op>> {
        self.removed_baseline.remove(&key);
        if op.formula.is_some() {
            self.formula_keys.insert(key);
        } else {
            self.formula_keys.remove(&key);
        }
        self.overlay.insert(key, op)
    }

    pub fn remove(&mut self, key: &(usize, u32, u32)) -> Option<Arc<Op>> {
        self.formula_keys.remove(key);
        if self.baseline.is_some() {
            self.removed_baseline.insert(*key);
        }
        self.overlay.remove(key)
    }

    pub fn keys(&self) -> std::collections::btree_map::Keys<'_, (usize, u32, u32), Arc<Op>> {
        self.overlay.keys()
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, (usize, u32, u32), Arc<Op>> {
        self.overlay.iter()
    }

    pub fn values(&self) -> std::collections::btree_map::Values<'_, (usize, u32, u32), Arc<Op>> {
        self.overlay.values()
    }

    pub fn values_mut(
        &mut self,
    ) -> std::collections::btree_map::ValuesMut<'_, (usize, u32, u32), Arc<Op>> {
        self.overlay.values_mut()
    }

    /// 仅对实际含公式的覆盖单元格执行引用改写，跳过全部纯数值/文本格
    pub fn rewrite_formulas(&mut self, shifts: &[crate::refs::Shift]) {
        for &key in &self.formula_keys {
            if let Some(cell) = self.overlay.get_mut(&key) {
                if let Some(formula) = cell.shifted_formula(shifts) {
                    Arc::make_mut(cell).formula = Some(Arc::from(formula));
                }
            }
        }
    }
}

impl std::ops::Index<&(usize, u32, u32)> for CellOps {
    type Output = Arc<Op>;
    fn index(&self, key: &(usize, u32, u32)) -> &Self::Output {
        &self.overlay[key]
    }
}

/// Use the same per-sheet reference context as the workbook's structural edit.
pub fn structural_reference_shifts(names: &[String], target: usize, op: &crate::structural::Structural)
    -> Vec<crate::refs::Shift> {
    let Some(name) = names.get(target) else { return Vec::new() };
    let base = op.reference_shift();
    names.iter().map(|own| crate::refs::Shift {
        target_sheet:name.clone(), own_sheet:own.clone(), ..base.clone()
    }).collect()
}

impl Op {
    pub fn shifted_formula(&self, shifts: &[crate::refs::Shift]) -> Option<String> {
        let formula = self.formula.as_deref()?;
        let rewritten = crate::refs::rewrite(formula,shifts.get(self.sheet)?);
        (rewritten != formula).then_some(rewritten)
    }
}

pub fn shift_cell_ops(cells: &mut CellOps, sheet: usize, op: &crate::structural::Structural) {
    shift_cell_map(&mut cells.overlay, sheet, op, |op, row, col| {
        let cell = Arc::make_mut(op);
        cell.row = row;
        cell.col = col;
    });
    shift_coord_set(&mut cells.formula_keys, sheet, op);
    if cells.baseline.is_some() {
        shift_coord_set(&mut cells.removed_baseline, sheet, op);
        if let Some(base) = &mut cells.baseline {
            base.shift_structural(sheet, op);
        }
    }
}

impl Peer {
    pub fn new(site: u32) -> Self {
        Self { site, clock: 0, cells: CellOps::new(), peers: HashMap::new() }
    }

    /// 宿主启动时以 O(页数) CoW 快照建立只读基准（零逐格分配、零字符串克隆）
    pub fn seed_workbook(&mut self, wb: &crate::model::Workbook) -> BaselineBook {
        self.clock = self.clock.max(1);
        let base = BaselineBook::from_workbook(self.site, wb);
        self.cells.set_baseline(base.clone());
        base
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
            formula: cell.formula.clone(),
            ts: 1,
            site: self.site,
        };
        self.cells.insert((sheet, row, col), op.into());
    }

    /// 本地编辑：递增逻辑时钟并返回要广播的操作
    pub fn local_set(&mut self, sheet: usize, row: u32, col: u32, cell: &Cell) -> Op {
        (*self.local_set_shared(sheet, row, col, cell)).clone()
    }

    pub fn local_set_shared(&mut self, sheet: usize, row: u32, col: u32, cell: &Cell) -> Arc<Op> {
        self.clock = self.clock.saturating_add(1);
        let op = Op {
            sheet,
            row,
            col,
            value: ValueWire::from(&cell.value),
            formula: cell.formula.clone(),
            ts: self.clock,
            site: self.site,
        };
        let op = Arc::new(op);
        self.cells.insert((sheet, row, col), op.clone());
        op
    }

    /// 应用远端操作；返回是否真的改动了本地状态
    pub fn apply(&mut self, op: Op) -> bool {
        self.apply_shared(op.into())
    }

    pub fn apply_shared(&mut self, op: Arc<Op>) -> bool {
        if !op.is_valid() {
            return false; // 越界坐标或离谱时间戳：不进副本，也不推高时钟
        }
        self.clock = self.clock.max(op.ts);
        let key = (op.sheet, op.row, op.col);
        if let Some(cur) = self.cells.get_overlay(&key) {
            if !op.newer_than(cur) {
                return false;
            }
        } else if !self.cells.removed_baseline.contains(&key) {
            if let Some(base) = self.cells.baseline() {
                if base.has_cell(op.sheet, op.row, op.col) && (op.ts, op.site) <= (1, base.site) {
                    return false;
                }
            }
        }
        self.cells.insert(key, op);
        true
    }

    /// 强制以宿主权威版本覆盖单格状态（用于租约冲突被宿主驳回后的本地回滚）
    pub fn force_cell(&mut self, sheet: usize, row: u32, col: u32, op: Option<Op>) {
        let key = (sheet, row, col);
        match op {
            Some(op) if !op.is_valid() => {}
            Some(op) => {
                self.clock = self.clock.max(op.ts);
                self.cells.insert(key, op.into());
            }
            None => {
                self.cells.remove(&key);
            }
        }
    }

    /// 结构性增删行列后同步平移副本状态坐标
    pub fn shift_structural(&mut self, sheet: usize, op: &crate::structural::Structural) {
        shift_cell_ops(&mut self.cells, sheet, op);
    }

    /// 平移副本坐标并仅针对实际含公式的单元格改写引用（跳过百万纯数值格）
    pub fn shift_structural_with_formulas(
        &mut self,
        names: &[String],
        sheet: usize,
        op: &crate::structural::Structural,
    ) {
        shift_cell_ops(&mut self.cells, sheet, op);
        let shifts = structural_reference_shifts(names, sheet, op);
        self.cells.rewrite_formulas(&shifts);
    }

    pub fn snapshot(&self) -> Vec<Op> {
        let Some(base) = self.cells.baseline() else {
            return self.cells.values().map(|op| op.as_ref().clone()).collect();
        };
        let mut merged: BTreeMap<(usize, u32, u32), Op> = BTreeMap::new();
        for (si, sh) in base.sheets.iter().enumerate() {
            for (k, cell) in sh.iter() {
                let key = (si, k.row, k.col);
                if !self.cells.removed_baseline.contains(&key) {
                    merged.insert(
                        key,
                        Op {
                            sheet: si,
                            row: k.row,
                            col: k.col,
                            value: ValueWire::from(&cell.value),
                            formula: cell.formula.clone(),
                            ts: 1,
                            site: base.site,
                        },
                    );
                }
            }
        }
        for (&key, op) in self.cells.iter() {
            merged.insert(key, op.as_ref().clone());
        }
        merged.into_values().collect()
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
                    let cell = Cell { value: Value::from(&op.value), formula: op.formula.clone() };
                    sheet.set(*r, *c, cell);
                }
            } else if let Some(sheet) = wb.sheets.get_mut(*si) {
                sheet.set(*r, *c, Cell::default());
            }
        }
    }

    /// 整本工作簿写回（加入房间时用）
    pub fn apply_all_to_workbook(&self, wb: &mut crate::model::Workbook) {
        if let Some(base) = self.cells.baseline() {
            for (si, sh) in base.sheets.iter().enumerate() {
                if let Some(dst) = wb.sheets.get_mut(si) {
                    for (k, cell) in sh.iter_views() {
                        if !self.cells.removed_baseline.contains(&(si, k.row, k.col)) {
                            dst.set(k.row, k.col, cell.to_cell());
                        }
                    }
                }
            }
        }
        for ((si, r, c), op) in self.cells.iter() {
            if let Some(sheet) = wb.sheets.get_mut(*si) {
                let cell = Cell { value: Value::from(&op.value), formula: op.formula.clone() };
                sheet.set(*r, *c, cell);
            }
        }
    }

    /// 兼容旧调用：只写第 0 张表
    pub fn apply_to_sheet(&self, sheet: &mut Sheet) {
        if let Some(base) = self.cells.baseline() {
            if let Some(src) = base.sheets.first() {
                for (k, cell) in src.iter_views() {
                    if !self.cells.removed_baseline.contains(&(0, k.row, k.col)) {
                        sheet.set(k.row, k.col, cell.to_cell());
                    }
                }
            }
        }
        for ((si, r, c), op) in self.cells.iter() {
            if *si != 0 {
                continue;
            }
            let cell = Cell { value: Value::from(&op.value), formula: op.formula.clone() };
            sheet.set(*r, *c, cell);
        }
    }
}

/// 新加入者单批次上限保护（全量快照按 `SNAPSHOT_CHUNK_OPS` 流式分片传输）
pub const MAX_SNAPSHOT_OPS: usize = MAX_BATCH_ITEMS;

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

    #[test]
    fn seed_workbook_cow_baseline_and_palette_encoding_preserve_semantics() {
        let mut wb = crate::model::Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("共享文本".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Number(10.0), formula: Some("=A1".into()) });

        let mut host_peer = Peer::new(5);
        let base = host_peer.seed_workbook(&wb);
        assert_eq!(base.sheets.len(), 1);
        assert_eq!(host_peer.cells.len(), 2);
        assert_eq!(
            host_peer.cells.get(&(0, 0, 0)).unwrap().value,
            ValueWire::Text("共享文本".into())
        );

        // Stale remote write at ts=1 with lower site loses to baseline (ts=1, site=5)
        assert!(!host_peer.apply(Op {
            sheet: 0,
            row: 0,
            col: 0,
            value: ValueWire::Number(99.0),
            formula: None,
            ts: 1,
            site: 2,
        }));
        // Newer write at ts=2 wins over baseline
        assert!(host_peer.apply(Op {
            sheet: 0,
            row: 0,
            col: 0,
            value: ValueWire::Number(42.0),
            formula: None,
            ts: 2,
            site: 2,
        }));

        // Structural insert shifts both baseline formula cell and overlay cell
        host_peer.shift_structural_with_formulas(
            &["Sheet1".to_string()],
            0,
            &crate::structural::Structural::InsertRows { at: 0, n: 2 },
        );
        assert_eq!(
            host_peer.cells.get(&(0, 2, 0)).unwrap().value,
            ValueWire::Number(42.0)
        );
        assert_eq!(
            host_peer.cells.get(&(0, 3, 0)).unwrap().formula.as_deref(),
            Some("=A3")
        );

        let mut st_a = crate::style::CellStyle::default();
        st_a.bold = true;
        let mut st_b = crate::style::CellStyle::default();
        st_b.italic = true;
        let raw_items = vec![
            (0, 0, st_a.clone()),
            (0, 1, st_b.clone()),
            (0, 2, st_a.clone()),
        ];
        let (palette, items) = encode_style_palette(&raw_items);
        assert_eq!(palette.len(), 2);
        assert_eq!(items, vec![(0, 0, 0), (0, 1, 1), (0, 2, 0)]);
        assert!(Msg::StylePaletteOp {
            site: 5,
            sheet: 0,
            ts: 3,
            palette: palette.clone(),
            items: items.clone(),
        }
        .is_within_limits());
        assert_eq!(decode_style_palette(&palette, &items).unwrap(), raw_items);
    }
}
