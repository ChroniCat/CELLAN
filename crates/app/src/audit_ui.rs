//! 防篡改审计记录的文档生命周期、协作归属追踪与完整 UI 面板。
//!
//! 包含：
//! - `DocAuditState`：随文档标签页停放/切换的单文档审计上下文（CoW 基线、核验结果、多作者归属、协作联署）
//! - 启用审计确认弹窗、状态栏核验徽章、Backstage 审计卡片、「不可篡改审计记录」主面板
//! - 单元格右键「查看修改溯源 (Blame)」弹窗
//! - 「导出干净副本 (.xlsx)」、「导出审计 CSV」、「导出/验证冷归档包 (.cellan-audit)」

use crate::audit_store::{self, AuthorTrustBadge};
use crate::document_tasks::{AuditSaveContext, DocumentTasks};
use crate::{egui, net, ui_kit, NoticeAction, NoticeBanner, NoticeLevel, SheetApp};
use eframe::egui::{Color32, RichText};
use sheetcore::audit::{
    AuditAuthor, AuditColdArchive, AuditLog, AuditSeverity, AuditSnap, AuditVerification,
    ColdArchiveVerifyResult, PeerAttestationInput,
};
use sheetcore::model::Workbook;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 未落盘的样式/保护/恢复缺口等批量摘要事件（带单调序号与可选撤销 ID，确保保存并发安全且支持 Ctrl+Z 还原）
#[derive(Clone)]
pub(crate) struct PendingBulkOp {
    pub seq: u64,
    pub undo_id: Option<u64>,
    pub sheet_name: String,
    pub author: AuditAuthor,
    pub kind: String,
    pub summary: String,
}

/// 未落盘的插删行列结构操作（不直接改写内存 `wb.audit.log.blame`，仅在落盘签名时提交；支持 Ctrl+Z/Ctrl+Y 精确回滚）
#[derive(Clone)]
pub(crate) struct PendingStructuralOp {
    pub seq: u64,
    pub undo_id: Option<u64>,
    pub sheet_id: u64,
    pub sheet_name: String,
    pub author: AuditAuthor,
    pub op: sheetcore::structural::Structural,
    pub prev_dirty_cell_authors: Option<HashMap<(u64, u32, u32), AuditAuthor>>,
    pub post_dirty_cell_authors: Option<HashMap<(u64, u32, u32), AuditAuthor>>,
}

/// 单份文档标签页专属的审计运行态（与 `ParkedDoc` 一同停放/恢复）
#[derive(Clone, Default)]
pub(crate) struct DocAuditState {
    /// 上次已落盘（或打开时）的 CoW 工作簿基线，用于保存时 O(变化页数) 计算净差
    pub baseline: Option<Box<Workbook>>,
    /// 最近一次完整核验结果（哈希链 + 签名 + 状态根 + 本机水印 + TOFU）
    pub verification: Option<AuditVerification>,
    /// 打开或恢复时若检测到绕过审计的外部修改/基线缺口，暂存当时观察到的正文哈希，下次合规保存永久写入链中
    pub pending_external_gap: Option<String>,
    /// 本轮未保存期间每个单元格的真实修改者归属：(sheet_identity, row, col) -> AuditAuthor
    pub dirty_cell_authors: HashMap<(u64, u32, u32), AuditAuthor>,
    /// 本轮未保存期间每个合并区域的真实操作者归属：(sheet_identity, rect) -> (seq, AuditAuthor)
    pub dirty_merge_authors: HashMap<(u64, (u32, u32, u32, u32)), (u64, AuditAuthor)>,
    /// 本轮未保存期间的结构/样式/保护等摘要事件（带单调 seq 与可选 undo_id）
    pub dirty_bulk_ops: Vec<PendingBulkOp>,
    /// 被 Ctrl+Z 撤销的摘要事件（供 Ctrl+Y 重做恢复）
    pub redo_bulk_ops: Vec<PendingBulkOp>,
    /// 本轮未保存期间的插删行列结构操作（落盘前绝不直接改写 `wb.audit.log.blame`）
    pub pending_structural_ops: Vec<PendingStructuralOp>,
    /// 被 Ctrl+Z 撤销的结构操作（供 Ctrl+Y 重做恢复）
    pub redo_structural_ops: Vec<PendingStructuralOp>,
    /// 本协作会话中已通过 ECDSA 验签的在线成员身份：site -> AuditAuthor
    pub peer_identities: HashMap<u32, AuditAuthor>,
    /// 本轮已收到的远端协作者独立联署证明：site -> (seq, PeerAttestationInput)
    pub peer_attestations: HashMap<u32, (u64, PeerAttestationInput)>,
    /// 单调事件序号（用于保存完成回执精准消费快照内事件，保留保存飞行期间并发新增事件）
    pub next_event_seq: u64,
    /// 已导入并通过 Merkle/签名验真的冷归档包（供在审计面板中展开查阅历史明细）
    pub verified_cold_archive: Option<(ColdArchiveVerifyResult, AuditColdArchive)>,
    /// 为 `true` 时禁用保存时自动开启防篡改审计（如 `.xls` 文件、检测到审计剥离/损坏/回滚异常的文档）
    pub auto_enable_disabled: bool,
    /// 上次打开或保存落盘时的 `self.rev.len()` 游标，用于在统一面板中仅把「尚未落盘的新编辑」标为待保存
    pub rev_count_at_last_save: usize,
}

impl DocAuditState {
    /// 为新建空白工作簿初始化默认开启防篡改审计的运行态（零磁盘 I/O，首次保存 `.xlsx` 时自动生成签名基线）
    pub(crate) fn new_for_blank(wb: &Workbook) -> Self {
        Self {
            baseline: Some(Box::new(wb.clone())),
            auto_enable_disabled: false,
            ..Self::default()
        }
    }

    /// 当前工作簿是否处于「默认开启、待保存自动写入审计链」状态
    pub(crate) fn auto_enable_active(&self, wb: &Workbook) -> bool {
        if self.auto_enable_disabled || wb.audit.log.is_some() || wb.audit.has_anomaly() {
            return false;
        }
        if self
            .verification
            .as_ref()
            .is_some_and(|v| v.severity == AuditSeverity::Tampered)
        {
            return false;
        }
        let is_xls = wb
            .path
            .as_deref()
            .is_some_and(|p| p.extension().is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xls")));
        !is_xls
    }

    /// 是否需要跟踪本轮未保存的归属/样式/结构事件（已落盘审计链或默认开启待落盘）
    pub(crate) fn tracking_active(&self, wb: &Workbook) -> bool {
        wb.audit.log.is_some() || self.auto_enable_active(wb)
    }

    pub(crate) fn alloc_seq(&mut self) -> u64 {
        self.next_event_seq = self.next_event_seq.saturating_add(1);
        self.next_event_seq
    }

    pub(crate) fn push_bulk_op(
        &mut self,
        undo_id: Option<u64>,
        sheet_name: String,
        author: AuditAuthor,
        kind: impl Into<String>,
        summary: impl Into<String>,
    ) {
        let seq = self.alloc_seq();
        if undo_id.is_some() {
            self.redo_bulk_ops.clear();
        }
        self.dirty_bulk_ops.push(PendingBulkOp {
            seq,
            undo_id,
            sheet_name,
            author,
            kind: kind.into(),
            summary: summary.into(),
        });
    }

    pub(crate) fn record_peer_attestation(&mut self, site: u32, attest: PeerAttestationInput) {
        let seq = self.alloc_seq();
        self.peer_attestations.insert(site, (seq, attest));
    }

    pub(crate) fn record_merge_author(
        &mut self,
        sheet_id: u64,
        rect: (u32, u32, u32, u32),
        author: AuditAuthor,
    ) {
        let seq = self.alloc_seq();
        self.dirty_merge_authors.insert((sheet_id, rect), (seq, author));
    }
}

fn invert_structural_coord(
    row: u32,
    col: u32,
    op: &sheetcore::structural::Structural,
) -> Option<(u32, u32)> {
    use sheetcore::structural::Structural;
    match *op {
        Structural::InsertRows { at, n } => {
            if row < at {
                Some((row, col))
            } else if row < at.saturating_add(n) {
                None
            } else {
                Some((row - n, col))
            }
        }
        Structural::DeleteRows { at, n } => {
            if row < at {
                Some((row, col))
            } else {
                Some((row.saturating_add(n), col))
            }
        }
        Structural::InsertCols { at, n } => {
            if col < at {
                Some((row, col))
            } else if col < at.saturating_add(n) {
                None
            } else {
                Some((row, col - n))
            }
        }
        Structural::DeleteCols { at, n } => {
            if col < at {
                Some((row, col))
            } else {
                Some((row, col.saturating_add(n)))
            }
        }
    }
}

/// 独立初始化已加载工作簿的防篡改审计运行态与告警横幅（同时用于前台标签与后台停放标签 `ParkedDoc`）
pub(crate) fn init_audit_state_for_loaded(
    wb: &mut Workbook,
    path: Option<&Path>,
    my_pk: Option<&str>,
    doc_uid: u64,
) -> (DocAuditState, Option<NoticeBanner>) {
    if let Some(log) = wb.audit.log.as_ref().cloned() {
        log.bind_workbook_sheets(wb);
    }
    let mut state = DocAuditState::default();
    state.baseline = Some(Box::new(wb.clone()));

    let ver = audit_store::verify_workbook_full(path, wb, my_pk);
    let mut banner = None;
    if let Some(ref v) = ver {
        if v.has_external_gap() {
            state.pending_external_gap = Some(v.current_state_hash.clone());
        }
        match v.severity {
            AuditSeverity::Tampered => {
                let first_msg = v
                    .issues
                    .first()
                    .map(|i| i.message.clone())
                    .unwrap_or_else(|| "内嵌审计记录校验未通过".into());
                banner = Some(NoticeBanner {
                    level: NoticeLevel::Error,
                    title: "防篡改审计异常警告".into(),
                    detail: first_msg,
                    actions: vec![("查看审计记录".into(), NoticeAction::OpenAudit)],
                    tag: Some("audit_tamper"),
                    target_doc_uid: Some(doc_uid),
                });
            }
            AuditSeverity::Warning => {
                let first_msg = v
                    .issues
                    .first()
                    .map(|i| i.message.clone())
                    .unwrap_or_else(|| "存在审计提醒事项".into());
                banner = Some(NoticeBanner {
                    level: NoticeLevel::Warn,
                    title: "防篡改审计提醒".into(),
                    detail: first_msg,
                    actions: vec![("查看审计记录".into(), NoticeAction::OpenAudit)],
                    tag: Some("audit_warn"),
                    target_doc_uid: Some(doc_uid),
                });
            }
            AuditSeverity::Verified => {}
        }
    }
    let is_xls = path.is_some_and(|p| {
        p.extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xls"))
    });
    state.auto_enable_disabled = is_xls
        || wb.audit.has_anomaly()
        || ver
            .as_ref()
            .is_some_and(|v| v.severity == AuditSeverity::Tampered);
    state.verification = ver;
    (state, banner)
}

impl SheetApp {
    /// 懒加载或首次生成本机 DPAPI 保护的 ECDSA P-256 签名密钥对
    pub(crate) fn ensure_audit_keypair(&mut self) -> Result<sheetcore::audit::AuditKeyPair, String> {
        if let Some(kp) = self.audit_keypair.clone() {
            return Ok(kp);
        }
        match audit_store::load_or_create_device_keypair() {
            Ok(kp) => {
                self.audit_keypair = Some(kp.clone());
                Ok(kp)
            }
            Err(e) => Err(format!("初始化本机签名密钥失败：{e:#}")),
        }
    }

    pub(crate) fn my_audit_author(&mut self) -> Option<AuditAuthor> {
        let kp = self.ensure_audit_keypair().ok()?;
        Some(audit_store::current_author(&self.my_name, &kp))
    }

    pub(crate) fn my_audit_pubkey(&self) -> Option<&str> {
        self.audit_keypair.as_ref().map(|k| k.pubkey_hex.as_str())
    }

    /// 打开工作簿完成后执行完整防篡改核验，并建立 CoW 基线与告警横幅
    pub(crate) fn on_workbook_loaded_audit(&mut self) {
        self.audit_archive_msg = None;

        let my_pk = self
            .ensure_audit_keypair()
            .ok()
            .map(|k| k.pubkey_hex);
        let path = self.wb.path.clone();
        let (state, banner) = init_audit_state_for_loaded(
            &mut self.wb,
            path.as_deref(),
            my_pk.as_deref(),
            self.doc_uid,
        );
        if let Some(b) = banner {
            self.set_notice(b);
        }
        self.audit_state = state;
        self.audit_state.rev_count_at_last_save = if self.wb.audit.log.is_some() {
            self.rev.len()
        } else {
            0
        };
    }

    /// 异常退出恢复草稿后初始化审计基线：
    /// - 若原磁盘文件可读且状态根与草稿所携审计链头一致，直接以其作为精确基线，另存时完整记录逐格净变更；
    /// - 否则（原文件不可得或已偏离）以当前草稿为后续基线，并显式标记「恢复缺口」与 `pending_external_gap`，
    ///   杜绝另存后签出“0 格修改 / 核验通过”的失真提交。
    pub(crate) fn on_workbook_recovered_audit(&mut self, original_path: Option<&str>) {
        self.audit_state = DocAuditState::default();
        self.audit_archive_msg = None;

        let Some(log) = self.wb.audit.log.as_ref().cloned() else {
            let is_xls = original_path.is_some_and(|p| {
                Path::new(p)
                    .extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xls"))
            });
            self.audit_state.baseline = Some(Box::new(self.wb.clone()));
            self.audit_state.auto_enable_disabled = is_xls || self.wb.audit.has_anomaly();
            self.audit_state.rev_count_at_last_save = 0;
            return;
        };
        log.bind_workbook_sheets(&mut self.wb);
        let expected_hash = log.head_state_hash().map(|s| s.to_string());
        let cur_hash = sheetcore::audit::workbook_state_hash(&self.wb);

        // 1. 尝试从原文件加载与审计链头精确匹配的已签名基线
        let mut recovered_base: Option<Workbook> = None;
        if let (Some(orig_str), Some(exp)) = (original_path, expected_hash.as_deref()) {
            let orig_path = Path::new(orig_str);
            if orig_path.is_file() && !sheetcore::crypto::needs_password(orig_path) {
                if let Ok(mut orig_wb) = sheetcore::xlsx::load(orig_path) {
                    let same_doc = orig_wb
                        .audit
                        .log
                        .as_ref()
                        .is_some_and(|l| l.doc_id == log.doc_id && l.head_seq() == log.head_seq());
                    if same_doc && sheetcore::audit::workbook_state_hash(&orig_wb) == exp {
                        // 对齐两份独立加载工作簿的运行时 `Sheet.identity`，使 CoW 净差按逻辑表匹配
                        for osh in &mut orig_wb.sheets {
                            if let Some(csh) = self.wb.sheets.iter().find(|c| {
                                (osh.audit_sheet_idx.is_some() && c.audit_sheet_idx == osh.audit_sheet_idx)
                                    || c.name == osh.name
                            }) {
                                osh.identity = csh.identity;
                            }
                        }
                        recovered_base = Some(orig_wb);
                    }
                }
            }
        }

        if let Some(orig_base) = recovered_base {
            self.audit_state.baseline = Some(Box::new(orig_base));
            self.audit_state.verification = sheetcore::audit::verify_workbook_audit(&self.wb);
            self.audit_state.rev_count_at_last_save = self.rev.len();
            return;
        }

        // 2. 无法取得原已签名基线：以当前恢复草稿作为后续编辑基线；若与上次签名状态不同，显式钉下恢复缺口
        self.audit_state.baseline = Some(Box::new(self.wb.clone()));
        let ver = sheetcore::audit::verify_workbook_audit(&self.wb);
        if expected_hash.as_deref() != Some(cur_hash.as_str()) {
            self.audit_state.pending_external_gap = Some(cur_hash);
            let author = self.my_audit_author().unwrap_or_else(|| AuditAuthor {
                display_name: self.my_name.clone(),
                ..Default::default()
            });
            self.audit_state.push_bulk_op(
                None,
                "(工作簿)".into(),
                author,
                "恢复缺口",
                "从异常退出恢复草稿另存：崩溃前未落盘的改动无法取得原签名基线逐格回溯，已永久标记状态缺口",
            );
        }
        self.audit_state.verification = ver;
        self.audit_state.rev_count_at_last_save = self.rev.len();
    }

