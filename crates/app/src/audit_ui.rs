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

/// 单份文档标签页专属的审计运行态（与 `ParkedDoc` 一同停放/恢复）
#[derive(Clone, Default)]
pub(crate) struct DocAuditState {
    /// 上次已落盘（或打开时）的 CoW 工作簿基线，用于保存时 O(变化页数) 计算净差
    pub baseline: Option<Box<Workbook>>,
    /// 最近一次完整核验结果（哈希链 + 签名 + 状态根 + 本机水印 + TOFU）
    pub verification: Option<AuditVerification>,
    /// 打开时若检测到绕过审计的外部修改，暂存当时观察到的正文哈希，下次合规保存永久写入链中
    pub pending_external_gap: Option<String>,
    /// 本轮未保存期间每个单元格的真实修改者归属：(sheet_identity, row, col) -> AuditAuthor
    pub dirty_cell_authors: HashMap<(u64, u32, u32), AuditAuthor>,
    /// 本轮未保存期间的结构/样式/保护等摘要事件：(sheet_name, author, kind, summary)
    pub dirty_bulk_ops: Vec<(String, AuditAuthor, String, String)>,
    /// 本协作会话中已通过 ECDSA 验签的在线成员身份：site -> AuditAuthor
    pub peer_identities: HashMap<u32, AuditAuthor>,
    /// 本轮已收到的远端协作者独立联署证明：site -> PeerAttestationInput
    pub peer_attestations: HashMap<u32, PeerAttestationInput>,
    /// 已导入并通过 Merkle/签名验真的冷归档包（供在审计面板中展开查阅历史明细）
    pub verified_cold_archive: Option<(ColdArchiveVerifyResult, AuditColdArchive)>,
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
        self.audit_state = DocAuditState::default();
        self.audit_expanded_seq = None;
        self.audit_archive_msg = None;
        self.audit_state.baseline = Some(Box::new(self.wb.clone()));

        let my_pk = self
            .ensure_audit_keypair()
            .ok()
            .map(|k| k.pubkey_hex);
        let path = self.wb.path.clone();
        let ver = audit_store::verify_workbook_full(path.as_deref(), &self.wb, my_pk.as_deref());
        if let Some(ref v) = ver {
            if v.has_external_gap() {
                self.audit_state.pending_external_gap = Some(v.current_state_hash.clone());
            }
            self.rev_tab = 0;
            match v.severity {
                AuditSeverity::Tampered => {
                    let first_msg = v
                        .issues
                        .first()
                        .map(|i| i.message.clone())
                        .unwrap_or_else(|| "内嵌审计记录校验未通过".into());
                    self.set_notice(NoticeBanner {
                        level: NoticeLevel::Error,
                        title: "防篡改审计异常警告".into(),
                        detail: first_msg,
                        actions: vec![("查看审计记录".into(), NoticeAction::OpenAudit)],
                        tag: Some("audit_tamper"),
                        target_doc_uid: Some(self.doc_uid),
                    });
                }
                AuditSeverity::Warning => {
                    let first_msg = v
                        .issues
                        .first()
                        .map(|i| i.message.clone())
                        .unwrap_or_else(|| "存在审计提醒事项".into());
                    self.set_notice(NoticeBanner {
                        level: NoticeLevel::Warn,
                        title: "防篡改审计提醒".into(),
                        detail: first_msg,
                        actions: vec![("查看审计记录".into(), NoticeAction::OpenAudit)],
                        tag: Some("audit_warn"),
                        target_doc_uid: Some(self.doc_uid),
                    });
                }
                AuditSeverity::Verified => {}
            }
        }
        self.audit_state.verification = ver;
    }

    /// 打开「修订与审计记录」窗口并定位到审计页签
    pub(crate) fn open_audit_panel(&mut self) {
        self.rev_open = true;
        self.rev_tab = 0;
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

    /// 为当前工作簿启用防篡改审计记录（不可关闭，写入创世提交 #1）
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
                self.wb.audit.log = Some(arc.clone());
                self.wb.audit.stripped_marker = None;
                self.wb.audit.parse_error = None;
                self.wb.audit.strip_on_save = false;
                self.wb.dirty = true;
                self.audit_state.baseline = Some(Box::new(self.wb.clone()));
                self.audit_state.dirty_cell_authors.clear();
                self.audit_state.dirty_bulk_ops.clear();
                self.audit_state.pending_external_gap = None;
                self.audit_state.verification = Some(arc.verify_against_workbook(&self.wb));
                if let Some(net) = self.net.as_ref().filter(|n| n.hosting) {
                    net.broadcast_audit_sync(&arc);
                }
                self.status = "已开启防篡改审计记录（创世基线 #1 已生成，请按 Ctrl+S 保存生效）".into();
                self.rev_open = true;
                self.rev_tab = 0;
            }
            Err(e) => {
                self.status = format!("开启防篡改审计失败：{e:#}");
            }
        }
    }

    /// 本地单元格编辑归属登记（零开销：未开启审计时直接返回）
    pub(crate) fn audit_note_local_deltas(&mut self, label: &str, deltas: &[sheetcore::undo::CellDelta]) {
        if self.wb.audit.log.is_none() || deltas.is_empty() {
            return;
        }
        let Some(author) = self.my_audit_author() else {
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
            self.audit_state.dirty_bulk_ops.push((
                sheet_name,
                author,
                "格式变更".into(),
                format!("{label} {range}（{} 格）", deltas.len()),
            ));
        } else if self.net.as_ref().is_some_and(|n| !n.hosting) {
            // 协作访客每次完成单元格编辑后立即刷新独立 ECDSA 联署，确保无论访客还是宿主保存均带有效联署
            self.guest_send_audit_attest_before_save();
        }
    }

    /// 远端协作单元格编辑归属登记
    pub(crate) fn audit_note_remote_ops(&mut self, ops: &[sheetcore::sync::Op]) {
        if self.wb.audit.log.is_none() || ops.is_empty() {
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

    /// 远端或本地结构变动（插删行列）同步平移 Blame 与未落盘单元格归属坐标
    pub(crate) fn audit_note_structural(
        &mut self,
        site: Option<u32>,
        sheet_idx: usize,
        op: &sheetcore::structural::Structural,
    ) {
        if self.wb.audit.log.is_none() {
            return;
        }
        let Some(sh) = self.wb.sheets.get(sheet_idx) else {
            return;
        };
        let sheet_id = sh.identity;
        let sheet_name = sh.name.clone();
        let author = match site {
            Some(s) => self.audit_author_for_site(s),
            None => self.my_audit_author().unwrap_or_else(|| AuditAuthor {
                display_name: self.my_name.clone(),
                ..Default::default()
            }),
        };
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
        // 同步平移 Blame 坐标
        if let Some(log_arc) = self.wb.audit.log.as_mut() {
            Arc::make_mut(log_arc).apply_structural_shift(&sheet_name, op);
        }
        self.audit_state.dirty_bulk_ops.push((
            sheet_name,
            author,
            "结构变更".into(),
            op.label(),
        ));
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

    /// 在发起保存前组装防篡改审计提交上下文（同时支持活动标签页与后台停放标签页）
    pub(crate) fn prepare_audit_save_ctx_for(
        wb: &Workbook,
        audit_state: &DocAuditState,
        tasks: &mut DocumentTasks,
        net: Option<&net::Net>,
        my_name: &str,
        keypair_cache: &mut Option<sheetcore::audit::AuditKeyPair>,
    ) {
        let Some(log) = wb.audit.log.as_ref() else {
            tasks.next_audit_ctx = None;
            return;
        };
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
        let has_other_authors = audit_state
            .dirty_cell_authors
            .values()
            .any(|a| !a.pubkey_hex.eq_ignore_ascii_case(&committer.pubkey_hex));
        let action = if net.is_some() && (has_other_authors || !audit_state.peer_attestations.is_empty()) {
            "协作保存".to_string()
        } else if log.head_seq() == 1 && audit_state.dirty_cell_authors.is_empty() && audit_state.dirty_bulk_ops.is_empty() {
            "启用防篡改审计".to_string()
        } else {
            "保存修改".to_string()
        };
        let baseline = audit_state
            .baseline
            .clone()
            .unwrap_or_else(|| Box::new(wb.clone()));
        let co_signers: Vec<PeerAttestationInput> = audit_state
            .peer_attestations
            .values()
            .cloned()
            .collect();

        tasks.next_audit_ctx = Some(AuditSaveContext {
            baseline,
            committer,
            keypair: kp,
            action,
            cell_attributions: audit_state.dirty_cell_authors.clone(),
            bulk_ops: audit_state.dirty_bulk_ops.clone(),
            co_signers,
            external_gap_hash: audit_state.pending_external_gap.clone(),
        });
    }

    /// 后台保存成功回包后推进基线、刷新本机防回滚水印，并在宿主侧广播 `AuditSync` 给全体协作者
    pub(crate) fn on_audit_save_completed_for(
        wb: &Workbook,
        audit_state: &mut DocAuditState,
        tasks: &mut DocumentTasks,
        net: Option<&net::Net>,
        saved_path: &Path,
        my_pubkey: Option<&str>,
    ) -> Option<String> {
        let mut extra_note = None;
        if let Some(new_base) = tasks.last_audit_saved_baseline.take() {
            audit_state.baseline = Some(new_base);
        } else if wb.audit.log.is_some() && !wb.dirty {
            audit_state.baseline = Some(Box::new(wb.clone()));
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
        audit_state.dirty_bulk_ops.clear();
        audit_state.peer_attestations.clear();
        audit_state.pending_external_gap = None;

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
        extra_note
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
        self.wb.audit.log = Some(arc.clone());
        self.wb.audit.stripped_marker = None;
        self.wb.audit.parse_error = None;
        self.audit_state.baseline = Some(Box::new(self.wb.clone()));
        self.audit_state.dirty_cell_authors.clear();
        self.audit_state.dirty_bulk_ops.clear();
        audit_store::record_verified_watermark(self.wb.path.as_deref(), &arc);
        self.audit_state.verification =
            audit_store::verify_workbook_full(self.wb.path.as_deref(), &self.wb, my_pk.as_deref());
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
    // UI 渲染：状态栏徽章、审计主面板、启用确认弹窗、单元格 Blame 溯源弹窗
    // -----------------------------------------------------------------------

    /// 底部状态栏右侧防篡改审计状态徽章（点击直达审计面板）
    pub(crate) fn draw_audit_status_pill(&mut self, ui: &mut egui::Ui) {
        let Some(ver) = self.audit_state.verification.as_ref() else {
            return;
        };
        let (label, fg, bg, tip) = match ver.severity {
            AuditSeverity::Verified => (
                format!("审计完整 #{}", ver.head_seq),
                Color32::from_rgb(16, 124, 65),
                Color32::from_rgba_unmultiplied(16, 124, 65, 22),
                format!(
                    "防篡改审计链完整（共 {} 版，{} KB）· 点击查看详情",
                    ver.head_seq,
                    ver.embedded_bytes / 1024
                ),
            ),
            AuditSeverity::Warning => (
                format!("审计提醒 #{}", ver.head_seq),
                Color32::from_rgb(168, 108, 12),
                Color32::from_rgba_unmultiplied(196, 130, 14, 26),
                format!("存在 {} 项审计提醒（如时钟异常或身份指纹变更）· 点击查看", ver.issues.len()),
            ),
            AuditSeverity::Tampered => (
                "⚠ 审计异常".to_string(),
                Color32::from_rgb(196, 43, 28),
                Color32::from_rgba_unmultiplied(196, 43, 28, 28),
                format!("检测到 {} 项防篡改高危异常（外部修改/篡改/回退）· 立即点击查看", ver.issues.len()),
            ),
        };
        ui.add_space(6.0);
        let btn = egui::Button::new(RichText::new(label).size(11.0).strong().color(fg))
            .fill(bg)
            .stroke(egui::Stroke::new(1.0_f32, fg.gamma_multiply(0.45)))
            .corner_radius(4.0);
        if ui.add(btn).on_hover_text(tip).clicked() {
            self.open_audit_panel();
        }
    }

    /// Backstage「文件 → 信息」中的防篡改审计状态卡片
    pub(crate) fn draw_backstage_audit_section(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("不可篡改编辑记录（防篡改审计）").strong().size(13.0));
        ui.add_space(4.0);
        if let Some(ver) = self.audit_state.verification.clone() {
            let (badge, col) = match ver.severity {
                AuditSeverity::Verified => ("已开启 · 密码学核验完整", Color32::from_rgb(16, 124, 65)),
                AuditSeverity::Warning => ("已开启 · 存在提醒事项", Color32::from_rgb(168, 108, 12)),
                AuditSeverity::Tampered => ("⚠ 检测到疑似篡改 / 外部修改 / 回退", Color32::from_rgb(196, 43, 28)),
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
                if ui_kit::primary_button(ui, "查看防篡改审计记录").clicked() {
                    self.open_audit_panel();
                    *close = true;
                }
                if ui_kit::secondary_button(ui, "导出对外干净副本 (.xlsx)…").clicked() {
                    *close = true;
                    self.export_clean_copy_dialog();
                }
            });
        } else {
            ui.label(
                RichText::new("未开启：开启后将随 .xlsx 文件内嵌经 ECDSA P-256 签名的哈希链编辑记录与单元格溯源，所有查看文件的人均可核验。")
                    .size(11.5)
                    .weak(),
            );
            ui.add_space(4.0);
            if ui_kit::secondary_button(ui, "开启防篡改审计记录…").clicked() {
                self.audit_enable_confirm = true;
                *close = true;
            }
        }
    }

    /// 修订记录窗口内的「不可篡改审计记录」主标签页
    pub(crate) fn draw_audit_tab(
        &mut self,
        ui: &mut egui::Ui,
        jump: &mut Option<(String, u32, u32)>,
    ) {
        let mut do_enable_prompt = false;
        let mut do_export_csv = false;
        let mut do_export_clean = false;
        let mut do_export_archive = false;
        let mut do_verify_archive = false;
        let mut do_reverify = false;

        let log_opt = self.wb.audit.log.clone();
        let ver_opt = self.audit_state.verification.clone();

        if log_opt.is_none() && ver_opt.is_none() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("此工作簿尚未开启「不可篡改审计记录」").strong().size(15.0));
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "• 随 .xlsx 文件内嵌保存，无需任何服务器，所有查看文件的人都能直接看到并验真\n\
                         • 按保存合并净变更 + 紧凑压缩 + 自动滚动归档（体积硬上限 512 KB，绝不会无限膨胀）\n\
                         • ECDSA P-256 签名 + 哈希链 + 正文状态根 + 本地防回滚水印：改日志、Excel 偷改数、旧备份覆盖必露馅\n\
                         • 对外发给客户时可使用「导出干净副本」一键脱敏，绝不泄露内部修改史",
                    )
                    .size(12.0)
                    .weak(),
                );
                ui.add_space(14.0);
                if ui_kit::primary_button(ui, "为此工作簿开启防篡改审计记录…").clicked() {
                    do_enable_prompt = true;
                }
            });
            ui.add_space(16.0);
            if do_enable_prompt {
                self.audit_enable_confirm = true;
            }
            return;
        }

        // 1. 顶部核验结果横幅
        if let Some(ref ver) = ver_opt {
            let (title, fg, bg) = match ver.severity {
                AuditSeverity::Verified => (
                    format!("✓ 密码学核验完整（共 #{} 版）", ver.head_seq),
                    Color32::from_rgb(16, 124, 65),
                    Color32::from_rgba_unmultiplied(16, 124, 65, 16),
                ),
                AuditSeverity::Warning => (
                    format!("⚠ 核验通过但有 {} 项提醒（共 #{} 版）", ver.issues.len(), ver.head_seq),
                    Color32::from_rgb(168, 108, 12),
                    Color32::from_rgba_unmultiplied(196, 130, 14, 20),
                ),
                AuditSeverity::Tampered => (
                    format!("✕ 检测到 {} 项高危审计异常！", ver.issues.len()),
                    Color32::from_rgb(196, 43, 28),
                    Color32::from_rgba_unmultiplied(196, 43, 28, 22),
                ),
            };
            egui::Frame::NONE
                .fill(bg)
                .stroke(egui::Stroke::new(1.0_f32, fg.gamma_multiply(0.4)))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).strong().size(12.5).color(fg));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("重新核验").clicked() {
                                do_reverify = true;
                            }
                            if let Some(ref log) = log_opt {
                                ui.label(
                                    RichText::new(format!(
                                        "ID:{} · {:.1}/{}KB · Blame {}格",
                                        &log.doc_id[..log.doc_id.len().min(8)],
                                        ver.embedded_bytes as f64 / 1024.0,
                                        log.max_embedded_bytes / 1024,
                                        ver.blame_count
                                    ))
                                    .size(10.5)
                                    .weak(),
                                );
                            }
                        });
                    });
                    if !ver.issues.is_empty() {
                        ui.add_space(4.0);
                        for issue in &ver.issues {
                            let icol = match issue.severity {
                                AuditSeverity::Tampered => Color32::from_rgb(196, 43, 28),
                                AuditSeverity::Warning => Color32::from_rgb(168, 108, 12),
                                AuditSeverity::Verified => Color32::from_rgb(16, 124, 65),
                            };
                            ui.label(RichText::new(format!("• {}", issue.message)).size(11.5).color(icol));
                        }
                    }
                    if self.wb.dirty {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("提示：当前有尚未保存的本地改动，按 Ctrl+S 保存后将自动签名追加新提交。")
                                .size(11.0)
                                .weak(),
                        );
                    }
                });
        }

        ui.add_space(6.0);

        // 2. 操作工具栏
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.rev_filter_who)
                    .hint_text("按作者/设备筛选")
                    .desired_width(115.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.rev_filter_kw)
                    .hint_text("按表/单元格/内容筛选")
                    .desired_width(145.0),
            );
            if log_opt.is_some() {
                if ui.small_button("导出审计 CSV…").clicked() {
                    do_export_csv = true;
                }
                if ui.small_button("导出对外干净副本…").on_hover_text("生成不含审计记录的脱敏 .xlsx，用于发给外部客户/供应商").clicked() {
                    do_export_clean = true;
                }
                if ui.small_button("验证冷归档包…").on_hover_text("导入历史折叠导出的 .cellan-audit 归档包并核验 Merkle 根").clicked() {
                    do_verify_archive = true;
                }
                if log_opt.as_ref().is_some_and(|l| l.last_compacted.is_some())
                    && ui.small_button("导出刚折叠冷归档…").clicked()
                {
                    do_export_archive = true;
                }
            }
        });

        if let Some((ok, ref msg)) = self.audit_archive_msg {
            ui.add_space(4.0);
            ui.label(
                RichText::new(msg).size(11.5).color(if ok {
                    Color32::from_rgb(16, 124, 65)
                } else {
                    Color32::from_rgb(196, 43, 28)
                }),
            );
        }

        ui.add_space(4.0);
        ui.separator();

        // 3. 提交链与归档节点时间线
        let Some(log) = log_opt else {
            if do_reverify {
                self.refresh_audit_verification_now();
            }
            return;
        };

        let who_q = self.rev_filter_who.trim().to_lowercase();
        let kw_q = self.rev_filter_kw.trim().to_lowercase();
        let my_pk = self.my_audit_pubkey().map(|s| s.to_string());

        egui::ScrollArea::vertical()
            .id_salt("audit_commit_scroll")
            .max_height(370.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // 最新提交在前
                for commit in log.commits.iter().rev() {
                    let committer = log.author_of(commit.committer_idx);
                    let who_label = committer
                        .map(|a| a.short_label())
                        .unwrap_or_else(|| "未知作者".into());
                    if !who_q.is_empty() {
                        let hit_who = who_label.to_lowercase().contains(&who_q)
                            || commit.co_signers.iter().any(|co| {
                                log.author_of(co.author_idx)
                                    .is_some_and(|a| a.short_label().to_lowercase().contains(&who_q))
                            });
                        if !hit_who {
                            continue;
                        }
                    }
                    if !kw_q.is_empty() {
                        let hit_kw = commit.action.to_lowercase().contains(&kw_q)
                            || format!("#{}", commit.seq).contains(&kw_q)
                            || commit.cells.iter().any(|ch| {
                                let sh = log.sheet_of(ch.sheet_idx).to_lowercase();
                                let pos = sheetcore::model::cell_name(ch.row, ch.col).to_lowercase();
                                sh.contains(&kw_q)
                                    || pos.contains(&kw_q)
                                    || ch.before.summary().to_lowercase().contains(&kw_q)
                                    || ch.after.summary().to_lowercase().contains(&kw_q)
                            })
                            || commit.bulk_ops.iter().any(|b| {
                                b.summary.to_lowercase().contains(&kw_q)
                                    || b.kind.to_lowercase().contains(&kw_q)
                            });
                        if !hit_kw {
                            continue;
                        }
                    }

                    let expanded = self.audit_expanded_seq == Some(commit.seq);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("#{}", commit.seq))
                                .strong()
                                .size(12.0)
                                .color(ui_kit::ACCENT),
                        );
                        ui.label(
                            RichText::new(sheetcore::audit::format_local_ms(commit.ts_ms))
                                .size(11.0)
                                .weak(),
                        );
                        ui.label(RichText::new(&who_label).strong().size(12.0));
                        if let Some(au) = committer {
                            let badge = audit_store::trust_badge_for_author(au, my_pk.as_deref());
                            let (bt, bc) = match badge {
                                AuthorTrustBadge::SelfDevice => ("本机", Color32::from_rgb(16, 124, 65)),
                                AuthorTrustBadge::KnownTrusted => ("已信任", Color32::from_rgb(16, 124, 65)),
                                AuthorTrustBadge::FirstSeen => ("首次见", Color32::from_rgb(90, 96, 106)),
                                AuthorTrustBadge::KeyChanged => ("⚠密钥变化", Color32::from_rgb(196, 43, 28)),
                            };
                            ui.label(
                                RichText::new(format!("[{bt} · {}]", &au.fingerprint()[..9]))
                                    .size(10.5)
                                    .color(bc),
                            );
                        }
                        ui.label(RichText::new(&commit.action).size(11.5));
                        if !commit.co_signers.is_empty() {
                            let attested = commit.co_signers.iter().filter(|c| c.attested_by_peer).count();
                            ui.label(
                                RichText::new(format!(
                                    "联署 {attested}/{}",
                                    commit.co_signers.len()
                                ))
                                .size(10.5)
                                .color(Color32::from_rgb(16, 124, 65)),
                            );
                        }
                        if commit.external_gap_hash.is_some() {
                            ui.label(
                                RichText::new("⚠含外部修改缺口")
                                    .size(10.5)
                                    .color(Color32::from_rgb(168, 108, 12)),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(if expanded { "收起" } else { "详情" })
                                .clicked()
                            {
                                self.audit_expanded_seq = if expanded { None } else { Some(commit.seq) };
                            }
                            let count_txt = if commit.total_cell_changes > 0 {
                                format!("{} 格改动", commit.total_cell_changes)
                            } else if !commit.bulk_ops.is_empty() {
                                commit.bulk_ops[0].summary.clone()
                            } else {
                                "基线快照".into()
                            };
                            ui.label(RichText::new(count_txt).size(11.0).weak());
                        });
                    });

                    // 默认展示前 2 条细胞级改动预览，展开后显示全部明细与签名哈希
                    let preview_limit = if expanded { commit.cells.len() } else { 2 };
                    for ch in commit.cells.iter().take(preview_limit) {
                        let sh = log.sheet_of(ch.sheet_idx);
                        let pos = format!("{sh}!{}", sheetcore::model::cell_name(ch.row, ch.col));
                        let ch_who = log
                            .author_of(ch.author_idx)
                            .map(|a| a.display_name.as_str())
                            .unwrap_or("");
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            if ui.link(&pos).clicked() {
                                *jump = Some((sh.to_string(), ch.row, ch.col));
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{} → {}",
                                    ch.before.summary(),
                                    ch.after.summary()
                                ))
                                .size(11.5),
                            );
                            if ch.author_idx != commit.committer_idx && !ch_who.is_empty() {
                                ui.label(
                                    RichText::new(format!("（由 {ch_who} 修改）"))
                                        .size(10.5)
                                        .weak(),
                                );
                            }
                        });
                    }
                    if !expanded && commit.cells.len() > 2 {
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.label(
                                RichText::new(format!(
                                    "…其余 {} 条明细，点击右侧「详情」展开",
                                    commit.cells.len() - 2
                                ))
                                .size(10.5)
                                .weak(),
                            );
                        });
                    }
                    if expanded {
                        for bo in &commit.bulk_ops {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(
                                    RichText::new(format!(
                                        "[{}] {}：{}",
                                        bo.kind,
                                        log.sheet_of(bo.sheet_idx),
                                        bo.summary
                                    ))
                                    .size(11.0)
                                    .weak(),
                                );
                            });
                        }
                        for co in &commit.co_signers {
                            let name = log
                                .author_of(co.author_idx)
                                .map(|a| a.short_label())
                                .unwrap_or_else(|| "未知".into());
                            let tag = if co.attested_by_peer {
                                "✓ 协作者私钥独立联署已验签"
                            } else {
                                "△ 宿主代签"
                            };
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(
                                    RichText::new(format!("协作参与：{name}（{} 格 · {tag}）", co.change_count))
                                        .size(10.5)
                                        .weak(),
                                );
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.label(
                                RichText::new(format!(
                                    "提交哈希：{}…  |  状态根：{}…",
                                    &commit.commit_hash[..commit.commit_hash.len().min(16)],
                                    &commit.state_hash[..commit.state_hash.len().min(16)],
                                ))
                                .size(10.0)
                                .weak(),
                            );
                        });
                    }
                    ui.separator();
                }

                // 历史滚动折叠归档节点
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
            });

        if do_reverify {
            self.refresh_audit_verification_now();
        }
        if do_export_csv {
            if let Some(dest) = rfd::FileDialog::new()
                .set_file_name("防篡改审计记录.csv")
                .add_filter("CSV 表格", &["csv"])
                .save_file()
            {
                let csv = log.export_csv();
                match std::fs::write(&dest, csv.as_bytes()) {
                    Ok(_) => {
                        self.audit_archive_msg =
                            Some((true, format!("已导出防篡改审计报告至 {}", dest.display())));
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

    /// 渲染审计相关弹窗：启用确认弹窗 + 单元格最后修改溯源（Blame）弹窗
    pub(crate) fn draw_audit_dialogs(&mut self, ctx: &egui::Context) {
        // 1. 启用防篡改审计确认弹窗
        if self.audit_enable_confirm {
            let mut open = true;
            let mut confirm = false;
            let mut cancel = false;
            ui_kit::window(ctx, "开启不可篡改编辑记录", 440.0)
                .open(&mut open)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("开启后，本工作簿将启用去中心化防篡改审计：")
                            .strong()
                            .size(13.0),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        "1. 每次保存会自动计算净变更、用本机 ECDSA P-256 私钥签名并链式内嵌进 .xlsx 文件；\n\
                         2. 开启后不可在原文件上关闭，以防止编辑者先关闭再偷偷改数；\n\
                         3. 若有人绕过 CELLAN 用 Excel/WPS 修改或拿旧备份覆盖，所有查看者打开都会立刻收到异常警告；\n\
                         4. 体积自动受控（硬上限 512 KB，超限自动滚动折叠归档不断链）；\n\
                         5. 如需发给外部客户/供应商，请使用「文件 → 导出对外干净副本」脱敏发送。",
                    );
                    ui.add_space(10.0);
                    ui_kit::footer(ui, |ui| {
                        if ui_kit::primary_button(ui, "确认开启").clicked() {
                            confirm = true;
                        }
                        if ui_kit::secondary_button(ui, "取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                self.audit_enable_confirm = false;
                self.enable_audit_for_current_workbook();
            } else if cancel || !open {
                self.audit_enable_confirm = false;
            }
        }

        // 2. 单元格右键「查看修改溯源 (Blame)」弹窗
        if let Some((sheet_name, row, col)) = self.audit_blame_popup.clone() {
            let mut open = true;
            let mut open_full_audit = false;
            let mut enable_now = false;
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
                        if let Some(blame) = log.lookup_blame(&sheet_name, row, col) {
                            let badge = audit_store::trust_badge_for_author(&blame.author, my_pk.as_deref());
                            let (bt, bc) = match badge {
                                AuthorTrustBadge::SelfDevice => ("本机签名", Color32::from_rgb(16, 124, 65)),
                                AuthorTrustBadge::KnownTrusted => ("已信任设备", Color32::from_rgb(16, 124, 65)),
                                AuthorTrustBadge::FirstSeen => ("首次见设备", Color32::from_rgb(90, 96, 106)),
                                AuthorTrustBadge::KeyChanged => ("⚠ 身份指纹变化", Color32::from_rgb(196, 43, 28)),
                            };
                            egui::Frame::NONE
                                .fill(Color32::from_rgba_unmultiplied(16, 124, 65, 14))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("最后修改人（持久 Blame 溯源）").strong().size(12.0));
                                    ui.label(format!(
                                        "作者：{}（Windows 账户：{} · 设备：{}）",
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
                        let history = log.cell_history(&sheet_name, row, col);
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
                            if ui_kit::primary_button(ui, "查看整本审计记录").clicked() {
                                open_full_audit = true;
                            }
                            if ui_kit::secondary_button(ui, "关闭").clicked() {
                                open = false;
                            }
                        });
                    } else {
                        ui.label("当前工作簿尚未开启「不可篡改审计记录」。开启后每次保存将自动记录每个单元格的修改人与完整变更史。");
                        ui.add_space(8.0);
                        ui_kit::footer(ui, |ui| {
                            if ui_kit::primary_button(ui, "立即开启防篡改审计…").clicked() {
                                enable_now = true;
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
            } else if enable_now {
                self.audit_blame_popup = None;
                self.audit_enable_confirm = true;
            } else if !open {
                self.audit_blame_popup = None;
            }
        }
    }
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
        host_audit_state.peer_attestations.insert(
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
}