    /// 打开「编辑与防篡改记录」统一面板
    pub(crate) fn open_audit_panel(&mut self) {
        self.rev_open = true;
        self.refresh_audit_verification_now();
    }

    /// 重新核算当前审计状态（不含未保存改动时做完整比对）
    pub(crate) fn refresh_audit_verification_now(&mut self) {
        if self.wb.audit.log.is_none() && !self.wb.audit.has_anomaly() && self.audit_state.verification.is_none() {
            return;
        }
        // 若文档尚未落盘且已有本地编辑，仅在未脏时重跑完整状态根比对（否则保留上次落盘/打开时的核验快照，避免把正常未保存编辑误报成“外部修改”）
        if !self.wb.dirty {
            let my_pk = self.ensure_audit_keypair().ok().map(|k| k.pubkey_hex);
            let path = self.wb.path.clone();
            self.audit_state.verification =
                audit_store::verify_workbook_full(path.as_deref(), &self.wb, my_pk.as_deref());
        }
    }

    /// 为当前工作簿显式写入创世提交 #1（供单测与显式立即初始化场景使用；常规用户已默认自动开启）
    #[cfg(test)]
    pub(crate) fn enable_audit_for_current_workbook(&mut self) {
        if self.wb.audit.log.is_some() {
            self.status = "当前工作簿已开启防篡改审计记录（开启后不可关闭）".into();
            self.open_audit_panel();
            return;
        }
        if self.net.as_ref().is_some_and(|n| !n.hosting) {
            self.status = "当前处于协作访客状态，请由协作宿主开启防篡改审计记录".into();
            return;
        }
        if let Some(p) = self.wb.path.as_ref() {
            let is_xls = p
                .extension()
                .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xls"));
            if is_xls {
                self.status = "老格式 .xls 不支持内嵌防篡改审计，请先「另存为」.xlsx 后再开启".into();
                return;
            }
        }
        let kp = match self.ensure_audit_keypair() {
            Ok(kp) => kp,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let author = audit_store::current_author(&self.my_name, &kp);
        match AuditLog::enable(&self.wb, author, &kp, sheetcore::revision::now_ms()) {
            Ok(log) => {
                let arc = Arc::new(log);
                arc.bind_workbook_sheets(&mut self.wb);
                self.wb.audit.log = Some(arc.clone());
                self.wb.audit.stripped_marker = None;
                self.wb.audit.parse_error = None;
                self.wb.audit.strip_on_save = false;
                self.wb.dirty = true;
                self.audit_state.baseline = Some(Box::new(self.wb.clone()));
                self.audit_state.dirty_cell_authors.clear();
                self.audit_state.dirty_merge_authors.clear();
                self.audit_state.dirty_bulk_ops.clear();
                self.audit_state.redo_bulk_ops.clear();
                self.audit_state.pending_structural_ops.clear();
                self.audit_state.redo_structural_ops.clear();
                self.audit_state.pending_external_gap = None;
                self.audit_state.verification = Some(arc.verify_against_workbook(&self.wb));
                if let Some(net) = self.net.as_ref().filter(|n| n.hosting) {
                    net.broadcast_audit_sync(&arc);
                }
                self.status = "已开启防篡改审计记录（创世基线 #1 已生成，请按 Ctrl+S 保存生效）".into();
                self.rev_open = true;
            }
            Err(e) => {
                self.status = format!("开启防篡改审计失败：{e:#}");
            }
        }
    }

    /// 轻量获取当前本机作者：若密钥尚未加载且工作簿处于默认开启待首次保存态，
    /// 先返回占位作者（避免纯内存编辑触发 DPAPI 盘读写），保存前由 `prepare_audit_save_ctx_for` 补全真实公钥与签名。
    fn local_audit_author_lightweight(&mut self) -> Option<AuditAuthor> {
        if self.wb.audit.log.is_some() {
            return self.my_audit_author();
        }
        if let Some(kp) = self.audit_keypair.as_ref() {
            return Some(audit_store::current_author(&self.my_name, kp));
        }
        Some(AuditAuthor {
            display_name: self.my_name.clone(),
            ..Default::default()
        })
    }

    /// 本地单元格编辑归属登记（默认开启时亦跟踪纯格式变更与协作联署）
    pub(crate) fn audit_note_local_deltas(&mut self, label: &str, deltas: &[sheetcore::undo::CellDelta]) {
        if !self.audit_state.tracking_active(&self.wb) || deltas.is_empty() {
            return;
        }
        let Some(author) = self.local_audit_author_lightweight() else {
            return;
        };
        let mut only_style = true;
        for d in deltas {
            if d.before.value != d.after.value || d.before.formula != d.after.formula {
                only_style = false;
                if let Some(sh) = self.wb.sheets.get(d.sheet) {
                    self.audit_state
                        .dirty_cell_authors
                        .insert((sh.identity, d.row, d.col), author.clone());
                }
            }
        }
        if only_style {
            let sheet_name = deltas
                .first()
                .and_then(|d| self.wb.sheets.get(d.sheet))
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let first = &deltas[0];
            let (mut r1, mut c1, mut r2, mut c2) = (first.row, first.col, first.row, first.col);
            for d in deltas {
                r1 = r1.min(d.row);
                c1 = c1.min(d.col);
                r2 = r2.max(d.row);
                c2 = c2.max(d.col);
            }
            let range = if (r1, c1) == (r2, c2) {
                sheetcore::model::cell_name(r1, c1)
            } else {
                format!(
                    "{}:{}",
                    sheetcore::model::cell_name(r1, c1),
                    sheetcore::model::cell_name(r2, c2)
                )
            };
            let undo_id = self.undo.top_undo_id();
            self.audit_state.push_bulk_op(
                undo_id,
                sheet_name,
                author,
                "格式变更",
                format!("{label} {range}（{} 格）", deltas.len()),
            );
        } else if self.net.as_ref().is_some_and(|n| !n.hosting) {
            // 协作访客每次完成单元格编辑后立即刷新独立 ECDSA 联署，确保无论访客还是宿主保存均带有效联署
            self.guest_send_audit_attest_before_save();
        }
    }

    /// 本地批量 CoW 样式操作（如 `A1:A1025` 大选区改百分比、整行整列格式、格式刷、清除格式）登记审计摘要
    pub(crate) fn audit_note_style_rects(
        &mut self,
        label: &str,
        sheet_idx: usize,
        rects: &[(u32, u32, u32, u32)],
        undo_id: Option<u64>,
    ) {
        if !self.audit_state.tracking_active(&self.wb) || rects.is_empty() {
            return;
        }
        let Some(sh) = self.wb.sheets.get(sheet_idx) else {
            return;
        };
        let sheet_name = sh.name.clone();
        let Some(author) = self.local_audit_author_lightweight() else {
            return;
        };
        let (mut r1, mut c1, mut r2, mut c2) = rects[0];
        let mut total_cells: u64 = 0;
        for &(a, b, c, d) in rects {
            r1 = r1.min(a);
            c1 = c1.min(b);
            r2 = r2.max(c);
            c2 = c2.max(d);
            total_cells = total_cells.saturating_add(
                (c.saturating_sub(a) as u64 + 1).saturating_mul(d.saturating_sub(b) as u64 + 1),
            );
        }
        let range = if (r1, c1) == (r2, c2) {
            sheetcore::model::cell_name(r1, c1)
        } else {
            format!(
                "{}:{}",
                sheetcore::model::cell_name(r1, c1),
                sheetcore::model::cell_name(r2, c2)
            )
        };
        self.audit_state.push_bulk_op(
            undo_id,
            sheet_name,
            author,
            "格式变更",
            format!("{label} {range}（{total_cells} 格）"),
        );
    }

    /// 远端协作样式/轴样式等批量操作登记审计摘要（精确归属协作访客，不再漏记）
    pub(crate) fn audit_note_remote_bulk(
        &mut self,
        site: u32,
        sheet_idx: usize,
        kind: &str,
        summary: String,
    ) {
        if !self.audit_state.tracking_active(&self.wb) {
            return;
        }
        let Some(sh) = self.wb.sheets.get(sheet_idx) else {
            return;
        };
        let sheet_name = sh.name.clone();
        let author = self.audit_author_for_site(site);
        self.audit_state
            .push_bulk_op(None, sheet_name, author, kind, summary);
    }

    /// 本地或远端协作合并/取消合并操作归属登记（使净差中的合并变更精确归属真实操作者）
    pub(crate) fn audit_note_merge(
        &mut self,
        site: Option<u32>,
        sheet_idx: usize,
        rect: (u32, u32, u32, u32),
    ) {
        if !self.audit_state.tracking_active(&self.wb) {
            return;
        }
        let Some(sh) = self.wb.sheets.get(sheet_idx) else {
            return;
        };
        let sheet_id = sh.identity;
        let author = match site {
            Some(s) => self.audit_author_for_site(s),
            None => self.local_audit_author_lightweight().unwrap_or_else(|| AuditAuthor {
                display_name: self.my_name.clone(),
                ..Default::default()
            }),
        };
        self.audit_state.record_merge_author(sheet_id, rect, author);
    }

    /// 远端协作单元格编辑归属登记
    pub(crate) fn audit_note_remote_ops(&mut self, ops: &[sheetcore::sync::Op]) {
        if !self.audit_state.tracking_active(&self.wb) || ops.is_empty() {
            return;
        }
        for op in ops {
            let Some(sh) = self.wb.sheets.get(op.sheet) else {
                continue;
            };
            let author = self.audit_author_for_site(op.site);
            self.audit_state
                .dirty_cell_authors
                .insert((sh.identity, op.row, op.col), author);
        }
    }

    /// 远端或本地结构变动（插删行列）登记：
    /// 不再提前改写内存中的 `wb.audit.log.blame`，改为记录在 `pending_structural_ops` 中，
    /// 支持 Ctrl+Z / Ctrl+Y 完整回滚，并在保存签名时仅对最终留存的结构变更平移 Blame 与写入摘要。
    pub(crate) fn audit_note_structural(
        &mut self,
        site: Option<u32>,
        sheet_idx: usize,
        op: &sheetcore::structural::Structural,
        undo_id: Option<u64>,
    ) {
        if !self.audit_state.tracking_active(&self.wb) {
            return;
        }
        let Some(sh) = self.wb.sheets.get(sheet_idx) else {
            return;
        };
        let sheet_id = sh.identity;
        let sheet_name = sh.name.clone();
        let author = match site {
            Some(s) => self.audit_author_for_site(s),
            None => self.local_audit_author_lightweight().unwrap_or_else(|| AuditAuthor {
                display_name: self.my_name.clone(),
                ..Default::default()
            }),
        };
        let prev_authors = undo_id.map(|_| self.audit_state.dirty_cell_authors.clone());
        // 平移未落盘单元格归属坐标
        let old = std::mem::take(&mut self.audit_state.dirty_cell_authors);
        for ((sid, r, c), au) in old {
            if sid == sheet_id {
                if let Some((nr, nc)) = sheetcore::sync::shift_structural_coord(r, c, op) {
                    self.audit_state.dirty_cell_authors.insert((sid, nr, nc), au);
                }
            } else {
                self.audit_state.dirty_cell_authors.insert((sid, r, c), au);
            }
        }
        let post_authors = undo_id.map(|_| self.audit_state.dirty_cell_authors.clone());
        let seq = self.audit_state.alloc_seq();
        if undo_id.is_some() {
            self.audit_state.redo_structural_ops.clear();
        }
        self.audit_state.pending_structural_ops.push(PendingStructuralOp {
            seq,
            undo_id,
            sheet_id,
            sheet_name,
            author,
            op: op.clone(),
            prev_dirty_cell_authors: prev_authors,
            post_dirty_cell_authors: post_authors,
        });
    }

    /// 撤销（Ctrl+Z）后同步回滚关联的未落盘结构位移与格式摘要，防止撤销后仍留幽灵结构日志或 Blame 错位
    pub(crate) fn audit_on_undo(&mut self, undone_id: u64) {
        if !self.audit_state.tracking_active(&self.wb) {
            return;
        }
        if self
            .audit_state
            .pending_structural_ops
            .last()
            .is_some_and(|e| e.undo_id == Some(undone_id))
        {
            if let Some(entry) = self.audit_state.pending_structural_ops.pop() {
                if let Some(prev) = entry.prev_dirty_cell_authors.clone() {
                    self.audit_state.dirty_cell_authors = prev;
                }
                self.audit_state.redo_structural_ops.push(entry);
            }
        }
        while self
            .audit_state
            .dirty_bulk_ops
            .last()
            .is_some_and(|e| e.undo_id == Some(undone_id))
        {
            if let Some(entry) = self.audit_state.dirty_bulk_ops.pop() {
                self.audit_state.redo_bulk_ops.push(entry);
            }
        }
    }

    /// 重做（Ctrl+Y）后同步恢复关联的未落盘结构位移与格式摘要
    pub(crate) fn audit_on_redo(&mut self, redone_id: u64) {
        if !self.audit_state.tracking_active(&self.wb) {
            return;
        }
        if self
            .audit_state
            .redo_structural_ops
            .last()
            .is_some_and(|e| e.undo_id == Some(redone_id))
        {
            if let Some(mut entry) = self.audit_state.redo_structural_ops.pop() {
                if let Some(post) = entry.post_dirty_cell_authors.clone() {
                    self.audit_state.dirty_cell_authors = post;
                }
                entry.seq = self.audit_state.alloc_seq();
                self.audit_state.pending_structural_ops.push(entry);
            }
        }
        while self
            .audit_state
            .redo_bulk_ops
            .last()
            .is_some_and(|e| e.undo_id == Some(redone_id))
        {
            if let Some(mut entry) = self.audit_state.redo_bulk_ops.pop() {
                entry.seq = self.audit_state.alloc_seq();
                self.audit_state.dirty_bulk_ops.push(entry);
            }
        }
    }

    pub(crate) fn audit_author_for_site(&self, site: u32) -> AuditAuthor {
        if let Some(au) = self.audit_state.peer_identities.get(&site) {
            return au.clone();
        }
        AuditAuthor {
            display_name: self.peer_display_name(site),
            os_user: String::new(),
            machine: String::new(),
            pubkey_hex: String::new(),
        }
    }

    /// 协作访客在请求宿主保存前，对自己本轮所做的单元格改动计算摘要并用本机私钥独立联署
    pub(crate) fn guest_send_audit_attest_before_save(&mut self) {
        if !self.net.as_ref().is_some_and(|n| !n.hosting) {
            return;
        }
        let Ok(kp) = self.ensure_audit_keypair() else {
            return;
        };
        let Some(log) = self.wb.audit.log.as_ref() else {
            return;
        };
        let doc_id = log.doc_id.clone();
        let base_seq = log.head_seq();
        let Some(baseline) = self.audit_state.baseline.as_deref() else {
            return;
        };
        let my_pk = kp.pubkey_hex.clone();
        let mut my_cells = Vec::new();
        for (&(sid, r, c), au) in &self.audit_state.dirty_cell_authors {
            if !au.pubkey_hex.eq_ignore_ascii_case(&my_pk) {
                continue;
            }
            let Some(csh) = self.wb.sheets.iter().find(|s| s.identity == sid) else {
                continue;
            };
            let bsh = baseline.sheets.iter().find(|s| s.identity == sid);
            let before = bsh
                .and_then(|s| s.get_view(r, c))
                .map(|v| AuditSnap::from_cell_view(Some(v)))
                .unwrap_or(AuditSnap::Empty);
            let after = AuditSnap::from_cell_view(csh.get_view(r, c));
            if before != after {
                my_cells.push((csh.name.clone(), r, c, before, after));
            }
        }
        if my_cells.is_empty() {
            return;
        }
        let digest = sheetcore::audit::author_contribution_digest(&doc_id, base_seq, &my_cells);
        if let Some(net) = self.net.as_ref().filter(|n| !n.hosting) {
            net.send_audit_attest(&doc_id, base_seq, &digest, &kp);
        }
    }

    /// 在发起保存前组装防篡改审计提交上下文（同时支持活动标签页与后台停放标签页；
    /// 对新建工作簿与首次在 CELLAN 编辑保存的旧 `.xlsx` 默认自动启用审计链）
    pub(crate) fn prepare_audit_save_ctx_for(
        wb: &Workbook,
        audit_state: &DocAuditState,
        tasks: &mut DocumentTasks,
        net: Option<&net::Net>,
        my_name: &str,
        keypair_cache: &mut Option<sheetcore::audit::AuditKeyPair>,
        rev_len: usize,
    ) {
        let auto_enable = wb.audit.log.is_none() && audit_state.auto_enable_active(wb);
        if wb.audit.log.is_none() && !auto_enable {
            tasks.next_audit_ctx = None;
            return;
        }
        let kp = if let Some(k) = keypair_cache.clone() {
            k
        } else if let Ok(k) = audit_store::load_or_create_device_keypair() {
            *keypair_cache = Some(k.clone());
            k
        } else {
            tasks.next_audit_ctx = None;
            return;
        };
        let committer = audit_store::current_author(my_name, &kp);
        let normalize_author = |au: &AuditAuthor| -> AuditAuthor {
            if au.pubkey_hex.is_empty() && au.display_name == committer.display_name {
                committer.clone()
            } else {
                au.clone()
            }
        };
        let cell_attributions: HashMap<(u64, u32, u32), AuditAuthor> = audit_state
            .dirty_cell_authors
            .iter()
            .map(|(&k, au)| (k, normalize_author(au)))
            .collect();
        let bulk_ops: Vec<(String, AuditAuthor, String, String)> = audit_state
            .dirty_bulk_ops
            .iter()
            .map(|b| {
                (
                    b.sheet_name.clone(),
                    normalize_author(&b.author),
                    b.kind.clone(),
                    b.summary.clone(),
                )
            })
            .collect();
        let structural_ops: Vec<(String, AuditAuthor, sheetcore::structural::Structural)> = audit_state
            .pending_structural_ops
            .iter()
            .map(|s| {
                let cur_name = wb
                    .sheets
                    .iter()
                    .find(|sh| sh.identity == s.sheet_id)
                    .map(|sh| sh.name.clone())
                    .unwrap_or_else(|| s.sheet_name.clone());
                (cur_name, normalize_author(&s.author), s.op.clone())
            })
            .collect();
        let merge_attributions: HashMap<(u64, (u32, u32, u32, u32)), AuditAuthor> = audit_state
            .dirty_merge_authors
            .iter()
            .map(|(&k, (_, au))| (k, normalize_author(au)))
            .collect();
        let has_other_authors = cell_attributions
            .values()
            .any(|a| !a.pubkey_hex.eq_ignore_ascii_case(&committer.pubkey_hex))
            || bulk_ops
                .iter()
                .any(|(_, a, _, _)| !a.pubkey_hex.eq_ignore_ascii_case(&committer.pubkey_hex))
            || merge_attributions
                .values()
                .any(|a| !a.pubkey_hex.eq_ignore_ascii_case(&committer.pubkey_hex))
            || structural_ops
                .iter()
                .any(|(_, a, _)| !a.pubkey_hex.eq_ignore_ascii_case(&committer.pubkey_hex));
        let head_seq = wb.audit.log.as_ref().map(|l| l.head_seq()).unwrap_or(0);
        let action = if net.is_some() && (has_other_authors || !audit_state.peer_attestations.is_empty()) {
            "协作保存".to_string()
        } else if head_seq == 1
            && cell_attributions.is_empty()
            && bulk_ops.is_empty()
            && structural_ops.is_empty()
            && merge_attributions.is_empty()
        {
            "启用防篡改审计".to_string()
        } else {
            "保存修改".to_string()
        };
        let committed_bulk_seq = audit_state.dirty_bulk_ops.last().map(|b| b.seq).unwrap_or(0);
        let committed_structural_seq = audit_state
            .pending_structural_ops
            .last()
            .map(|s| s.seq)
            .unwrap_or(0);
        let committed_merge_seqs: HashMap<(u64, (u32, u32, u32, u32)), u64> = audit_state
            .dirty_merge_authors
            .iter()
            .map(|(&k, (seq, _))| (k, *seq))
            .collect();
        let co_signers: Vec<PeerAttestationInput> = audit_state
            .peer_attestations
            .values()
            .map(|(_, att)| att.clone())
            .collect();
        let committed_attest_seqs: HashMap<u32, u64> = audit_state
            .peer_attestations
            .iter()
            .map(|(&site, (seq, _))| (site, *seq))
            .collect();

        tasks.next_audit_ctx = Some(AuditSaveContext {
            baseline: audit_state.baseline.clone(),
            auto_enable,
            committer,
            keypair: kp,
            action,
            cell_attributions,
            merge_attributions,
            bulk_ops,
            structural_ops,
            co_signers,
            external_gap_hash: audit_state.pending_external_gap.clone(),
            committed_bulk_seq,
            committed_structural_seq,
            committed_merge_seqs,
            committed_attest_seqs,
            had_external_gap: audit_state.pending_external_gap.is_some(),
            committed_rev_len: rev_len,
        });
    }

    /// 后台保存成功回包后推进基线、精准消费已提交事件（保留保存飞行期间并发新增事件）、
    /// 处理另存 `.xls` 审计丢失清理、刷新本机防回滚水印，并在宿主侧广播 `AuditSync`。
    /// 返回 `(状态提示文案, 是否因另存 .xls 丢失了审计链)`。
    pub(crate) fn on_audit_save_completed_for(
        wb: &Workbook,
        audit_state: &mut DocAuditState,
        tasks: &mut DocumentTasks,
        net: Option<&net::Net>,
        saved_path: &Path,
        my_pubkey: Option<&str>,
    ) -> (Option<String>, bool) {
        let mut extra_note = None;
        let audit_rcpt = tasks.last_audit_saved_receipt.take();
        let is_xls = saved_path
            .extension()
            .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xls"));

        if is_xls || audit_rcpt.as_ref().is_some_and(|r| r.cleared_for_xls) || wb.audit.log.is_none() {
            let had_audit = audit_rcpt.as_ref().is_some_and(|r| r.cleared_for_xls)
                || audit_state.verification.is_some();
            *audit_state = DocAuditState::default();
            if is_xls {
                audit_state.auto_enable_disabled = true;
            }
            if is_xls && had_audit {
                return (
                    Some("已另存为 .xls：旧版格式不支持内嵌防篡改审计，当前文档审计链已停止并清零".into()),
                    true,
                );
            }
            return (None, false);
        }

        if let Some(rcpt) = audit_rcpt {
            audit_state.baseline = Some(rcpt.baseline);
            // 仅消费本次保存快照覆盖到的事件，严禁误删保存飞行期间并发产生的样式/结构/合并/联署事件！
            audit_state
                .dirty_bulk_ops
                .retain(|b| b.seq > rcpt.committed_bulk_seq);
            audit_state.redo_bulk_ops.clear();
            audit_state
                .pending_structural_ops
                .retain(|s| s.seq > rcpt.committed_structural_seq);
            audit_state.redo_structural_ops.clear();
            audit_state.dirty_merge_authors.retain(|k, (seq, _)| {
                rcpt.committed_merge_seqs
                    .get(k)
                    .is_none_or(|&cseq| *seq > cseq)
            });
            audit_state.peer_attestations.retain(|site, (seq, _)| {
                rcpt.committed_attest_seqs
                    .get(site)
                    .is_none_or(|&cseq| *seq > cseq)
            });
            if rcpt.consumed_external_gap {
                audit_state.pending_external_gap = None;
            }
            audit_state.rev_count_at_last_save =
                audit_state.rev_count_at_last_save.max(rcpt.committed_rev_len);
        } else {
            if !wb.dirty {
                audit_state.baseline = Some(Box::new(wb.clone()));
            }
            audit_state.dirty_bulk_ops.clear();
            audit_state.pending_structural_ops.clear();
            audit_state.dirty_merge_authors.clear();
            audit_state.peer_attestations.clear();
            audit_state.pending_external_gap = None;
        }

        if !wb.dirty {
            audit_state.dirty_cell_authors.clear();
        } else if let Some(base) = audit_state.baseline.as_deref() {
            // 保存飞行期间若有并发新改动，只保留相对新基线仍不同的格子归属
            audit_state.dirty_cell_authors.retain(|&(sid, r, c), _| {
                let Some(csh) = wb.sheets.iter().find(|s| s.identity == sid) else {
                    return false;
                };
                let bsh = base.sheets.iter().find(|s| s.identity == sid);
                let before = bsh
                    .and_then(|s| s.get_view(r, c))
                    .map(|v| AuditSnap::from_cell_view(Some(v)))
                    .unwrap_or(AuditSnap::Empty);
                let after = AuditSnap::from_cell_view(csh.get_view(r, c));
                before != after
            });
        }

        if let Some(log) = wb.audit.log.as_ref() {
            audit_store::record_verified_watermark(Some(saved_path), log);
            if !wb.dirty {
                audit_state.verification =
                    audit_store::verify_workbook_full(Some(saved_path), wb, my_pubkey);
            }
            if let Some(n) = net.filter(|n| n.hosting) {
                n.broadcast_audit_sync(log);
            }
            if let Some(arch) = log.last_compacted.as_ref() {
                extra_note = Some(format!(
                    "审计日志已自动滚动折叠 #{}..#{} 以控制文件体积（可在审计窗口导出冷归档包留存）",
                    arch.rollup.from_seq, arch.rollup.to_seq
                ));
            }
        }
        (extra_note, false)
    }

    /// 收到宿主广播的 `AuditSync`：访客更新本地可见审计链、基线，并刷新本机防回滚水印（P2P 见证！）
    pub(crate) fn on_receive_audit_sync(&mut self, payload_b64: &str) {
        let Ok(log) = AuditLog::decode_base64(payload_b64) else {
            return;
        };
        let arc = Arc::new(log);
        let my_pk = self.ensure_audit_keypair().ok().map(|k| k.pubkey_hex);
        let core_check = arc.verify_against_workbook(&self.wb);
        // 只要密码学哈希链与签名本身有效（即使访客本地公式重算略滞后），即同步展示并推进水印
        let chain_ok = !core_check.issues.iter().any(|i| {
            matches!(
                i.kind,
                sheetcore::audit::AuditIssueKind::BrokenHashChain
                    | sheetcore::audit::AuditIssueKind::InvalidCommitSignature
                    | sheetcore::audit::AuditIssueKind::InvalidRollupSignature
            )
        });
        if !chain_ok {
            self.status = "收到异常审计同步包（哈希链或签名校验失败），已拒绝".into();
            return;
        }
        arc.bind_workbook_sheets(&mut self.wb);
        self.wb.audit.log = Some(arc.clone());
        self.wb.audit.stripped_marker = None;
        self.wb.audit.parse_error = None;
        self.audit_state.baseline = Some(Box::new(self.wb.clone()));
        self.audit_state.dirty_cell_authors.clear();
        self.audit_state.dirty_merge_authors.clear();
        self.audit_state.dirty_bulk_ops.clear();
        self.audit_state.redo_bulk_ops.clear();
        self.audit_state.pending_structural_ops.clear();
        self.audit_state.redo_structural_ops.clear();
        audit_store::record_verified_watermark(self.wb.path.as_deref(), &arc);
        self.audit_state.verification =
            audit_store::verify_workbook_full(self.wb.path.as_deref(), &self.wb, my_pk.as_deref());
        if !self.wb.dirty {
            self.audit_state.rev_count_at_last_save = self.rev.len();
        }
    }

    /// 导出不含任何审计日志与标记的「干净副本」`.xlsx`（用于安全发给外部客户/甲方/供应商）
    pub(crate) fn export_clean_copy_dialog(&mut self) {
        if !self.finish_pending_edit() {
            return;
        }
        let stem = self
            .wb
            .path
            .as_deref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("未命名");
        let ext = if self.wb.has_vba { "xlsm" } else { "xlsx" };
        let dest = match std::env::var("SC_CLEAN_EXPORT") {
            Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
            _ => rfd::FileDialog::new()
                .set_file_name(format!("{stem}-对外干净副本.{ext}"))
                .add_filter("Excel 工作簿（不含审计记录）", &[ext])
                .save_file(),
        };
        let Some(dest) = dest else {
            return;
        };
        if self
            .wb
            .path
            .as_deref()
            .is_some_and(|p| crate::single_instance::same_file_path(p, &dest))
        {
            let msg = "对外干净副本不能直接覆盖当前受审计原文件，请另选路径".to_string();
            self.status = msg.clone();
            self.audit_archive_msg = Some((false, msg));
            return;
        }
        match sheetcore::audit::save_clean_copy(&dest, &self.wb) {
            Ok(()) => {
                let msg = format!("已导出对外干净副本（不含审计记录）：{}", dest.display());
                self.status = msg.clone();
                self.audit_archive_msg = Some((true, msg));
            }
            Err(e) => {
                let msg = format!("导出干净副本失败：{e:#}");
                self.status = msg.clone();
                self.audit_archive_msg = Some((false, msg));
            }
        }
    }

    /// 导出最近一次滚动折叠产生的 `.cellan-audit` 冷归档包
    pub(crate) fn export_cold_archive_dialog(&mut self) {
        let Some(archive) = self
            .wb
            .audit
            .log
            .as_ref()
            .and_then(|l| l.last_compacted.clone())
        else {
            self.audit_archive_msg = Some((false, "当前会话尚无新折叠的冷归档提交可导出".into()));
            return;
        };
        let stem = self
            .wb
            .path
            .as_deref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("workbook");
        let default_name = format!(
            "{stem}-审计归档-{}-{}.cellan-audit",
            archive.rollup.from_seq, archive.rollup.to_seq
        );
        let Some(dest) = rfd::FileDialog::new()
            .set_file_name(default_name)
            .add_filter("CELLAN 审计冷归档包", &["cellan-audit"])
            .save_file()
        else {
            return;
        };
        match serde_json::to_vec_pretty(&archive).and_then(|b| {
            std::fs::write(&dest, b).map_err(serde_json::Error::io)
        }) {
            Ok(()) => {
                self.audit_archive_msg = Some((
                    true,
                    format!(
                        "已导出冷归档包 #{}..#{}（{} 条）至 {}",
                        archive.rollup.from_seq,
                        archive.rollup.to_seq,
                        archive.commits.len(),
                        dest.display()
                    ),
                ));
            }
            Err(e) => {
                self.audit_archive_msg = Some((false, format!("导出冷归档包失败：{e}")));
            }
        }
    }

    /// 导入并对照当前文件内嵌的 Rollup 锚点密码学验真 `.cellan-audit` 冷归档包
    pub(crate) fn verify_cold_archive_dialog(&mut self) {
        let Some(log) = self.wb.audit.log.as_ref().cloned() else {
            self.audit_archive_msg = Some((false, "当前工作簿未启用防篡改审计".into()));
            return;
        };
        let Some(src) = rfd::FileDialog::new()
            .add_filter("CELLAN 审计冷归档包", &["cellan-audit"])
            .pick_file()
        else {
            return;
        };
        let raw = match std::fs::read(&src) {
            Ok(b) => b,
            Err(e) => {
                self.audit_archive_msg = Some((false, format!("读取冷归档包失败：{e}")));
                return;
            }
        };
        let archive: AuditColdArchive = match serde_json::from_slice(&raw) {
            Ok(a) => a,
            Err(e) => {
                self.audit_archive_msg = Some((false, format!("冷归档包格式损坏：{e}")));
                return;
            }
        };
        let res = log.verify_cold_archive(&archive);
        let ok = res.valid;
        let summary = res.summary.clone();
        if ok {
            self.audit_state.verified_cold_archive = Some((res, archive));
        }
        self.audit_archive_msg = Some((ok, summary));
    }

    // -----------------------------------------------------------------------
    // 统一面板数据模型与 UI 渲染：状态栏徽章、Backstage、单页融合记录窗口、Blame 溯源弹窗
    // -----------------------------------------------------------------------

    /// 组装统一「编辑与防篡改记录」面板的展示行（最新在前）：
    /// 1. 尚未保存落盘的本次编辑（`self.rev[rev_count_at_last_save..]`，标记为待保存，本地格改支持「撤回」）；
    /// 2. 已签名落盘的防篡改审计提交明细（单元格净变更、批量/结构/样式/恢复缺口摘要、非创世状态变更）。
    ///    纯空创世快照 `#1` 展示于顶部核验横幅，不重复计入用户修改记录条数，使动态用户筛选下拉计数与表格行严格一致。
    pub(crate) fn build_unified_audit_rows(&self) -> Vec<UnifiedAuditRow> {
        let mut rows = Vec::new();

        // 1. 尚未签名落盘的本地/协作会话修订（最新在前）
        let pending_start = if self.wb.audit.log.is_some() {
            self.audit_state.rev_count_at_last_save.min(self.rev.len())
        } else {
            0
        };
        for idx in (pending_start..self.rev.len()).rev() {
            let e = &self.rev[idx];
            let user_name = normalize_user_name(&e.who);
            let loc = if e.sheet.is_empty() {
                "(工作簿)".to_string()
            } else {
                format!("{}!{}", e.sheet, e.range())
            };
            let jump = e
                .cells
                .first()
                .map(|ch| (e.sheet.clone(), ch.r, ch.c));
            let detail = e.delta_text();
            let action = if self.wb.audit.log.is_some() || self.audit_state.auto_enable_active(&self.wb) {
                format!("待保存·{}", e.label)
            } else {
                e.label.clone()
            };
            let hover_tip = if self.wb.audit.log.is_some() || self.audit_state.auto_enable_active(&self.wb) {
                "尚未保存落盘的编辑记录；按 Ctrl+S 保存 .xlsx 后将自动计算净变更并用 ECDSA P-256 私钥签名上链。".to_string()
            } else {
                "本次会话编辑记录（当前 .xls 格式不支持内嵌防篡改审计链）".to_string()
            };
            let search_haystack = format!(
                "{} {} {} {} {}",
                action, loc, detail, user_name, e.note
            )
            .to_lowercase();
            rows.push(UnifiedAuditRow {
                ts_ms: e.ts,
                pending: true,
                commit_seq: None,
                user_name,
                author: None,
                action,
                location: loc,
                jump,
                detail,
                hover_tip,
                revert_rev_idx: (!e.remote && !e.cells.is_empty()).then_some(idx),
                badge_text: Some("待保存".into()),
                badge_is_warn: false,
                search_haystack,
            });
        }

        // 2. 已签名落盘的不可篡改审计链提交（最新在前）
        if let Some(log) = self.wb.audit.log.as_ref() {
            for commit in log.commits.iter().rev() {
                // 纯创世基线提交（#1 仅含「启用审计」初始化记录、无任何格改与缺口）由顶部核验横幅展示，
                // 不计入用户修改记录条数，保证「动态用户筛选下拉」仅显示实际编辑修改过的用户及准确条数。
                let is_pure_genesis = commit.seq == 1
                    && commit.cells.is_empty()
                    && commit.external_gap_hash.is_none()
                    && commit.bulk_ops.iter().all(|b| b.kind == "启用审计");
                if is_pure_genesis {
                    continue;
                }
                let committer = log.author_of(commit.committer_idx);
                let committer_name = committer
                    .map(|a| normalize_user_name(&a.display_name))
                    .unwrap_or_else(|| "未知用户".into());

                let mut tip_lines = vec![
                    format!(
                        "防篡改提交 #{} · 时间：{}",
                        commit.seq,
                        sheetcore::audit::format_local_ms(commit.ts_ms)
                    ),
                    format!(
                        "提交签名人：{}",
                        committer
                            .map(|a| format!("{}（指纹 {}）", a.short_label(), a.fingerprint()))
                            .unwrap_or_else(|| committer_name.clone())
                    ),
                    format!(
                        "提交哈希：{}…  |  状态根：{}…",
                        &commit.commit_hash[..commit.commit_hash.len().min(16)],
                        &commit.state_hash[..commit.state_hash.len().min(16)],
                    ),
                ];
                for co in &commit.co_signers {
                    let cname = log
                        .author_of(co.author_idx)
                        .map(|a| a.short_label())
                        .unwrap_or_else(|| "未知".into());
                    let ctag = if co.attested_by_peer {
                        "✓ 协作者独立私钥联署已验签"
                    } else {
                        "△ 宿主代签"
                    };
                    tip_lines.push(format!("协作参与：{cname}（{} 格 · {ctag}）", co.change_count));
                }
                if commit.external_gap_hash.is_some() {
                    tip_lines.push("⚠ 本次提交记录了绕过审计的外部修改/恢复状态缺口".into());
                }
                let hover_tip = tip_lines.join("\n");

                let (badge_text, badge_is_warn) = if commit.external_gap_hash.is_some() {
                    (Some("⚠缺口".into()), true)
                } else if !commit.co_signers.is_empty() {
                    let attested = commit.co_signers.iter().filter(|c| c.attested_by_peer).count();
                    (Some(format!("联署{attested}/{}", commit.co_signers.len())), false)
                } else {
                    (Some("已签名".into()), false)
                };

                // 2a. 逐格净变更明细
                for ch in &commit.cells {
                    let sh = log.sheet_of(ch.sheet_idx);
                    let loc = format!("{sh}!{}", sheetcore::model::cell_name(ch.row, ch.col));
                    let ch_author = log.author_of(ch.author_idx).or(committer).cloned();
                    let user_name = ch_author
                        .as_ref()
                        .map(|a| normalize_user_name(&a.display_name))
                        .unwrap_or_else(|| committer_name.clone());
                    let action = format!("#{} {}", commit.seq, commit.action);
                    let detail = format!("{} → {}", ch.before.summary(), ch.after.summary());
                    let search_haystack = format!(
                        "#{} {} {} {} {}",
                        commit.seq, action, loc, detail, user_name
                    )
                    .to_lowercase();
                    rows.push(UnifiedAuditRow {
                        ts_ms: commit.ts_ms,
                        pending: false,
                        commit_seq: Some(commit.seq),
                        user_name,
                        author: ch_author,
                        action,
                        location: loc,
                        jump: Some((sh.to_string(), ch.row, ch.col)),
                        detail,
                        hover_tip: hover_tip.clone(),
                        revert_rev_idx: None,
                        badge_text: badge_text.clone(),
                        badge_is_warn,
                        search_haystack,
                    });
                }

                // 2b. 批量/结构/样式/合并/恢复缺口摘要
                for bo in &commit.bulk_ops {
                    let sh = log.sheet_of(bo.sheet_idx).to_string();
                    let bo_author = log.author_of(bo.author_idx).or(committer).cloned();
                    let user_name = bo_author
                        .as_ref()
                        .map(|a| normalize_user_name(&a.display_name))
                        .unwrap_or_else(|| committer_name.clone());
                    let action = format!("#{} {}", commit.seq, bo.kind);
                    let detail = bo.summary.clone();
                    let search_haystack = format!(
                        "#{} {} {} {} {} {}",
                        commit.seq, action, sh, detail, user_name, commit.action
                    )
                    .to_lowercase();
                    rows.push(UnifiedAuditRow {
                        ts_ms: commit.ts_ms,
                        pending: false,
                        commit_seq: Some(commit.seq),
                        user_name,
                        author: bo_author,
                        action,
                        location: sh,
                        jump: None,
                        detail,
                        hover_tip: hover_tip.clone(),
                        revert_rev_idx: None,
                        badge_text: badge_text.clone(),
                        badge_is_warn: badge_is_warn || bo.kind.contains("缺口"),
                        search_haystack,
                    });
                }

                // 2c. 工作簿级变更（例如公式名称/表属性改动或带缺口提交，无逐格与批量摘要时保底展示）
                if commit.cells.is_empty() && commit.bulk_ops.is_empty() {
                    let action = format!("#{} {}", commit.seq, commit.action);
                    let detail = if commit.external_gap_hash.is_some() {
                        "检测到绕过审计的外部修改/恢复状态缺口".to_string()
                    } else if commit.total_cell_changes > 0 {
                        format!("{} 格改动（明细已折叠）", commit.total_cell_changes)
                    } else {
                        "工作簿结构/名称或元数据变更".to_string()
                    };
                    let search_haystack = format!(
                        "#{} {} {} {}",
                        commit.seq, action, detail, committer_name
                    )
                    .to_lowercase();
                    rows.push(UnifiedAuditRow {
                        ts_ms: commit.ts_ms,
                        pending: false,
                        commit_seq: Some(commit.seq),
                        user_name: committer_name,
                        author: committer.cloned(),
                        action,
                        location: "(工作簿)".into(),
                        jump: None,
                        detail,
                        hover_tip,
                        revert_rev_idx: None,
                        badge_text,
                        badge_is_warn,
                        search_haystack,
                    });
                }
            }
        }

        rows
    }

    /// 底部状态栏右侧防篡改审计状态徽章（点击直达统一编辑与防篡改记录面板）
    pub(crate) fn draw_audit_status_pill(&mut self, ui: &mut egui::Ui) {
        let pal = crate::theme::palette();
        let normal = self.audit_state.verification.as_ref()
            .is_none_or(|verification| matches!(verification.severity, AuditSeverity::Verified));
        let (label, fg, bg, tip) = if let Some(ver) = self.audit_state.verification.as_ref() {
            match ver.severity {
                AuditSeverity::Verified => (
                    format!("防篡改完整 #{}", ver.head_seq),
                    pal.accent,
                    pal.accent_tint,
                    format!(
                        "防篡改审计链完整（共 {} 版，{:.1} KB）· 点击查看记录",
                        ver.head_seq,
                        ver.embedded_bytes as f64 / 1024.0
                    ),
                ),
                AuditSeverity::Warning => (
                    format!("防篡改提醒 #{}", ver.head_seq),
                    pal.warn_fg,
                    pal.warn_bg,
                    format!("存在 {} 项审计提醒（如时钟异常或身份指纹变更）· 点击查看", ver.issues.len()),
                ),
                AuditSeverity::Tampered => (
                    "⚠ 防篡改异常".to_string(),
                    pal.err_fg,
                    pal.err_bg,
                    format!("检测到 {} 项防篡改高危异常（外部修改/篡改/回退）· 立即点击查看", ver.issues.len()),
                ),
            }
        } else if self.audit_state.auto_enable_active(&self.wb) {
            (
                "防篡改已开".to_string(),
                pal.accent,
                pal.accent_tint,
                "防篡改编辑记录已默认开启：保存 .xlsx 时自动生成 ECDSA P-256 签名链 · 点击查看记录".to_string(),
            )
        } else {
            return;
        };
        ui.add_space(6.0);
        let btn = egui::Button::new(RichText::new(label).size(11.0).color(if normal { pal.text_weak } else { fg }))
            .fill(if normal { egui::Color32::TRANSPARENT } else { bg })
            .stroke(if normal { egui::Stroke::NONE } else { egui::Stroke::new(1.0, fg.gamma_multiply(0.55)) })
            .corner_radius(3.0);
        let response = ui.add(btn);
        crate::ui_kit::focus_ring(ui, &response, response.rect, 3.0);
        if response.on_hover_text(tip).clicked() {
            self.open_audit_panel();
        }
    }

    /// Backstage「文件 → 信息」中的防篡改审计状态卡片
    pub(crate) fn draw_backstage_audit_section(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        let pal = crate::theme::palette();
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("编辑与防篡改记录").strong().size(13.0));
        ui.add_space(4.0);
        if let Some(ver) = self.audit_state.verification.clone() {
            let (badge, col) = match ver.severity {
                AuditSeverity::Verified => ("已保护 · 密码学核验完整", pal.accent),
                AuditSeverity::Warning => ("已保护 · 存在提醒事项", pal.warn_fg),
                AuditSeverity::Tampered => ("⚠ 检测到疑似篡改 / 外部修改 / 回退", pal.err_fg),
            };
            ui.label(RichText::new(badge).size(12.5).strong().color(col));
            if let Some(log) = self.wb.audit.log.as_ref() {
                ui.label(
                    RichText::new(format!(
                        "文档标识：{} · 审计链高度：#{}（内嵌明细 {} 条 + 折叠归档 {} 条）· 占用 {:.1} KB / {} KB 上限",
                        &log.doc_id[..log.doc_id.len().min(12)],
                        ver.head_seq,
                        ver.detailed_commit_count,
                        ver.rolled_up_commit_count,
                        ver.embedded_bytes as f64 / 1024.0,
                        log.max_embedded_bytes / 1024,
                    ))
                    .size(11.5)
                    .weak(),
                );
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui_kit::primary_button(ui, "查看编辑与防篡改记录").clicked() {
                    self.open_audit_panel();
                    *close = true;
                }
                if ui_kit::secondary_button(ui, "导出对外干净副本 (.xlsx)…").clicked() {
                    *close = true;
                    self.export_clean_copy_dialog();
                }
            });
        } else if self.audit_state.auto_enable_active(&self.wb) {
            ui.label(
                RichText::new("已默认开启 · 首次保存 .xlsx 时自动生成防篡改签名链")
                    .size(12.5)
                    .strong()
                    .color(pal.accent),
            );
            ui.label(
                RichText::new("新建工作簿与在 CELLAN 中编辑保存的 .xlsx 默认开启去中心化防篡改审计，随文件内嵌 ECDSA P-256 签名链与单元格溯源（纯只读查看旧文件不会修改原文件）。")
                    .size(11.5)
                    .weak(),
            );
            ui.add_space(4.0);
            if ui_kit::primary_button(ui, "查看编辑与防篡改记录").clicked() {
                self.open_audit_panel();
                *close = true;
            }
        } else {
            ui.label(
                RichText::new("当前文件格式（.xls）不支持内嵌防篡改签名链；如需开启请先另存为 .xlsx。")
                    .size(11.5)
                    .weak(),
            );
            ui.add_space(4.0);
            if ui_kit::secondary_button(ui, "查看本次编辑记录").clicked() {
                self.open_audit_panel();
                *close = true;
            }
        }
    }

    /// 统一的「编辑与防篡改记录」单页列表面板（融合 P2 表格体验、动态用户下拉筛选、防篡改验真与导出）
    pub(crate) fn draw_unified_audit_panel(
        &mut self,
        ui: &mut egui::Ui,
        jump: &mut Option<(String, u32, u32)>,
        revert: &mut Option<usize>,
    ) {
        let pal = crate::theme::palette();
        let mut do_export_csv = false;
        let mut do_export_clean = false;
        let mut do_export_archive = false;
        let mut do_verify_archive = false;
        let mut do_reverify = false;

        let log_opt = self.wb.audit.log.clone();
        let ver_opt = self.audit_state.verification.clone();

        // 1. 顶部紧凑防篡改状态横幅
        if let Some(ref ver) = ver_opt {
            let (title, fg, bg) = match ver.severity {
                AuditSeverity::Verified => (
                    format!("✓ 防篡改核验完整（共 #{} 版）", ver.head_seq),
                    pal.accent,
                    pal.accent_tint,
                ),
                AuditSeverity::Warning => (
                    format!("⚠ 核验通过但有 {} 项提醒（共 #{} 版）", ver.issues.len(), ver.head_seq),
                    pal.warn_fg,
                    pal.warn_bg,
                ),
                AuditSeverity::Tampered => (
                    format!("✕ 检测到 {} 项高危防篡改异常！", ver.issues.len()),
                    pal.err_fg,
                    pal.err_bg,
                ),
            };
            egui::Frame::NONE
                .fill(bg)
                .stroke(egui::Stroke::new(1.0_f32, fg.gamma_multiply(0.45)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).strong().size(12.0).color(fg));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("重新核验").clicked() {
                                do_reverify = true;
                            }
                            if let Some(ref log) = log_opt {
                                let genesis_who = log
                                    .commits
                                    .first()
                                    .and_then(|c| log.author_of(c.committer_idx))
                                    .map(|a| a.display_name.as_str())
                                    .unwrap_or("");
                                let genesis_hint = if genesis_who.is_empty() {
                                    String::new()
                                } else {
                                    format!("基线:{genesis_who} · ")
                                };
                                ui.label(
                                    RichText::new(format!(
                                        "{genesis_hint}ID:{} · {:.1}/{}KB · 溯源 {}格",
                                        &log.doc_id[..log.doc_id.len().min(8)],
                                        ver.embedded_bytes as f64 / 1024.0,
                                        log.max_embedded_bytes / 1024,
                                        ver.blame_count
                                    ))
                                    .size(10.5)
                                    .color(pal.text_weak),
                                );
                            }
                        });
                    });
                    if !ver.issues.is_empty() {
                        ui.add_space(3.0);
                        for issue in &ver.issues {
                            let icol = match issue.severity {
                                AuditSeverity::Tampered => pal.err_fg,
                                AuditSeverity::Warning => pal.warn_fg,
                                AuditSeverity::Verified => pal.accent,
                            };
                            ui.label(RichText::new(format!("• {}", issue.message)).size(11.0).color(icol));
                        }
                    }
                });
        } else if self.audit_state.auto_enable_active(&self.wb) {
            egui::Frame::NONE
                .fill(pal.accent_tint)
                .stroke(egui::Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.45)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("✓ 防篡改保护已默认开启 · 保存 .xlsx 时将自动生成 ECDSA P-256 不可篡改签名链")
                            .size(11.5)
                            .color(pal.accent),
                    );
                });
        } else {
            egui::Frame::NONE
                .fill(pal.warn_bg)
                .stroke(egui::Stroke::new(1.0_f32, pal.warn_fg.gamma_multiply(0.45)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("ℹ 当前格式（.xls）不支持内嵌防篡改签名链，仅展示会话修订；另存为 .xlsx 后将自动开启防篡改保护。")
                            .size(11.5)
                            .color(pal.warn_fg),
                    );
                });
        }

        ui.add_space(6.0);

        // 2. 组装统一记录行 & 动态用户下拉选项（仅显示编辑修改过的用户，含记录数并按记录数降序排列）
        let all_rows = self.build_unified_audit_rows();
        let user_counts = collect_user_filter_counts(&all_rows);
        if !self.rev_filter_who.is_empty()
            && !user_counts.iter().any(|(u, _)| u == &self.rev_filter_who)
        {
            self.rev_filter_who.clear();
        }
        let kw_q = self.rev_filter_kw.trim().to_lowercase();
        let filtered_indices: Vec<usize> = all_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if !self.rev_filter_who.is_empty() && r.user_name != self.rev_filter_who {
                    return false;
                }
                if !kw_q.is_empty() && !r.search_haystack.contains(&kw_q) {
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        // 3. 统一筛选与导出工具栏（动态用户下拉框 + 关键词搜索 + 导出功能）
        let selected_user_text = if self.rev_filter_who.is_empty() {
            format!("全部用户 ({})", all_rows.len())
        } else {
            let cnt = user_counts
                .iter()
                .find(|(u, _)| u == &self.rev_filter_who)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            format!("{} ({cnt})", self.rev_filter_who)
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "显示 {} / {} 条",
                    filtered_indices.len(),
                    all_rows.len()
                ))
                .strong(),
            );
            egui::ComboBox::from_id_salt("audit_user_filter_combo")
                .width(152.0)
                .selected_text(selected_user_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.rev_filter_who,
                        String::new(),
                        format!("全部用户 ({})", all_rows.len()),
                    );
                    if !user_counts.is_empty() {
                        ui.separator();
                        for (uname, cnt) in &user_counts {
                            ui.selectable_value(
                                &mut self.rev_filter_who,
                                uname.clone(),
                                format!("{uname} ({cnt})"),
                            );
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.rev_filter_kw)
                    .hint_text("按表/位置/内容筛选")
                    .desired_width(155.0),
            );
            if ui.small_button("导出 CSV…").clicked() {
                do_export_csv = true;
            }
            if log_opt.is_some() {
                if ui
                    .small_button("导出干净副本…")
                    .on_hover_text("生成不含审计记录的脱敏 .xlsx，用于发给外部客户/供应商")
                    .clicked()
                {
                    do_export_clean = true;
                }
                if ui
                    .small_button("验证冷归档…")
                    .on_hover_text("导入历史折叠导出的 .cellan-audit 归档包并核验 Merkle 根")
                    .clicked()
                {
                    do_verify_archive = true;
                }
                if log_opt.as_ref().is_some_and(|l| l.last_compacted.is_some())
                    && ui.small_button("导出冷归档…").clicked()
                {
                    do_export_archive = true;
                }
            }
        });

        if !self.rev_msg.is_empty() {
            let ok = self.rev_msg.starts_with("已撤回") || self.rev_msg.starts_with("已导出");
            ui.add_space(3.0);
            ui.label(
                RichText::new(&self.rev_msg)
                    .size(11.5)
                    .color(if ok { pal.accent } else { pal.err_fg }),
            );
        }
        if let Some((ok, ref msg)) = self.audit_archive_msg {
            ui.add_space(3.0);
            ui.label(
                RichText::new(msg)
                    .size(11.5)
                    .color(if ok { pal.accent } else { pal.err_fg }),
            );
        }

        ui.add_space(4.0);

        // 4. P2 风格 5 列表头
        ui.horizontal(|ui| {
            crate::dlg_col(ui, 98.0, |ui| {
                ui.label(RichText::new("时间").size(10.5).weak());
            });
            crate::dlg_col(ui, 112.0, |ui| {
                ui.label(RichText::new("用户").size(10.5).weak());
            });
            crate::dlg_col(ui, 108.0, |ui| {
                ui.label(RichText::new("动作").size(10.5).weak());
            });
            crate::dlg_col(ui, 126.0, |ui| {
                ui.label(RichText::new("位置").size(10.5).weak());
            });
            crate::dlg_col(ui, 250.0, |ui| {
                ui.label(RichText::new("改动").size(10.5).weak());
            });
            ui.label(RichText::new("状态").size(10.5).weak());
        });
        ui.separator();

        let my_pk = self.my_audit_pubkey().map(|s| s.to_string());

        if filtered_indices.is_empty() && log_opt.as_ref().is_none_or(|l| l.rollups.is_empty()) {
            ui.add_space(12.0);
            let empty_txt = if all_rows.is_empty() {
                "尚无编辑修改记录。修改单元格后会实时记录，按 Ctrl+S 保存 .xlsx 时自动签名保护。"
            } else {
                "没有匹配的编辑记录。"
            };
            ui.label(RichText::new(empty_txt).weak());
            ui.add_space(12.0);
        }

        // 5. 统一记录滚动表 + 底部历史冷归档节点
        egui::ScrollArea::vertical()
            .id_salt("unified_audit_scroll")
            .max_height(390.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for &idx in &filtered_indices {
                    let row = &all_rows[idx];
                    ui.horizontal(|ui| {
                        crate::dlg_col(ui, 98.0, |ui| {
                            let ts_txt = if row.pending {
                                crate::rel_time(row.ts_ms)
                            } else {
                                let full = sheetcore::audit::format_local_ms(row.ts_ms);
                                full.get(5..16).unwrap_or(&full).to_string()
                            };
                            let lbl = ui.label(RichText::new(ts_txt).size(11.0).weak());
                            if !row.hover_tip.is_empty() {
                                lbl.on_hover_text(&row.hover_tip);
                            }
                        });
                        crate::dlg_col(ui, 112.0, |ui| {
                            let mut name_text = row.user_name.clone();
                            let mut name_color: Option<Color32> = None;
                            let mut author_tip = String::new();
                            if let Some(ref au) = row.author {
                                let badge = audit_store::trust_badge_for_author(au, my_pk.as_deref());
                                let bt = match badge {
                                    AuthorTrustBadge::SelfDevice => "本机签名",
                                    AuthorTrustBadge::KnownTrusted => "已信任设备",
                                    AuthorTrustBadge::FirstSeen => "首次见设备",
                                    AuthorTrustBadge::KeyChanged => {
                                        name_text = format!("⚠ {}", row.user_name);
                                        name_color = Some(pal.err_fg);
                                        "⚠ 身份密钥发生变化"
                                    }
                                };
                                author_tip = format!(
                                    "{}（Windows：{} · 设备：{}）\n信任状态：{bt} · 指纹：{}",
                                    au.display_name,
                                    if au.os_user.is_empty() { "-" } else { &au.os_user },
                                    if au.machine.is_empty() { "-" } else { &au.machine },
                                    au.fingerprint(),
                                );
                            }
                            let mut rt = RichText::new(name_text).strong().size(11.5);
                            if let Some(c) = name_color {
                                rt = rt.color(c);
                            }
                            let lbl = ui.add(egui::Label::new(rt).truncate());
                            if !author_tip.is_empty() {
                                lbl.on_hover_text(author_tip);
                            }
                        });
                        crate::dlg_col(ui, 108.0, |ui| {
                            let rt = if row.pending {
                                RichText::new(&row.action).size(11.0).color(pal.warn_fg)
                            } else {
                                RichText::new(&row.action).size(11.5)
                            };
                            let lbl = ui.add(egui::Label::new(rt).truncate());
                            if !row.hover_tip.is_empty() {
                                lbl.on_hover_text(&row.hover_tip);
                            }
                        });
                        crate::dlg_col(ui, 126.0, |ui| {
                            if let Some((ref sh, r, c)) = row.jump {
                                if ui.link(&row.location).clicked() {
                                    *jump = Some((sh.clone(), r, c));
                                }
                            } else {
                                ui.add(
                                    egui::Label::new(RichText::new(&row.location).size(11.0).weak())
                                        .truncate(),
                                );
                            }
                        });
                        crate::dlg_col(ui, 250.0, |ui| {
                            let lbl = ui.add(
                                egui::Label::new(RichText::new(&row.detail).size(11.0))
                                    .truncate(),
                            );
                            if !row.hover_tip.is_empty() {
                                lbl.on_hover_text(format!("{}\n\n{}", row.detail, row.hover_tip));
                            } else {
                                lbl.on_hover_text(&row.detail);
                            }
                        });
                        if let Some(rev_idx) = row.revert_rev_idx {
                            if ui.small_button("撤回").clicked() {
                                *revert = Some(rev_idx);
                            }
                        } else if let Some(ref btxt) = row.badge_text {
                            let col = if row.badge_is_warn {
                                pal.err_fg
                            } else if row.pending {
                                pal.warn_fg
                            } else {
                                pal.accent
                            };
                            let lbl = ui.label(RichText::new(btxt).size(10.5).color(col));
                            if !row.hover_tip.is_empty() {
                                lbl.on_hover_text(&row.hover_tip);
                            }
                        }
                    });
                    ui.separator();
                }

                // 历史滚动折叠归档节点
                if let Some(ref log) = log_opt {
                    for (ridx, rollup) in log.rollups.iter().enumerate().rev() {
                        let authors_txt: String = rollup
                            .author_counts
                            .iter()
                            .filter_map(|&(au, cnt)| {
                                log.author_of(au).map(|a| format!("{}({cnt}次)", a.display_name))
                            })
                            .collect::<Vec<_>>()
                            .join("、");
                        egui::Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(120, 130, 145, 16))
                            .corner_radius(5.0)
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "📦 历史归档折叠 #{}..#{}（{} ~ {}）",
                                        rollup.from_seq,
                                        rollup.to_seq,
                                        sheetcore::audit::format_local_ms(rollup.from_ts_ms),
                                        sheetcore::audit::format_local_ms(rollup.to_ts_ms),
                                    ))
                                    .strong()
                                    .size(11.5),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "参与人：{authors_txt} · 累计 {} 格改动 · Merkle根：{}…（可导入 .cellan-audit 归档包验真）",
                                        rollup.total_cell_changes,
                                        &rollup.commit_merkle_root[..rollup.commit_merkle_root.len().min(14)],
                                    ))
                                    .size(10.5)
                                    .weak(),
                                );
                                if let Some((ref res, ref arch)) = self.audit_state.verified_cold_archive
                                    && res.matched_rollup_idx == Some(ridx)
                                {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(format!("✓ 已加载冷归档包：{}", res.summary))
                                            .size(11.0)
                                            .color(Color32::from_rgb(16, 124, 65)),
                                    );
                                    for hc in arch.commits.iter().rev().take(30) {
                                        let who = arch
                                            .author_dict
                                            .get(hc.committer_idx as usize)
                                            .map(|a| a.display_name.as_str())
                                            .unwrap_or("?");
                                        ui.label(
                                            RichText::new(format!(
                                                "  #{} {} · {} · {}（{} 格）",
                                                hc.seq,
                                                sheetcore::audit::format_local_ms(hc.ts_ms),
                                                who,
                                                hc.action,
                                                hc.total_cell_changes
                                            ))
                                            .size(10.5),
                                        );
                                    }
                                }
                            });
                        ui.add_space(4.0);
                    }
                }
            });

        if do_reverify {
            self.refresh_audit_verification_now();
        }
        if do_export_csv {
            let rows_to_export: Vec<&UnifiedAuditRow> =
                filtered_indices.iter().map(|&i| &all_rows[i]).collect();
            if let Some(dest) = rfd::FileDialog::new()
                .set_file_name("编辑与防篡改记录.csv")
                .add_filter("CSV 表格", &["csv"])
                .save_file()
            {
                let csv = export_unified_rows_csv(&rows_to_export);
                match std::fs::write(&dest, csv.as_bytes()) {
                    Ok(_) => {
                        self.audit_archive_msg = Some((
                            true,
                            format!("已导出 {} 条记录至 {}", rows_to_export.len(), dest.display()),
                        ));
                    }
                    Err(e) => {
                        self.audit_archive_msg = Some((false, format!("导出失败：{e}")));
                    }
                }
            }
        }
        if do_export_clean {
            self.export_clean_copy_dialog();
        }
        if do_export_archive {
            self.export_cold_archive_dialog();
        }
        if do_verify_archive {
            self.verify_cold_archive_dialog();
        }
    }

    /// 渲染单元格最后修改溯源（Blame）弹窗
    pub(crate) fn draw_audit_dialogs(&mut self, ctx: &egui::Context) {
        if let Some((sheet_name, row, col)) = self.audit_blame_popup.clone() {
            let mut open = true;
            let mut open_full_audit = false;
            let cell_coord = format!("{sheet_name}!{}", sheetcore::model::cell_name(row, col));
            let cur_summary = self
                .wb
                .sheets
                .iter()
                .find(|s| s.name == sheet_name)
                .and_then(|s| s.get_view(row, col))
                .map(|v| AuditSnap::from_cell_view(Some(v)).summary())
                .unwrap_or_else(|| "(空)".into());
            let my_pk = self.my_audit_pubkey().map(|s| s.to_string());

            ui_kit::window(ctx, format!("单元格修改溯源 — {cell_coord}"), 480.0)
                .open(&mut open)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
                .show(ctx, |ui| {
                    ui.label(RichText::new(format!("当前内容：{cur_summary}")).strong().size(12.5));
                    ui.add_space(6.0);
                    if let Some(log) = self.wb.audit.log.as_ref() {
                        let live_sheet = self.wb.sheets.iter().find(|s| s.name == sheet_name);
                        // 若存在未落盘的插删行列操作，将当前坐标逆映射回已提交坐标查询 Blame
                        let committed_coord = live_sheet.and_then(|sh| {
                            let mut cur = Some((row, col));
                            for sop in self.audit_state.pending_structural_ops.iter().rev() {
                                if sop.sheet_id == sh.identity {
                                    cur = cur.and_then(|(r, c)| invert_structural_coord(r, c, &sop.op));
                                }
                            }
                            cur
                        });
                        let blame_opt = match (live_sheet, committed_coord) {
                            (Some(sh), Some((cr, cc))) => {
                                log.lookup_blame_for_sheet(&self.wb, sh, cr, cc)
                            }
                            (None, _) => log.lookup_blame(&sheet_name, row, col),
                            _ => None,
                        };
                        if let Some(blame) = blame_opt {
                            let pal = crate::theme::palette();
                            let badge = audit_store::trust_badge_for_author(&blame.author, my_pk.as_deref());
                            let (bt, bc) = match badge {
                                AuthorTrustBadge::SelfDevice => ("本机签名", pal.accent),
                                AuthorTrustBadge::KnownTrusted => ("已信任设备", pal.accent),
                                AuthorTrustBadge::FirstSeen => ("首次见设备", pal.text_weak),
                                AuthorTrustBadge::KeyChanged => ("⚠ 身份指纹变化", pal.err_fg),
                            };
                            let (role_title, who_prefix) = if blame.is_baseline {
                                ("基线登记人（开启审计时已存在，尚未发生审计后修改）", "登记人")
                            } else {
                                ("最后修改人（持久 Blame 溯源）", "作者")
                            };
                            egui::Frame::NONE
                                .fill(pal.accent_tint)
                                .stroke(egui::Stroke::new(1.0, pal.accent.gamma_multiply(0.35)))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.label(RichText::new(role_title).strong().size(12.0));
                                    ui.label(format!(
                                        "{who_prefix}：{}（Windows 账户：{} · 设备：{}）",
                                        blame.author.display_name,
                                        if blame.author.os_user.is_empty() { "-" } else { &blame.author.os_user },
                                        if blame.author.machine.is_empty() { "-" } else { &blame.author.machine },
                                    ));
                                    ui.horizontal(|ui| {
                                        ui.label(format!("提交版本：#{} · 时间：{}", blame.seq, sheetcore::audit::format_local_ms(blame.ts_ms)));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(format!("公钥指纹：{}", blame.author.fingerprint())).size(11.0).weak());
                                        ui.label(RichText::new(format!("[{bt}]")).size(11.0).color(bc));
                                    });
                                });
                        } else {
                            ui.label(RichText::new("该单元格尚无已落盘的审计记录（可能为本次未保存新建或空单元格）。").weak());
                        }

                        ui.add_space(8.0);
                        let history = match (live_sheet, committed_coord) {
                            (Some(sh), Some((cr, cc))) => {
                                log.cell_history_for_sheet(&self.wb, sh, cr, cc)
                            }
                            (None, _) => log.cell_history(&sheet_name, row, col),
                            _ => Vec::new(),
                        };
                        ui.label(RichText::new(format!("近期详细修改记录（{} 条）", history.len())).strong().size(12.0));
                        ui.separator();
                        if history.is_empty() {
                            ui.label(RichText::new("当前详细窗口内无逐格变动明细（早期修改已归档折叠，但上方 Blame 仍保留最后修改人）。").size(11.5).weak());
                        } else {
                            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                for h in &history {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(format!("#{}", h.seq)).strong().color(ui_kit::ACCENT));
                                        ui.label(RichText::new(sheetcore::audit::format_local_ms(h.ts_ms)).size(11.0).weak());
                                        ui.label(RichText::new(h.author.short_label()).strong().size(11.5));
                                        ui.label(format!("{} → {}", h.before.summary(), h.after.summary()));
                                    });
                                }
                            });
                        }
                        ui.add_space(8.0);
                        ui_kit::footer(ui, |ui| {
                            if ui_kit::primary_button(ui, "查看全部编辑与防篡改记录").clicked() {
                                open_full_audit = true;
                            }
                            if ui_kit::secondary_button(ui, "关闭").clicked() {
                                open = false;
                            }
                        });
                    } else if self.audit_state.auto_enable_active(&self.wb) {
                        ui.label("防篡改编辑记录已默认开启。按 Ctrl+S 保存为 .xlsx 后，将自动生成签名审计链并可查看该单元格的修改人与历史记录。");
                        ui.add_space(8.0);
                        ui_kit::footer(ui, |ui| {
                            if ui_kit::primary_button(ui, "查看编辑与防篡改记录").clicked() {
                                open_full_audit = true;
                            }
                            if ui_kit::secondary_button(ui, "关闭").clicked() {
                                open = false;
                            }
                        });
                    } else {
                        ui.label("当前文档为 .xls 格式或存在审计异常，暂无可用的已签名单元格溯源记录；另存为 .xlsx 后将自动开启防篡改审计。");
                        ui.add_space(8.0);
                        ui_kit::footer(ui, |ui| {
                            if ui_kit::primary_button(ui, "查看编辑记录").clicked() {
                                open_full_audit = true;
                            }
                            if ui_kit::secondary_button(ui, "关闭").clicked() {
                                open = false;
                            }
                        });
                    }
                });
            if open_full_audit {
                self.audit_blame_popup = None;
                self.open_audit_panel();
            } else if !open {
                self.audit_blame_popup = None;
            }
        }
    }
}

/// 统一「编辑与防篡改记录」面板的单行展示结构
#[derive(Clone, Debug)]
pub(crate) struct UnifiedAuditRow {
    pub ts_ms: u64,
    pub pending: bool,
    pub commit_seq: Option<u64>,
    pub user_name: String,
    pub author: Option<AuditAuthor>,
    pub action: String,
    pub location: String,
    pub jump: Option<(String, u32, u32)>,
    pub detail: String,
    pub hover_tip: String,
    pub revert_rev_idx: Option<usize>,
    pub badge_text: Option<String>,
    pub badge_is_warn: bool,
    pub search_haystack: String,
}

fn normalize_user_name(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        "未知用户".to_string()
    } else {
        t.to_string()
    }
}

/// 动态汇总编辑修改过该工作簿的用户及其记录数，严格按记录数降序排列（同数按用户名升序）
pub(crate) fn collect_user_filter_counts(rows: &[UnifiedAuditRow]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for r in rows {
        *counts.entry(normalize_user_name(&r.user_name)).or_insert(0) += 1;
    }
    let mut list: Vec<(String, usize)> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    list
}

fn csv_escape_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub(crate) fn export_unified_rows_csv(rows: &[&UnifiedAuditRow]) -> String {
    let mut out = String::from("\u{FEFF}状态,版本,时间,用户,动作,位置,改动\r\n");
    for r in rows {
        let status = if r.pending { "待保存" } else { "已签名" };
        let seq = r
            .commit_seq
            .map(|s| format!("#{s}"))
            .unwrap_or_else(|| "-".into());
        let ts = sheetcore::audit::format_local_ms(r.ts_ms);
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\r\n",
            csv_escape_field(status),
            csv_escape_field(&seq),
            csv_escape_field(&ts),
            csv_escape_field(&r.user_name),
            csv_escape_field(&r.action),
            csv_escape_field(&r.location),
            csv_escape_field(&r.detail),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheetcore::audit::{
        author_contribution_digest, generate_keypair, verify_workbook_audit, AuditIssueKind,
        AuditLog, AuditSeverity, AuditSnap,
    };
    use sheetcore::model::{Cell, Value, Workbook};
    use std::time::{Duration, Instant};

    fn wait_event(
        net: &crate::net::Net,
        timeout: Duration,
        mut pred: impl FnMut(&crate::net::NetEvent) -> bool,
    ) -> Option<crate::net::NetEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            while let Ok(ev) = net.events.try_recv() {
                if pred(&ev) {
                    return Some(ev);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn lan_collab_verified_identity_cosign_and_p2p_watermark_sync() {
        let env = crate::test_env::TestEnv::new("audit-lan-collab");
        let shared_file = env.root().join("shared_ledger.xlsx");

        let host_kp = generate_keypair().unwrap();
        let guest_kp = generate_keypair().unwrap();
        let host_author = audit_store::current_author("宿主张三", &host_kp);
        let guest_author = audit_store::current_author("访客李四", &guest_kp);

        // 1. 宿主新建工作簿并开启防篡改审计，保存基线 #1
        let mut host_wb = Workbook::new_blank();
        host_wb.sheets[0].set(0, 0, Cell { value: Value::Number(1000.0), formula: None });
        let init_log = AuditLog::enable(&host_wb, host_author.clone(), &host_kp, 1_700_000_000_000).unwrap();
        host_wb.audit.log = Some(Arc::new(init_log.clone()));
        sheetcore::xlsx::save_atomic(&shared_file, &host_wb).unwrap();
        host_wb.path = Some(shared_file.clone());

        // 2. 启动真实本地环回协作房间，宿主宣告审计身份并发布 AuditSync
        let host_net = crate::net::host_auto_initialized(
            1,
            "宿主张三".into(),
            "shared_ledger.xlsx".into(),
            "room-secret".into(),
            "sess-audit-1".into(),
            1_700_000_000_000,
            |n| {
                let mut peer = sheetcore::sync::Peer::new(1);
                peer.seed_workbook(&host_wb);
                n.seed_snapshot(
                    &peer,
                    &host_wb,
                    sheetcore::sync::Cursor {
                        sheet: 0,
                        site: 1,
                        name: "宿主张三".into(),
                        color: 1,
                        row: 0,
                        col: 0,
                        end_row: None,
                        end_col: None,
                    },
                );
            },
        )
        .unwrap();
        host_net.announce_audit_identity("宿主张三", &host_kp);
        host_net.broadcast_audit_sync(&init_log);

        // 3. 访客连入，自动收到宿主身份与 AuditSync；随后宣告自己的签名身份
        let addr = format!("127.0.0.1:{}", host_net.port);
        let guest_net = crate::net::join(
            2,
            "访客李四".into(),
            "shared_ledger.xlsx".into(),
            &addr,
            "room-secret".into(),
        )
        .unwrap();

        let got_sync = wait_event(&guest_net, Duration::from_secs(5), |ev| {
            matches!(ev, crate::net::NetEvent::AuditSync { head_seq: 1, .. })
        });
        assert!(got_sync.is_some(), "访客入房必须收到宿主当前的 AuditSync");

        guest_net.announce_audit_identity("访客李四", &guest_kp);
        let host_got_id = wait_event(&host_net, Duration::from_secs(5), |ev| {
            matches!(ev, crate::net::NetEvent::PeerAuditIdentity { site: 2, .. })
        });
        let verified_guest = match host_got_id {
            Some(crate::net::NetEvent::PeerAuditIdentity { author, .. }) => author,
            other => panic!("宿主必须收到并验签通过访客身份，实际：{other:?}"),
        };
        assert_eq!(verified_guest.pubkey_hex, guest_kp.pubkey_hex);

        // 4. 访客编辑 B1=520 并发送 ECDSA P-256 独立联署
        let guest_cells = vec![(
            "Sheet1".to_string(),
            0,
            1,
            AuditSnap::Empty,
            AuditSnap::Num(520.0),
        )];
        let g_digest = author_contribution_digest(&init_log.doc_id, 1, &guest_cells);
        guest_net.send_audit_attest(&init_log.doc_id, 1, &g_digest, &guest_kp);

        let host_got_attest = wait_event(&host_net, Duration::from_secs(5), |ev| {
            matches!(ev, crate::net::NetEvent::PeerAuditAttest { site: 2, .. })
        });
        let (att_digest, att_sig) = match host_got_attest {
            Some(crate::net::NetEvent::PeerAuditAttest { changes_digest, sig_hex, .. }) => {
                (changes_digest, sig_hex)
            }
            other => panic!("宿主必须收到并验签通过访客独立联署，实际：{other:?}"),
        };

        // 5. 宿主应用访客改动并通过 DocumentTasks 后台保存生成第 #2 版协作提交
        let mut host_audit_state = DocAuditState::default();
        host_audit_state.baseline = Some(Box::new(host_wb.clone()));
        host_audit_state.peer_identities.insert(2, verified_guest.clone());
        host_audit_state.dirty_cell_authors.insert(
            (host_wb.sheets[0].identity, 0, 1),
            verified_guest.clone(),
        );
        host_audit_state.record_peer_attestation(
            2,
            PeerAttestationInput {
                author: guest_author.clone(),
                changes_digest: att_digest,
                sig_hex: att_sig,
                attested_by_peer: true,
            },
        );
        host_wb.sheets[0].set(0, 1, Cell { value: Value::Number(520.0), formula: None });
        host_wb.dirty = true;

        let mut tasks = DocumentTasks::default();
        let mut kp_cache = Some(host_kp.clone());
        SheetApp::prepare_audit_save_ctx_for(
            &host_wb,
            &host_audit_state,
            &mut tasks,
            Some(&host_net),
            "宿主张三",
            &mut kp_cache,
            0,
        );
        tasks.start_save(&mut host_wb, shared_file.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saved = false;
        while Instant::now() < deadline {
            if let Some(res) = tasks.poll_save(&mut host_wb) {
                res.expect("后台审计保存必须成功");
                saved = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(saved, "后台保存应在超时前完成");

        let _ = SheetApp::on_audit_save_completed_for(
            &host_wb,
            &mut host_audit_state,
            &mut tasks,
            Some(&host_net),
            &shared_file,
            Some(&host_kp.pubkey_hex),
        );

        // 6. 访客必须收到宿主广播的第 #2 版 AuditSync，刷新本机水印，且落盘文件校验完全通过！
        let got_sync2 = wait_event(&guest_net, Duration::from_secs(5), |ev| {
            matches!(ev, crate::net::NetEvent::AuditSync { head_seq: 2, .. })
        });
        let payload2 = match got_sync2 {
            Some(crate::net::NetEvent::AuditSync { payload_b64, .. }) => payload_b64,
            other => panic!("访客必须收到第 2 版 AuditSync，实际：{other:?}"),
        };
        let synced_log = AuditLog::decode_base64(&payload2).unwrap();
        assert_eq!(synced_log.head_seq(), 2);
        assert_eq!(synced_log.commits[1].action, "协作保存");
        assert_eq!(synced_log.commits[1].co_signers.len(), 1);
        assert!(synced_log.commits[1].co_signers[0].attested_by_peer);

        // 从磁盘重新打开，验证 ECDSA 主签 + 访客独立联署 + Blame 精确归属访客李四！
        let reopened = sheetcore::xlsx::load(&shared_file).unwrap();
        let v = verify_workbook_audit(&reopened).unwrap();
        assert_eq!(v.severity, AuditSeverity::Verified, "{:?}", v.issues);
        let blame = reopened
            .audit
            .log
            .as_ref()
            .unwrap()
            .lookup_blame("Sheet1", 0, 1)
            .expect("B1 Blame 必须存在");
        assert_eq!(blame.author.pubkey_hex, guest_kp.pubkey_hex);
        assert_eq!(blame.seq, 2);

        // 7. 模拟事后有人拿旧备份（第 #1 版）覆盖共享文件 -> 本机防回滚水印立刻抓出！
        let mut old_backup = Workbook::new_blank();
        old_backup.sheets[0].set(0, 0, Cell { value: Value::Number(1000.0), formula: None });
        old_backup.audit.log = Some(Arc::new(init_log));
        let v_rb = audit_store::verify_workbook_full(
            Some(&shared_file),
            &old_backup,
            Some(&host_kp.pubkey_hex),
        )
        .unwrap();
        assert_eq!(v_rb.severity, AuditSeverity::Tampered);
        assert!(v_rb.issues.iter().any(|i| i.kind == AuditIssueKind::RollbackDetected));

        guest_net.shutdown();
        host_net.shutdown();
    }

    #[test]
    fn sheet_app_end_to_end_enable_net_diff_external_tamper_gap_and_clean_export() {
        let env = crate::test_env::TestEnv::new("audit-sheetapp-e2e");
        let ledger = env.root().join("finance_ledger.xlsx");
        let clean_copy = env.root().join("finance_ledger_clean.xlsx");

        // 1. 创建普通工作簿并落盘
        let mut init_wb = Workbook::new_blank();
        init_wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        init_wb.sheets[0].set(0, 1, Cell { value: Value::Number(10.0), formula: None });
        sheetcore::xlsx::save_atomic(&ledger, &init_wb).unwrap();

        let wait_open = |app: &mut SheetApp| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.tasks.open_rx.is_some() && Instant::now() < deadline {
                app.poll_open();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(app.tasks.open_rx.is_none(), "打开工作簿超时");
        };
        let wait_save = |app: &mut SheetApp| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.has_pending_save() && Instant::now() < deadline {
                app.poll_save();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!app.has_pending_save(), "保存工作簿超时");
        };

        // 2. 通过真实 SheetApp 打开并开启防篡改审计（保存 #1 基线）
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "王财务".into();
        app.open_path(ledger.clone());
        wait_open(&mut app);
        assert!(app.wb.audit.log.is_none());

        app.enable_audit_for_current_workbook();
        app.save(false);
        wait_save(&mut app);
        let ver1 = app.audit_state.verification.as_ref().expect("开启审计后应有核验状态");
        assert_eq!(ver1.severity, AuditSeverity::Verified, "{:?}", ver1.issues);
        assert_eq!(ver1.head_seq, 1);

        // 3. 会话内多次改写与撤回对冲：A1: 100->200->150，B1: 10->999->10
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(200.0), formula: None });
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(150.0), formula: None });
        app.wb.sheets[0].set(0, 1, Cell { value: Value::Number(999.0), formula: None });
        app.wb.sheets[0].set(0, 1, Cell { value: Value::Number(10.0), formula: None });
        app.wb.dirty = true;
        app.save(false);
        wait_save(&mut app);

        let log2 = app.wb.audit.log.as_ref().unwrap();
        assert_eq!(log2.head_seq(), 2);
        let c2 = &log2.commits[1];
        assert_eq!(c2.total_cell_changes, 1, "B1 撤回对冲应自动抵消，仅留 A1 净变更");
        assert_eq!(c2.cells.len(), 1);
        assert_eq!((c2.cells[0].row, c2.cells[0].col), (0, 0));
        assert_eq!(c2.cells[0].before, AuditSnap::Num(100.0));
        assert_eq!(c2.cells[0].after, AuditSnap::Num(150.0));

        // 4. 模拟外部 Excel/WPS 绕过 CELLAN 偷偷把 A1 改成 88888（保留 customXml 但不追加签名提交）
        let mut ext_wb = sheetcore::xlsx::load(&ledger).unwrap();
        ext_wb.sheets[0].set(0, 0, Cell { value: Value::Number(88888.0), formula: None });
        sheetcore::xlsx::save_atomic(&ledger, &ext_wb).unwrap();

        // 5. 新 SheetApp 重新打开：必须亮红灯 NoticeBanner + ExternalModification + pending_external_gap
        let mut app2 = SheetApp::new_with_context(&egui::Context::default(), None);
        app2.my_name = "赵审核".into();
        app2.open_path(ledger.clone());
        wait_open(&mut app2);
        let ver_ext = app2.audit_state.verification.as_ref().expect("打开被外改文件必须有核验结论");
        assert_eq!(ver_ext.severity, AuditSeverity::Tampered);
        assert!(ver_ext
            .issues
            .iter()
            .any(|i| i.kind == AuditIssueKind::ExternalModification));
        assert!(app2.audit_state.pending_external_gap.is_some());
        assert!(app2.notice.as_ref().is_some_and(|n| {
            n.actions.iter().any(|(_, a)| matches!(a, NoticeAction::OpenAudit))
        }));

        // 6. 赵审核继续合法修改 C1=42 并保存：永久把外部修改缺口钉在 #3 提交里
        app2.wb.sheets[0].set(0, 2, Cell { value: Value::Number(42.0), formula: None });
        app2.wb.dirty = true;
        app2.save(false);
        wait_save(&mut app2);

        let log3 = app2.wb.audit.log.as_ref().unwrap();
        assert_eq!(log3.head_seq(), 3);
        assert!(log3.commits[2].external_gap_hash.is_some(), "#3 提交必须永久记录外部修改缺口指纹");
        assert!(app2.audit_state.pending_external_gap.is_none());

        // 重新从磁盘加载，确认状态为 Warning（永久提示曾存在外部修改缺口，绝不默默洗白为 Verified）
        let reopened3 = sheetcore::xlsx::load(&ledger).unwrap();
        let ver3 = verify_workbook_audit(&reopened3).unwrap();
        assert_eq!(ver3.severity, AuditSeverity::Warning);
        assert!(ver3
            .issues
            .iter()
            .any(|i| i.kind == AuditIssueKind::HistoricalExternalGap));

        // 7. 导出对外干净副本：业务数据完全保留，审计数据与标记彻底剥离，且原活文档不受影响
        sheetcore::audit::save_clean_copy(&clean_copy, &app2.wb).unwrap();
        let mut app_clean = SheetApp::new_with_context(&egui::Context::default(), None);
        app_clean.open_path(clean_copy.clone());
        wait_open(&mut app_clean);
        assert_eq!(app_clean.wb.sheets[0].value(0, 0), Value::Number(88888.0));
        assert_eq!(app_clean.wb.sheets[0].value(0, 2), Value::Number(42.0));
        assert!(app_clean.wb.audit.log.is_none());
        assert!(app_clean.wb.audit.stripped_marker.is_none());
        assert!(app_clean.audit_state.verification.is_none());
        assert!(app2.wb.audit.log.is_some(), "导出干净副本绝不能影响原活文档的审计状态");
    }

    #[test]
    fn review_182_parked_tab_load_initializes_baseline_and_records_first_edit() {
        let env = crate::test_env::TestEnv::new("audit-parked-load");
        let ledger = env.root().join("parked_ledger.xlsx");

        let kp = generate_keypair().unwrap();
        let alice = audit_store::current_author("王会计", &kp);
        let mut init_wb = Workbook::new_blank();
        init_wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let log1 = AuditLog::enable(&init_wb, alice, &kp, 1_700_000_000_000).unwrap();
        init_wb.audit.log = Some(Arc::new(log1));
        sheetcore::xlsx::save_atomic(&ledger, &init_wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "王会计".into();
        app.has_open_workbook = true;
        // 发起打开后立刻切到新建标签，使账本在后台 Parked 状态完成加载
        app.open_path(ledger.clone());
        app.open_in_new_tab(PathBuf::new());

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.poll_open();
            let loaded = matches!(
                app.tabs.first(),
                Some(crate::TabSlot::Parked(doc)) if doc.tasks.open_rx.is_none() && doc.wb.audit.log.is_some()
            );
            if loaded {
                break;
            }
            assert!(Instant::now() < deadline, "后台标签加载超时");
            std::thread::sleep(Duration::from_millis(10));
        }

        // 切回账本标签页，把 A1 从 100 改成 275 并保存
        app.switch_tab(0);
        assert!(app.audit_state.baseline.is_some(), "后台加载完成后必须建立审计基线");
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(275.0), formula: None });
        app.wb.dirty = true;
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.has_pending_save());

        let reopened = sheetcore::xlsx::load(&ledger).unwrap();
        let ver = verify_workbook_audit(&reopened).unwrap();
        assert_eq!(ver.severity, AuditSeverity::Verified, "{:?}", ver.issues);
        let rlog = reopened.audit.log.as_ref().unwrap();
        assert_eq!(rlog.head_seq(), 2);
        assert_eq!(rlog.commits[1].total_cell_changes, 1, "后台加载切回后首次编辑必须记录 1 格改动而非 0 格");
        assert_eq!(rlog.commits[1].cells[0].before, AuditSnap::Num(100.0));
        assert_eq!(rlog.commits[1].cells[0].after, AuditSnap::Num(275.0));
    }

    #[test]
    fn review_182_crash_recovery_marks_gap_without_baseline_and_diffs_with_baseline() {
        let env = crate::test_env::TestEnv::new("audit-crash-recovery");
        let orig_path = env.root().join("orig_ledger.xlsx");
        let missing_path = env.root().join("deleted_orig.xlsx");
        let save_a = env.root().join("recovered_no_base.xlsx");
        let save_b = env.root().join("recovered_with_base.xlsx");

        let kp = generate_keypair().unwrap();
        let alice = audit_store::current_author("财务甲", &kp);
        let mut orig_wb = Workbook::new_blank();
        orig_wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
        let log1 = AuditLog::enable(&orig_wb, alice, &kp, 1_700_000_000_000).unwrap();
        orig_wb.audit.log = Some(Arc::new(log1.clone()));
        sheetcore::xlsx::save_atomic(&orig_path, &orig_wb).unwrap();

        // 模拟崩溃草稿：正文 A1 已改为 20，内嵌审计日志仍停在 #1（A1=10）
        let mut draft_wb = orig_wb.clone();
        draft_wb.sheets[0].set(0, 0, Cell { value: Value::Number(20.0), formula: None });
        draft_wb.path = None;
        draft_wb.dirty = true;

        // 情况 A：原文件不可得 -> 另存必须记录恢复缺口与 HistoricalExternalGap，绝不签出“核验通过 + 0 格修改”
        let mut app_a = SheetApp::new_with_context(&egui::Context::default(), None);
        app_a.my_name = "财务甲".into();
        app_a.wb = draft_wb.clone();
        app_a.on_workbook_recovered_audit(Some(missing_path.to_string_lossy().as_ref()));
        assert!(app_a.audit_state.pending_external_gap.is_some());
        app_a.wb.path = Some(save_a.clone());
        app_a.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app_a.has_pending_save() && Instant::now() < deadline {
            app_a.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let reopened_a = sheetcore::xlsx::load(&save_a).unwrap();
        let ver_a = verify_workbook_audit(&reopened_a).unwrap();
        assert_eq!(ver_a.severity, AuditSeverity::Warning);
        assert!(ver_a.issues.iter().any(|i| i.kind == AuditIssueKind::HistoricalExternalGap));
        assert!(reopened_a.audit.log.as_ref().unwrap().commits[1]
            .bulk_ops
            .iter()
            .any(|b| b.kind == "恢复缺口"));

        // 情况 B：原文件在磁盘完好且匹配 #1 哈希 -> 自动恢复精确基线，另存记录 A1: 10 -> 20
        let mut app_b = SheetApp::new_with_context(&egui::Context::default(), None);
        app_b.my_name = "财务甲".into();
        app_b.wb = draft_wb;
        app_b.on_workbook_recovered_audit(Some(orig_path.to_string_lossy().as_ref()));
        assert!(app_b.audit_state.pending_external_gap.is_none());
        app_b.wb.path = Some(save_b.clone());
        app_b.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app_b.has_pending_save() && Instant::now() < deadline {
            app_b.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let reopened_b = sheetcore::xlsx::load(&save_b).unwrap();
        let ver_b = verify_workbook_audit(&reopened_b).unwrap();
        assert_eq!(ver_b.severity, AuditSeverity::Verified, "{:?}", ver_b.issues);
        let log_b = reopened_b.audit.log.as_ref().unwrap();
        assert_eq!(log_b.commits[1].total_cell_changes, 1);
        assert_eq!(log_b.commits[1].cells[0].before, AuditSnap::Num(10.0));
        assert_eq!(log_b.commits[1].cells[0].after, AuditSnap::Num(20.0));
    }

    #[test]
    fn review_182_insert_row_then_ctrl_z_restores_blame_and_clears_structural_log() {
        let env = crate::test_env::TestEnv::new("audit-insert-undo");
        let ledger = env.root().join("undo_row.xlsx");

        let mut init_wb = Workbook::new_blank();
        init_wb.sheets[0].set(0, 0, Cell { value: Value::Number(500.0), formula: None });
        sheetcore::xlsx::save_atomic(&ledger, &init_wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "李出纳".into();
        app.open_path(ledger.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }
        app.enable_audit_for_current_workbook();
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 在 A1 前插入 1 行，再立即 Ctrl+Z 撤销，然后保存
        app.apply_local_structural(0, sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
        assert_eq!(app.audit_state.pending_structural_ops.len(), 1);
        assert!(app.apply_undo(false), "Ctrl+Z 撤销插行应成功");
        assert!(app.audit_state.pending_structural_ops.is_empty(), "撤销插行后待提交结构列表必须清空");
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(500.0));

        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }

        let reopened = sheetcore::xlsx::load(&ledger).unwrap();
        let log = reopened.audit.log.as_ref().unwrap();
        assert_eq!(log.head_seq(), 1, "插行后撤销再保存不得产生幽灵「插入 1 行」提交");
        assert!(log.lookup_blame("Sheet1", 0, 0).is_some(), "A1 必须保留原 Blame 归属");
        assert!(log.lookup_blame("Sheet1", 1, 0).is_none(), "空白 A2 不得残留错位 Blame");
    }

    #[test]
    fn review_182_save_as_xls_warns_clears_audit_and_does_not_poison_watermark() {
        let env = crate::test_env::TestEnv::new("audit-save-xls");
        let xlsx_path = env.root().join("audited.xlsx");
        let xls_path = env.root().join("downgraded.xls");

        let mut init_wb = Workbook::new_blank();
        init_wb.sheets[0].set(0, 0, Cell { value: Value::Number(88.0), formula: None });
        sheetcore::xlsx::save_atomic(&xlsx_path, &init_wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "周经理".into();
        app.open_path(xlsx_path.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }
        app.enable_audit_for_current_workbook();
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 1. 兼容警告必须明确指出 .xls 将丢失审计链
        let warns = sheetcore::xls_write::compatibility_warnings(&app.wb);
        assert!(
            warns.iter().any(|w| w.contains("审计")),
            "另存为 .xls 兼容提示不能为空且须提醒审计链丢失: {warns:?}"
        );

        // 2. 真实另存为 .xls
        app.wb.path = Some(xls_path.clone());
        app.wb.dirty = true;
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.wb.audit.log.is_none(), "另存为 .xls 后内存工作簿不得继续残留启用态");
        assert!(app.audit_state.verification.is_none(), "另存为 .xls 后界面核验态必须清空");
        assert!(
            app.notice.as_ref().is_some_and(|n| n.tag == Some("audit_xls_loss")),
            "另存为 .xls 后必须弹出审计丢失警告横幅"
        );

        // 3. 同机重新打开该 .xls，绝不能误触发路径水印回滚告警
        let mut app2 = SheetApp::new_with_context(&egui::Context::default(), None);
        app2.open_path(xls_path.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app2.tasks.open_rx.is_some() && Instant::now() < deadline {
            app2.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app2.audit_state.verification.is_none(),
            "重开另存出的 .xls 不得命中路径水印回滚告警: {:?}",
            app2.audit_state.verification
        );
    }

    #[test]
    fn review_182_bulk_style_over_1024_and_concurrent_style_during_save_are_recorded() {
        let env = crate::test_env::TestEnv::new("audit-style-paths");
        let ledger = env.root().join("styles.xlsx");

        let mut init_wb = Workbook::new_blank();
        init_wb.sheets[0].set(0, 0, Cell { value: Value::Number(0.25), formula: None });
        sheetcore::xlsx::save_atomic(&ledger, &init_wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "孙会计".into();
        app.open_path(ledger.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }
        app.enable_audit_for_current_workbook();
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 路径 A：一次性给 A1:A1025（>1024 格，走 CoW 批量路径）设置百分比格式
        app.sel = (0, 0);
        app.sel_end = (1024, 0);
        app.apply_style(|st| st.format = "0.00%".into());
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let log2 = app.wb.audit.log.as_ref().unwrap();
        assert_eq!(log2.head_seq(), 2, "超过 1024 格的纯格式批量设置必须记入审计日志");
        assert!(log2.commits[1].bulk_ops.iter().any(|b| b.kind == "格式变更"));

        // 路径 B：修改金额并启动后台保存，在保存完成回执到达前设置 B1 百分比格式，随后再保存一次
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(0.5), formula: None });
        app.wb.dirty = true;
        app.save(false);
        // 在 poll_save 之前（模拟后台写盘飞行期间）追加新格式改动
        app.sel = (0, 1);
        app.sel_end = (0, 1);
        app.apply_style(|st| st.format = "0.00%".into());
        assert!(!app.audit_state.dirty_bulk_ops.is_empty());

        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !app.audit_state.dirty_bulk_ops.is_empty(),
            "上一轮保存回执到达时绝不能清空飞行期间新增的格式事件"
        );

        // 第二次保存必须把飞行期间新增的格式事件记为第 #4 版
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let log4 = app.wb.audit.log.as_ref().unwrap();
        assert_eq!(log4.head_seq(), 4, "并发新增的纯格式事件必须在下一次保存落账");
        assert!(log4.commits[3].bulk_ops.iter().any(|b| b.kind == "格式变更"));
    }

    #[test]
    fn review_182_collab_guest_style_and_peer_left_preserve_attestation() {
        let env = crate::test_env::TestEnv::new("audit-collab-regress");
        let ledger = env.root().join("collab_reg.xlsx");

        let host_kp = generate_keypair().unwrap();
        let guest_kp = generate_keypair().unwrap();
        let host_author = audit_store::current_author("宿主", &host_kp);
        let guest_author = audit_store::current_author("访客", &guest_kp);

        let mut host_wb = Workbook::new_blank();
        host_wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        let log1 = AuditLog::enable(&host_wb, host_author, &host_kp, 1_700_000_000_000).unwrap();
        host_wb.audit.log = Some(Arc::new(log1.clone()));
        sheetcore::xlsx::save_atomic(&ledger, &host_wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "宿主".into();
        app.audit_keypair = Some(host_kp.clone());
        app.open_path(ledger.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 1. 访客仅对已有单元格加粗（纯样式操作）
        app.audit_state.peer_identities.insert(2, guest_author.clone());
        app.audit_note_remote_bulk(2, 0, "格式变更", "格式变更（1 格）".into());
        app.wb.dirty = true;
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let log2 = app.wb.audit.log.as_ref().unwrap();
        assert_eq!(log2.head_seq(), 2, "访客纯格式修改必须产生审计提交");
        let bold_op = log2.commits[1]
            .bulk_ops
            .iter()
            .find(|b| b.kind == "格式变更")
            .expect("应包含访客格式变更摘要");
        assert_eq!(
            log2.author_of(bold_op.author_idx).unwrap().pubkey_hex,
            guest_kp.pubkey_hex
        );

        // 2. 访客编辑 B1=300 并提交独立联署，随后离线（移除 peer_identities 但保留 peer_attestations）
        app.audit_state.peer_identities.insert(2, guest_author.clone());
        let g_cells = vec![("Sheet1".to_string(), 0, 1, AuditSnap::Empty, AuditSnap::Num(300.0))];
        let g_dig = author_contribution_digest(&log2.doc_id, 2, &g_cells);
        let g_sig = guest_kp.sign_digest(&g_dig).unwrap();
        app.audit_state.dirty_cell_authors.insert(
            (app.wb.sheets[0].identity, 0, 1),
            guest_author.clone(),
        );
        app.audit_state.record_peer_attestation(
            2,
            PeerAttestationInput {
                author: guest_author.clone(),
                changes_digest: sheetcore::audit::hex_encode(&g_dig),
                sig_hex: g_sig,
                attested_by_peer: true,
            },
        );
        app.wb.sheets[0].set(0, 1, Cell { value: Value::Number(300.0), formula: None });
        app.wb.dirty = true;
        // 模拟访客正常离线（只移除在线身份表，不删已收到的联署证明）
        app.audit_state.peer_identities.remove(&2);

        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let log3 = app.wb.audit.log.as_ref().unwrap();
        assert_eq!(log3.head_seq(), 3);
        assert_eq!(log3.commits[2].co_signers.len(), 1);
        assert!(
            log3.commits[2].co_signers[0].attested_by_peer,
            "访客提交联署后离线，宿主保存时必须保留其有效独立联署"
        );
    }

    #[test]
    fn ux_default_on_new_blank_workbook_auto_enables_audit_on_first_save() {
        let env = crate::test_env::TestEnv::new("audit-default-on-blank");
        let target = env.root().join("brand_new.xlsx");

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "陈会计".into();
        assert!(
            app.audit_state.auto_enable_active(&app.wb),
            "新建空白工作簿默认应处于防篡改自动开启状态"
        );
        assert!(app.audit_state.baseline.is_some(), "新建空白工作簿应立即持有基线");

        // 未手动点击任何“开启审计”按钮，直接编辑 A1=123 并保存
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(123.0), formula: None });
        app.wb.path = Some(target.clone());
        app.wb.dirty = true;
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.has_pending_save());

        let reopened = sheetcore::xlsx::load(&target).unwrap();
        let ver = verify_workbook_audit(&reopened).unwrap();
        assert_eq!(ver.severity, AuditSeverity::Verified, "{:?}", ver.issues);
        let log = reopened.audit.log.as_ref().expect("新建工作簿首次保存必须自动内嵌防篡改审计链");
        assert_eq!(log.head_seq(), 2, "应自动写入创世基线 #1 + 首轮改动 #2");
        assert_eq!(log.commits[1].total_cell_changes, 1);
        assert_eq!(log.commits[1].cells[0].before, AuditSnap::Empty);
        assert_eq!(log.commits[1].cells[0].after, AuditSnap::Num(123.0));
        let blame = log.lookup_blame("Sheet1", 0, 0).expect("A1 应有 Blame 溯源");
        assert_eq!(blame.author.display_name, "陈会计");
        assert!(!blame.is_baseline);
    }

    #[test]
    fn ux_default_on_legacy_xlsx_untouched_on_read_and_auto_enables_on_edit_save() {
        let env = crate::test_env::TestEnv::new("audit-default-on-legacy");
        let legacy = env.root().join("legacy_book.xlsx");

        let mut raw_wb = Workbook::new_blank();
        raw_wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        raw_wb.sheets[0].set(0, 1, Cell { value: Value::Number(50.0), formula: None });
        sheetcore::xlsx::save_atomic(&legacy, &raw_wb).unwrap();
        let orig_bytes = std::fs::read(&legacy).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "林审核".into();
        app.open_path(legacy.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 纯只读打开：既不改写磁盘原文件，也不在内存中强行变脏，但处于默认自动开启就绪态
        assert!(app.wb.audit.log.is_none());
        assert!(!app.wb.dirty);
        assert!(app.audit_state.auto_enable_active(&app.wb));
        assert_eq!(std::fs::read(&legacy).unwrap(), orig_bytes, "只读查看旧文件绝不能改动原文件");

        // 首次编辑 A1: 100 -> 260 并保存：自动生成 #1 基线 + #2 精准净变更，未改动的 B1 标为基线登记
        app.wb.sheets[0].set(0, 0, Cell { value: Value::Number(260.0), formula: None });
        app.wb.dirty = true;
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }

        let reopened = sheetcore::xlsx::load(&legacy).unwrap();
        let ver = verify_workbook_audit(&reopened).unwrap();
        assert_eq!(ver.severity, AuditSeverity::Verified, "{:?}", ver.issues);
        let log = reopened.audit.log.as_ref().expect("旧文件首次编辑保存后应自动开启防篡改审计");
        assert_eq!(log.head_seq(), 2);
        assert_eq!(log.commits[1].total_cell_changes, 1);
        assert_eq!(log.commits[1].cells[0].before, AuditSnap::Num(100.0));
        assert_eq!(log.commits[1].cells[0].after, AuditSnap::Num(260.0));
        assert!(
            log.lookup_blame("Sheet1", 0, 1).unwrap().is_baseline,
            "旧文件中未编辑过的 B1 应标为基线登记人"
        );
        assert!(
            !log.lookup_blame("Sheet1", 0, 0).unwrap().is_baseline,
            "本次编辑过的 A1 应标为修改作者"
        );
    }

    #[test]
    fn ux_unified_rows_and_dynamic_user_dropdown_sorted_by_count_desc() {
        let env = crate::test_env::TestEnv::new("audit-unified-user-dropdown");
        let ledger = env.root().join("multi_user.xlsx");

        let kp_alice = generate_keypair().unwrap();
        let kp_bob = generate_keypair().unwrap();
        let kp_carol = generate_keypair().unwrap();
        let alice = audit_store::current_author("张三", &kp_alice);
        let bob = audit_store::current_author("李四", &kp_bob);
        let carol = audit_store::current_author("王五", &kp_carol);

        // 构造多用户真实签名审计链：张三改 1 格，李四改 3 格，王五改 2 格
        let mut wb = Workbook::new_blank();
        let mut log = AuditLog::enable(&wb, alice.clone(), &kp_alice, 1_700_000_000_000).unwrap();

        let base1 = wb.clone();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        let d2 = sheetcore::audit::compute_net_diff(&base1, &wb, &alice, &HashMap::new(), &[]);
        log.append_commit(&wb, alice.clone(), &kp_alice, "保存修改", &d2, &[], None, 1_700_000_010_000)
            .unwrap();

        let base2 = wb.clone();
        wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None });
        wb.sheets[0].set(2, 0, Cell { value: Value::Number(3.0), formula: None });
        wb.sheets[0].set(3, 0, Cell { value: Value::Number(4.0), formula: None });
        let d3 = sheetcore::audit::compute_net_diff(&base2, &wb, &bob, &HashMap::new(), &[]);
        log.append_commit(&wb, bob.clone(), &kp_bob, "保存修改", &d3, &[], None, 1_700_000_020_000)
            .unwrap();

        let base3 = wb.clone();
        wb.sheets[0].set(4, 0, Cell { value: Value::Number(5.0), formula: None });
        wb.sheets[0].set(5, 0, Cell { value: Value::Number(6.0), formula: None });
        let d4 = sheetcore::audit::compute_net_diff(&base3, &wb, &carol, &HashMap::new(), &[]);
        log.append_commit(&wb, carol.clone(), &kp_carol, "保存修改", &d4, &[], None, 1_700_000_030_000)
            .unwrap();

        wb.audit.log = Some(Arc::new(log));
        sheetcore::xlsx::save_atomic(&ledger, &wb).unwrap();

        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        app.my_name = "张三".into();
        app.open_path(ledger.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.tasks.open_rx.is_some() && Instant::now() < deadline {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(10));
        }

        // 1. 刚打开：只有 3 位编辑过的用户，下拉列表严格按记录数降序：李四(3) > 王五(2) > 张三(1)
        let rows = app.build_unified_audit_rows();
        assert_eq!(rows.len(), 6, "空创世 #1 不应计入修改行，总修改行应为 1+3+2=6");
        let counts = collect_user_filter_counts(&rows);
        assert_eq!(
            counts,
            vec![
                ("李四".to_string(), 3),
                ("王五".to_string(), 2),
                ("张三".to_string(), 1),
            ],
            "动态用户下拉框必须仅含修改过的用户且按记录数降序排列"
        );

        // 2. 张三继续在会话内未保存修改 3 格 -> 待保存行自动并入同一列表，张三跃升至第一：张三(4) > 李四(3) > 王五(2)
        for r in 10..13u32 {
            let new_cell = Cell { value: Value::Number(f64::from(r)), formula: None };
            app.rev.push(sheetcore::revision::Entry {
                ts: 1_700_000_040_000 + u64::from(r),
                who: "张三".into(),
                site: 0,
                remote: false,
                label: "输入".into(),
                sheet: "Sheet1".into(),
                cells: vec![sheetcore::revision::Change {
                    r,
                    c: 0,
                    before: sheetcore::revision::Snap::default(),
                    after: sheetcore::revision::Snap::from_cell(&new_cell),
                    before_style: None,
                    after_style: None,
                }],
                note: String::new(),
            });
            app.wb.sheets[0].set(r, 0, new_cell);
        }
        app.wb.dirty = true;

        let rows_pending = app.build_unified_audit_rows();
        assert_eq!(rows_pending.len(), 9);
        assert!(rows_pending[0].pending && rows_pending[0].revert_rev_idx.is_some());
        let counts_pending = collect_user_filter_counts(&rows_pending);
        assert_eq!(
            counts_pending,
            vec![
                ("张三".to_string(), 4),
                ("李四".to_string(), 3),
                ("王五".to_string(), 2),
            ]
        );

        // 3. 保存落盘后，待保存记录无缝转为已签名记录，不重复计数（仍为 4+3+2=9 条）
        app.save(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.has_pending_save() && Instant::now() < deadline {
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let rows_after = app.build_unified_audit_rows();
        assert_eq!(rows_after.len(), 9, "保存后待保存项应无缝转为已签名项且绝不重复");
        assert!(rows_after.iter().all(|r| !r.pending));
        assert_eq!(
            collect_user_filter_counts(&rows_after),
            vec![
                ("张三".to_string(), 4),
                ("李四".to_string(), 3),
                ("王五".to_string(), 2),
            ]
        );
    }
}
