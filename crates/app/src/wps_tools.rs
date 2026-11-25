//! WPS 高频效率工具与核心交互实现：
//! - 4 向智能填充（公式 `$` 保持、日期、中文星期/月份/季度、`第N组`/`A-001`）+ 自动填充选项切换
//! - 单元格/行/列自适应、按值筛选、聚光灯模式、拆分窗格
//! - 数据工具：高亮重复项、拒绝录入重复项、合并相同单元格
//! - 快捷填充日期/时间 (`Ctrl+;` / `Ctrl+Shift+;`) 与粘贴选项即时切换

use eframe::egui;
use sheetcore::cell_name;
use sheetcore::model::{Cell, Value};

use crate::fill_seq::{self, FillMode};
use crate::{LastFillState, LastPasteState, SheetApp};

fn is_date_style_format(fmt: &str) -> bool {
    let lower = fmt.to_ascii_lowercase();
    lower.contains("yy") || lower.contains("dd") || (lower.contains('m') && (lower.contains('/') || lower.contains('-') || fmt.contains('月')))
}

fn col_name_abs(c: u32) -> String {
    format!("${}", sheetcore::col_label(c))
}

impl SheetApp {
    /// 记录最近颜色（去重，最多保留 8 个）
    pub(crate) fn push_recent_color(&mut self, rgb: [u8; 3]) {
        self.recent_colors.retain(|x| *x != rgb);
        self.recent_colors.insert(0, rgb);
        self.recent_colors.truncate(8);
    }

    /// 立即套用填充颜色并记忆
    pub(crate) fn apply_fill_color(&mut self, rgb: Option<[u8; 3]>) {
        if let Some(c) = rgb {
            self.last_fill_color = c;
            self.push_recent_color(c);
        }
        self.apply_style(move |s| s.fill = rgb);
        self.set_repeat_action(crate::RepeatAction::FillColor(rgb));
    }

    /// 立即套用字体颜色并记忆
    pub(crate) fn apply_font_color(&mut self, rgb: [u8; 3]) {
        self.last_font_color = rgb;
        self.push_recent_color(rgb);
        self.apply_style(move |s| s.color = rgb);
        self.set_repeat_action(crate::RepeatAction::FontColor(rgb));
    }

    /// 聚光灯（阅读模式）开关，持久化到 UiPrefs
    pub(crate) fn toggle_spotlight(&mut self) {
        self.prefs.spotlight = !self.prefs.spotlight;
        self.prefs.save();
        self.status = if self.prefs.spotlight {
            "已开启聚光灯（阅读模式）".into()
        } else {
            "已关闭聚光灯".into()
        };
    }

    /// 拆分窗格开关：与冻结窗格互斥
    pub(crate) fn toggle_split_panes(&mut self) {
        if self.split_panes.is_some() {
            self.split_panes = None;
            self.split_scroll = egui::Vec2::ZERO;
            self.status = "已取消拆分窗格".into();
        } else {
            let (mut r, mut c) = self.sel;
            if r == 0 && c == 0 {
                r = 8;
                c = 4;
            }
            if self.wb.sheet(self.sheet_idx).freeze.is_some() {
                let sh = self.wb.sheet_mut(self.sheet_idx);
                sh.freeze = None;
                sh.freeze_dirty = true;
            }
            self.split_panes = Some((r, c));
            self.split_scroll = egui::Vec2::ZERO;
            self.status = format!("已在 {} 拆分窗格（可拖动分隔条调整）", cell_name(r, c));
        }
    }

    /// Ctrl+; 插入当前日期；Ctrl+Shift+; 插入当前时间
    pub(crate) fn insert_current_datetime(&mut self, is_time: bool) {
        if self.edit_blocked() {
            return;
        }
        if self.editing.is_some() {
            self.commit_edit(None);
        }
        let now = chrono_free_now(is_time);
        let (r, c) = self.sel;
        let keys = vec![(r, c)];
        if !self.write_preflight(&keys, if is_time { "插入时间" } else { "插入日期" }) {
            return;
        }
        let txn = self.txn_begin(&keys);
        let mut cell = Cell::default();
        if is_time {
            cell.value = Value::Number(now.serial);
            self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
            let sid = self.wb.sheet(self.sheet_idx).style_of(r, c);
            let mut st = self.wb.styles.get(sid).clone();
            if st.format.is_empty() {
                st.format = "hh:mm".into();
                let nsid = self.wb.styles.intern(st);
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, nsid);
            }
        } else {
            cell.value = Value::Number(now.serial);
            self.wb.sheet_mut(self.sheet_idx).set(r, c, cell);
            let sid = self.wb.sheet(self.sheet_idx).style_of(r, c);
            let mut st = self.wb.styles.get(sid).clone();
            if st.format.is_empty() {
                st.format = "yyyy-mm-dd".into();
                let nsid = self.wb.styles.intern(st);
                self.wb.sheet_mut(self.sheet_idx).set_style(r, c, nsid);
            }
        }
        self.txn_end(if is_time { "插入当前时间" } else { "插入当前日期" }, txn);
        self.recalc_and_broadcast(&[(self.sheet_idx, r, c)]);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = if is_time { "已插入当前时间" } else { "已插入当前日期" }.into();
    }

    /// WPS：按所选单元格的值筛选
    pub(crate) fn filter_by_cell_value(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let (r, c) = self.sel;
        let val_text = {
            let sh = self.wb.sheet(self.sheet_idx);
            let cell = sh.get(r, c);
            match cell {
                Some(cell) => {
                    let st = self.wb.styles.get(sh.style_of(r, c));
                    sheetcore::style::format_value_in(self.wb.date_system, &cell.value, &st.format)
                }
                None => String::new(),
            }
        };
        let sh = self.wb.sheet_mut(self.sheet_idx);
        if sh.filter.is_none() {
            let reg = Self::current_region_fast(sh, r, c);
            if reg.2 <= reg.0 {
                self.status = "数据区域太小，无法按值筛选".into();
                return;
            }
            sh.filter = Some(sheetcore::sort_filter::Filter::new(reg.0, reg.1, reg.2, reg.3));
        }
        if let Some(f) = self.wb.sheet_mut(self.sheet_idx).filter.as_mut() {
            if c >= f.range.1 && c <= f.range.3 {
                f.criteria.remove(&c);
                let mut set = std::collections::HashSet::new();
                set.insert(val_text.clone());
                f.allows.insert(c, set);
            }
        }
        self.recalc_filter_hidden();
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        let disp = if val_text.is_empty() { "(空白)" } else { val_text.as_str() };
        self.status = format!("已按所选值 \"{disp}\" 筛选");
    }

    /// 稀疏页感知的「当前区域」探测：直接委托底层 `Store` 页级 `row_mask` / `col_nonempty` 位图实现，
    /// 无需逐格探测或遍历非空单元格即可在微秒级跳过百万空行。
    #[inline]
    pub(crate) fn current_region_fast(
        sheet: &sheetcore::Sheet,
        r: u32,
        c: u32,
    ) -> (u32, u32, u32, u32) {
        sheetcore::sort_filter::current_region(sheet, r, c)
    }

    /// 选中行自动调整行高（批量：单遍稀疏区域扫描 + 字号单行缓存 + 单次 `rows.set_many` 重建 + Undo）
    pub(crate) fn autofit_sel_rows(&mut self, ctx: &egui::Context) {
        let (r1, _, mut r2, _) = self.sel_rect();
        if r2 - r1 > 2_000 {
            if let Some((_, _, ur2, _)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                if ur2 >= r1 {
                    r2 = r2.min(ur2);
                } else {
                    r2 = r1;
                }
            } else {
                r2 = r1;
            }
        }
        if r2 - r1 > 50_000 {
            self.status = "选中非空行数过多，请缩小范围后再自适应行高".into();
            return;
        }
        if r1 == r2 {
            self.autofit_row(ctx, r1);
            return;
        }
        let updates = self.compute_autofit_rows(ctx, r1, r2);
        let msg = format!("第 {}–{} 行行高已自适应", r1 + 1, r2 + 1);
        self.apply_row_heights_undoable(updates, msg);
    }

    /// 选中列自动调整列宽（批量：共享二分 `MergeIndex` + O(1) 数值格式宽度上界粗筛 + 并行扫描 + Undo）
    pub(crate) fn autofit_sel_cols(&mut self, ctx: &egui::Context) {
        let (_, c1, _, mut c2) = self.sel_rect();
        if c2 - c1 > 500 {
            if let Some((_, _, _, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                if uc2 >= c1 {
                    c2 = c2.min(uc2);
                } else {
                    c2 = c1;
                }
            } else {
                c2 = c1;
            }
        }
        if c2 - c1 > 2_048 {
            self.status = "选中非空列数过多，请缩小范围后再自适应列宽".into();
            return;
        }
        if c1 == c2 {
            self.autofit_col(ctx, c1);
            return;
        }
        let updates = self.compute_autofit_cols(c1, c2);
        let msg = format!(
            "{}–{} 列宽已自适应",
            super::cell_name(0, c1).trim_end_matches('1'),
            super::cell_name(0, c2).trim_end_matches('1')
        );
        self.apply_col_widths_undoable(updates, msg);
    }

    /// WPS：高亮重复项（条件格式）
    pub(crate) fn highlight_duplicates(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let (r1, c1, mut r2, mut c2) = self.sel_rect();
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
            if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                r2 = r2.min(ur2.max(r1));
                c2 = c2.min(uc2.max(c1));
            } else {
                self.status = "选区内暂无数据可高亮重复项".into();
                return;
            }
        }
        let sh = self.wb.sheet_mut(self.sheet_idx);
        let dxf_idx = sh.cf.dxfs.len();
        sh.cf.dxfs.push(sheetcore::cf::Dxf {
            fill: Some([255, 199, 206]),
            color: Some([156, 0, 6]),
            ..Default::default()
        });
        let next_prio = sh
            .cf
            .blocks
            .iter()
            .flat_map(|b| b.rules.iter().map(|r| r.priority))
            .max()
            .unwrap_or(0)
            + 1;
        sh.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(r1, c1, r2, c2)],
            rules: vec![sheetcore::cf::CfRule {
                kind: sheetcore::cf::CfKind::Dup {
                    unique: false,
                    dxf: dxf_idx,
                },
                stop: false,
                priority: next_prio,
            }],
        });
        sh.cf_dirty = true;
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已高亮 {}:{} 中的重复值", cell_name(r1, c1), cell_name(r2, c2));
    }

    /// WPS：拒绝录入重复项（=COUNTIF($A$1:$A$100,A1)<=1 数据验证）
    pub(crate) fn reject_duplicates(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let (r1, c1, mut r2, mut c2) = self.sel_rect();
        if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
            if let Some((_, _, ur2, uc2)) = self.wb.sheet(self.sheet_idx).used_bounds() {
                r2 = r2.min(ur2.saturating_add(1_000).min(1_048_575).max(r1));
                c2 = c2.min(uc2.max(c1));
            } else {
                r2 = r2.min(r1 + 9_999);
                c2 = c2.min(c1 + 19);
            }
        }
        let abs_range = format!(
            "{}${}:{}${}",
            col_name_abs(c1),
            r1 + 1,
            col_name_abs(c2),
            r2 + 1
        );
        let top_left = cell_name(r1, c1);
        let formula = format!("=COUNTIF({abs_range},{top_left})<=1");
        let sh = self.wb.sheet_mut(self.sheet_idx);
        sh.dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(r1, c1, r2, c2)],
            kind: sheetcore::dv::DvKind::Custom,
            op: sheetcore::dv::DvOp::Equal,
            f1: formula,
            f2: String::new(),
            allow_blank: true,
            show_dropdown: false,
            error_style: sheetcore::dv::DvStyle::Stop,
            show_error: true,
            show_prompt: false,
            error_title: "输入重复".into(),
            error_msg: "当前区域已启用「拒绝录入重复项」，该值已存在。".into(),
            prompt_title: String::new(),
            prompt_msg: String::new(),
        });
        sh.dv_dirty = true;
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已设置 {}:{} 拒绝录入重复值", cell_name(r1, c1), cell_name(r2, c2));
    }

    /// WPS：合并相同单元格（按列纵向下扫连续相同非空值并合并）
    pub(crate) fn merge_same_cells(&mut self) {
        if self.edit_blocked() {
            return;
        }
        let (r1, c1, r2, c2) = self.sel_rect();
        if r2 <= r1 || (r2 - r1) > 10_000 || (c2 - c1) > 200 {
            self.status = "请先选定包含两行以上的连续数据区域".into();
            return;
        }
        let mut to_merge: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut keys: Vec<(u32, u32)> = Vec::new();
        {
            let sh = self.wb.sheet(self.sheet_idx);
            for c in c1..=c2 {
                let mut r = r1;
                while r <= r2 {
                    let disp = sh
                        .get(r, c)
                        .map(|cell| {
                            let st = self.wb.styles.get(sh.style_of(r, c));
                            sheetcore::style::format_value_in(
                                self.wb.date_system,
                                &cell.value,
                                &st.format,
                            )
                        })
                        .unwrap_or_default();
                    if disp.trim().is_empty() {
                        r += 1;
                        continue;
                    }
                    let mut end = r;
                    while end + 1 <= r2 {
                        let next_disp = sh
                            .get(end + 1, c)
                            .map(|cell| {
                                let st = self.wb.styles.get(sh.style_of(end + 1, c));
                                sheetcore::style::format_value_in(
                                    self.wb.date_system,
                                    &cell.value,
                                    &st.format,
                                )
                            })
                            .unwrap_or_default();
                        if next_disp == disp {
                            end += 1;
                        } else {
                            break;
                        }
                    }
                    if end > r {
                        to_merge.push((r, c, end, c));
                        for rr in (r + 1)..=end {
                            keys.push((rr, c));
                        }
                    }
                    r = end + 1;
                }
            }
        }
        if to_merge.is_empty() {
            self.status = "选区中没有可合并的连续相同单元格".into();
            return;
        }
        if !self.write_preflight(&keys, "合并相同单元格") {
            return;
        }
        let before_merges = self.wb.sheet(self.sheet_idx).merges.clone();
        let txn = self.txn_begin(&keys);
        {
            let sh = self.wb.sheet_mut(self.sheet_idx);
            // New runs are disjoint and sorted by (column, row). Test each old merge once.
            sh.merges.retain(|&(top, left, bottom, right)| {
                !(left.max(c1)..=right.min(c2)).any(|col| {
                    let i = to_merge.partition_point(|m| (m.1, m.2) < (col, top));
                    to_merge.get(i).is_some_and(|m| m.1 == col && m.0 <= bottom)
                })
            });
            sh.merges.extend_from_slice(&to_merge);
            for (mr1, mc1, mr2, _) in &to_merge {
                for rr in (*mr1 + 1)..=*mr2 {
                    sh.remove(rr, *mc1);
                }
            }
            sh.merges_dirty = true;
        }
        let after_merges = self.wb.sheet(self.sheet_idx).merges.clone();
        let before = self.wb.sheet(self.sheet_idx);
        let deltas = sheetcore::undo::finish(txn, before);
        self.undo.push(sheetcore::undo::Action {
            label: "合并相同单元格".into(),
            deltas,
            focus: (self.sheet_idx, r1, c1),
            merges: sheetcore::undo::MergeEdit::between(self.sheet_idx, &before_merges, &after_merges),
            snap_before: None,
            snap_after: None,
            by_site: 0,
            pictures: None,
            shapes: None,
            refresh_sizes: None,
        });
        let touched: Vec<_> = keys.into_iter().map(|(r, c)| (self.sheet_idx, r, c)).collect();
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已合并 {} 组相同单元格", to_merge.len());
    }

    /// WPS：自动求和 / 常用汇总函数（先上探，空则左探；单格进预览编辑模式，多格直填总计）
    pub(crate) fn auto_func_wps(&mut self, func: &str) {
        if self.edit_blocked() {
            return;
        }
        let (sr1, sc1, mut sr2, mut sc2) = self.sel_rect();
        // 多格选中且包含内容：自动把汇总放到下方（若多行多列）或右侧空格
        if sr2 > sr1 || sc2 > sc1 {
            let sh = self.wb.sheet(self.sheet_idx);
            let mut has_data = false;
            if (sr2 - sr1 + 1) as u64 * (sc2 - sc1 + 1) as u64 > 200_000 {
                if let Some((_, _, ur2, uc2)) = sh.used_bounds() {
                    if ur2 >= sr1 && uc2 >= sc1 {
                        sr2 = sr2.min(ur2);
                        sc2 = sc2.min(uc2);
                        has_data = true;
                    }
                }
            } else {
                for r in sr1..=sr2 {
                    for c in sc1..=sc2 {
                        if sh.get(r, c).is_some_and(|x| !x.is_blank()) {
                            has_data = true;
                            break;
                        }
                    }
                    if has_data {
                        break;
                    }
                }
            }
            if has_data && (sr2 > sr1 || sc2 > sc1) {
                if sr2 > sr1 {
                    if sr2 >= 1_048_575 || (sc2 - sc1 + 1) > 1_024 {
                        self.status = "选区贴到底部边界或列数过多，无法在下方插入汇总行".into();
                        return;
                    }
                    let target_r = sr2 + 1;
                    let mut keys = Vec::new();
                    for c in sc1..=sc2 {
                        keys.push((target_r, c));
                    }
                    if !self.write_preflight(&keys, func) {
                        return;
                    }
                    let txn = self.txn_begin(&keys);
                    for c in sc1..=sc2 {
                        let formula = format!("={func}({}:{})", cell_name(sr1, c), cell_name(sr2, c));
                        self.wb.sheet_mut(self.sheet_idx).set(
                            target_r,
                            c,
                            Cell {
                                value: Value::Empty,
                                formula: Some(formula.into()),
                            },
                        );
                    }
                    self.txn_end(func, txn);
                    let touched: Vec<_> = keys.iter().map(|&(r, c)| (self.sheet_idx, r, c)).collect();
                    self.recalc_and_broadcast(&touched);
                    self.sel = (target_r, sc1);
                    self.sel_end = (target_r, sc2);
                    self.wb.dirty = true;
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                    return;
                } else {
                    // 单行多列：填到右侧格
                    if sc2 >= 16_383 {
                        self.status = "已到达最后一列边界，无法在右侧生成汇总".into();
                        return;
                    }
                    let target_c = sc2 + 1;
                    let keys = vec![(sr1, target_c)];
                    if !self.write_preflight(&keys, func) {
                        return;
                    }
                    let txn = self.txn_begin(&keys);
                    let formula = format!("={func}({}:{})", cell_name(sr1, sc1), cell_name(sr1, sc2));
                    self.wb.sheet_mut(self.sheet_idx).set(
                        sr1,
                        target_c,
                        Cell {
                            value: Value::Empty,
                            formula: Some(formula.into()),
                        },
                    );
                    self.txn_end(func, txn);
                    self.recalc_and_broadcast(&[(self.sheet_idx, sr1, target_c)]);
                    self.sel = (sr1, target_c);
                    self.sel_end = (sr1, target_c);
                    self.wb.dirty = true;
                    self.doc_gen = self.doc_gen.wrapping_add(1);
                    return;
                }
            }
        }

        // 单格：先向上探连续非空，若上方无数据则向左探
        let (r, c) = self.sel;
        let sheet = self.wb.sheet(self.sheet_idx);
        let mut top = r;
        while top > 0
            && sheet
                .get(top - 1, c)
                .map(|x| !x.is_blank())
                .unwrap_or(false)
        {
            top -= 1;
        }
        let (formula, preview_rect) = if top < r {
            let f = format!("={func}({}:{})", cell_name(top, c), cell_name(r - 1, c));
            (f, Some((top, c, r - 1, c)))
        } else {
            let mut left = c;
            while left > 0
                && sheet
                    .get(r, left - 1)
                    .map(|x| !x.is_blank())
                    .unwrap_or(false)
            {
                left -= 1;
            }
            if left < c {
                let f = format!("={func}({}:{})", cell_name(r, left), cell_name(r, c - 1));
                (f, Some((r, left, r, c - 1)))
            } else {
                (format!("={func}()"), None)
            }
        };
        let caret = if preview_rect.is_some() {
            formula.chars().count()
        } else {
            formula.chars().count().saturating_sub(1)
        };
        self.begin_edit(Some(formula));
        if let Some(ed) = self.editing.as_mut() {
            ed.caret = caret;
            ed.caret_override = Some(caret);
        }
        self.pick_rect = preview_rect;
        if let Some((pr1, pc1, pr2, pc2)) = preview_rect {
            self.status = format!("确认汇总区域 {}:{}（按 Enter 提交，或拖动修改区域）", cell_name(pr1, pc1), cell_name(pr2, pc2));
        } else {
            self.status = format!("请输入 {func} 参数区域并按 Enter");
        }
    }

    fn fill_source_available(&mut self, rect: (u32, u32, u32, u32)) -> bool {
        let (r1, c1, r2, c2) = rect;
        if self.wb.sheet(self.sheet_idx).protection.is_some() && self.rects_have_locked(&[rect]) {
            self.status = "工作表受保护：填充源含锁定单元格".into();
            return false;
        }
        let site = self.net.as_ref().map_or(0, |net| net.site);
        let conflict = self.net.as_ref().and_then(|net| net.host_lock_conflict_rect(self.sheet_idx, (r1, c1, r2, c2)))
            .map(|(_, name, r, c)| (name, r, c))
            .or_else(|| self.locks.conflicts_rect(self.sheet_idx, r1, c1, r2, c2, site, self.lock_now())
                .map(|((r, c), holder)| (holder.name.clone(), r, c)));
        if let Some((name, r, c)) = conflict {
            self.status = format!("{name} 正在编辑 {}，不能智能填充", super::cell_name(r, c));
            return false;
        }
        true
    }

    /// 4 向智能填充：支持向下 / 向上 / 向右 / 向左拖拽填充，保留 `$` 引用、样式、日期及中文序列。
    pub(crate) fn smart_fill_to_mode(
        &mut self,
        target: u32,
        vertical: bool,
        mode: FillMode,
        record_last: bool,
    ) {
        if self.edit_blocked() {
            return;
        }
        let (r1, c1, r2, c2) = self.sel_rect();
        let fill_rect = if vertical {
            if target > r2 {
                (r2 + 1, c1, target, c2)
            } else if target < r1 {
                (target, c1, r1 - 1, c2)
            } else {
                return;
            }
        } else if target > c2 {
            (r1, c2 + 1, r2, target)
        } else if target < c1 {
            (r1, target, r2, c1 - 1)
        } else {
            return;
        };

        let (fr1, fc1, fr2, fc2) = fill_rect;
        if (fr2 - fr1 + 1) as u64 * (fc2 - fc1 + 1) as u64 > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            self.status = "填充区域超过 50 万格，请缩小拖拽范围".into();
            return;
        }
        if !self.fill_source_available((r1, c1, r2, c2)) { return; }
        let has_hidden = !self.wb.sheet(self.sheet_idx).hidden_rows.is_empty();
        let mut keys = sheetcore::undo::rect_keys(fr1, fc1, fr2, fc2);
        if has_hidden {
            let sh = self.wb.sheet(self.sheet_idx);
            keys.retain(|&(r, _)| !sh.hidden_rows.contains(&r));
        }
        if keys.is_empty() {
            self.status = "目标区域没有可填充的可见行（筛选隐藏行已保留）".into();
            return;
        }
        if !self.write_preflight(&keys, "智能填充") {
            return;
        }

        let mut formulas = sheetcore::refs::FillFormulas::default();
        let txn = self.txn_begin(&keys);
        let before_snap = if record_last {
            txn.iter().map(|d| (d.row, d.col, (!d.before.is_blank()).then(|| d.before.clone()), d.before_style)).collect()
        } else { Vec::new() };
        let mut touched_cells = Vec::with_capacity(keys.len());

        let mut apply_cell = |app: &mut SheetApp,
                              sr: u32,
                              sc: u32,
                              tr: u32,
                              tc: u32,
                              new_cell: Option<Cell>,
                              src_sid: u16| {
            let dr = tr as i64 - sr as i64;
            let dc = tc as i64 - sc as i64;
            if mode != FillMode::FormatsOnly {
                if let Some(mut nc) = new_cell {
                    if let Some(f) = &nc.formula {
                        let nf = formulas.translate(f, dr, dc);
                        nc.formula = Some(nf.into());
                        nc.value = Value::Empty;
                    }
                    app.wb.sheet_mut(app.sheet_idx).set(tr, tc, nc);
                } else {
                    app.wb.sheet_mut(app.sheet_idx).remove(tr, tc);
                }
            }
            if mode != FillMode::ValuesOnly {
                app.wb.sheet_mut(app.sheet_idx).set_style(tr, tc, src_sid);
            }
            touched_cells.push((app.sheet_idx, tr, tc));
        };

        if vertical {
            let down = target > r2;
            let dir_sign = if down { 1i64 } else { -1i64 };
            let win_r1 = if down && r2 - r1 >= 1024 { r2 - 1023 } else { r1 };
            let win_r2 = if !down && r2 - r1 >= 1024 { r1 + 1023 } else { r2 };
            let src_len = win_r2 - win_r1 + 1;
            for col in c1..=c2 {
                // 收集源列可见行样本与样式（最多采样靠近边缘的 1024 格，自动跳过筛选隐藏行）
                let mut src_cells: Vec<(u32, Option<Cell>, u16)> = Vec::with_capacity(src_len as usize);
                for sr in win_r1..=win_r2 {
                    let sh = self.wb.sheet(self.sheet_idx);
                    if has_hidden && sh.hidden_rows.contains(&sr) {
                        continue;
                    }
                    src_cells.push((sr, sh.get(sr, col).map(|c| c.into_owned()), sh.style_of(sr, col)));
                }
                if src_cells.is_empty() {
                    continue;
                }
                let src_ordered: Vec<(u32, Option<Cell>, u16)> = if down {
                    src_cells.clone()
                } else {
                    src_cells.iter().rev().cloned().collect()
                };
                let step_rows: Vec<u32> = {
                    let sh = self.wb.sheet(self.sheet_idx);
                    if down {
                        ((r2 + 1)..=target)
                            .filter(|r| !has_hidden || !sh.hidden_rows.contains(r))
                            .collect()
                    } else {
                        (target..r1)
                            .rev()
                            .filter(|r| !has_hidden || !sh.hidden_rows.contains(r))
                            .collect()
                    }
                };
                let nums: Option<Vec<f64>> = src_ordered
                    .iter()
                    .map(|(_, c, _)| match c.as_ref().filter(|x| x.formula.is_none()) {
                        Some(Cell { value: Value::Number(n), .. }) => Some(*n),
                        _ => None,
                    })
                    .collect();
                let texts: Option<Vec<String>> = src_ordered
                    .iter()
                    .map(|(_, c, _)| match c.as_ref().filter(|x| x.formula.is_none()) {
                        Some(Cell { value: Value::Text(t), .. }) if !t.trim().is_empty() => {
                            Some(t.to_string())
                        }
                        _ => None,
                    })
                    .collect();
                let text_samples = texts.as_ref().map(|v| v.iter().map(String::as_str).collect::<Vec<_>>());
                let first_fmt = src_ordered
                    .last()
                    .map(|(_, _, sid)| self.wb.styles.get(*sid).format.clone())
                    .unwrap_or_default();
                let is_date = is_date_style_format(&first_fmt);

                for (idx, tr) in step_rows.into_iter().enumerate() {
                    let step_k = (idx + 1) as i64;
                    let src_idx = idx % src_ordered.len();
                    let (sr, ref scell, src_sid) = src_ordered[src_idx];

                    let gen_cell = if mode == FillMode::CopyCells {
                        scell.clone()
                    } else if let Some(ref nv) = nums {
                        let val = if nv.len() >= 2 {
                            let d = nv[1] - nv[0];
                            nv[nv.len() - 1] + d * (step_k as f64)
                        } else if mode == FillMode::FillSeries || is_date {
                            nv[0] + (dir_sign as f64) * (step_k as f64)
                        } else {
                            nv[0]
                        };
                        Some(Cell { value: Value::Number(val), formula: None })
                    } else if let Some(refs) = &text_samples {
                        if let Some(next_txt) = fill_seq::next_text_sequence(refs, dir_sign, step_k) {
                            Some(Cell { value: Value::Text(next_txt.into()), formula: None })
                        } else {
                            scell.clone()
                        }
                    } else {
                        scell.clone()
                    };

                    apply_cell(self, sr, col, tr, col, gen_cell, src_sid);
                }
            }
        } else {
            let right = target > c2;
            let dir_sign = if right { 1i64 } else { -1i64 };
            let win_c1 = if right && c2 - c1 >= 1024 { c2 - 1023 } else { c1 };
            let win_c2 = if !right && c2 - c1 >= 1024 { c1 + 1023 } else { c2 };
            let src_len = win_c2 - win_c1 + 1;
            for row in r1..=r2 {
                if has_hidden && self.wb.sheet(self.sheet_idx).hidden_rows.contains(&row) {
                    continue;
                }
                let mut src_cells: Vec<(u32, Option<Cell>, u16)> = Vec::with_capacity(src_len as usize);
                for sc in win_c1..=win_c2 {
                    let sh = self.wb.sheet(self.sheet_idx);
                    src_cells.push((sc, sh.get(row, sc).map(|c| c.into_owned()), sh.style_of(row, sc)));
                }
                let src_ordered: Vec<(u32, Option<Cell>, u16)> = if right {
                    src_cells.clone()
                } else {
                    src_cells.iter().rev().cloned().collect()
                };
                let step_cols: Vec<u32> = if right {
                    ((c2 + 1)..=target).collect()
                } else {
                    (target..c1).rev().collect()
                };
                let nums: Option<Vec<f64>> = src_ordered
                    .iter()
                    .map(|(_, c, _)| match c.as_ref().filter(|x| x.formula.is_none()) {
                        Some(Cell { value: Value::Number(n), .. }) => Some(*n),
                        _ => None,
                    })
                    .collect();
                let texts: Option<Vec<String>> = src_ordered
                    .iter()
                    .map(|(_, c, _)| match c.as_ref().filter(|x| x.formula.is_none()) {
                        Some(Cell { value: Value::Text(t), .. }) if !t.trim().is_empty() => {
                            Some(t.to_string())
                        }
                        _ => None,
                    })
                    .collect();
                let text_samples = texts.as_ref().map(|v| v.iter().map(String::as_str).collect::<Vec<_>>());
                let first_fmt = src_ordered
                    .last()
                    .map(|(_, _, sid)| self.wb.styles.get(*sid).format.clone())
                    .unwrap_or_default();
                let is_date = is_date_style_format(&first_fmt);

                for (idx, tc) in step_cols.into_iter().enumerate() {
                    let step_k = (idx + 1) as i64;
                    let src_idx = idx % src_ordered.len();
                    let (sc, ref scell, src_sid) = src_ordered[src_idx];

                    let gen_cell = if mode == FillMode::CopyCells {
                        scell.clone()
                    } else if let Some(ref nv) = nums {
                        let val = if nv.len() >= 2 {
                            let d = nv[1] - nv[0];
                            nv[nv.len() - 1] + d * (step_k as f64)
                        } else if mode == FillMode::FillSeries || is_date {
                            nv[0] + (dir_sign as f64) * (step_k as f64)
                        } else {
                            nv[0]
                        };
                        Some(Cell { value: Value::Number(val), formula: None })
                    } else if let Some(refs) = &text_samples {
                        if let Some(next_txt) = fill_seq::next_text_sequence(refs, dir_sign, step_k) {
                            Some(Cell { value: Value::Text(next_txt.into()), formula: None })
                        } else {
                            scell.clone()
                        }
                    } else {
                        scell.clone()
                    };

                    apply_cell(self, row, sc, row, tc, gen_cell, src_sid);
                }
            }
        }

        // 扩选整个源+目标矩形
        self.sel = (r1.min(fr1), c1.min(fc1));
        self.sel_end = (r2.max(fr2), c2.max(fc2));

        self.txn_end("智能填充", txn);
        self.recalc_and_broadcast(&touched_cells);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        if record_last {
            self.last_fill = Some(LastFillState {
                src_rect: (r1, c1, r2, c2),
                fill_rect,
                target,
                vertical,
                mode,
                sheet_idx: self.sheet_idx,
                before: before_snap,
                popup_open: false,
            });
        }
        self.status = "已填充".into();
    }

    /// 切换「自动填充选项」并重算结果
    pub(crate) fn reapply_last_fill(&mut self, mode: FillMode) {
        let Some(lf) = self.last_fill.take() else {
            return;
        };
        if lf.sheet_idx != self.sheet_idx {
            return;
        }
        if !self.fill_source_available(lf.src_rect) {
            self.last_fill = Some(lf);
            return;
        }
        let restore_keys: Vec<_> = lf.before.iter().map(|&(r, c, _, _)| (r, c)).collect();
        if !self.write_preflight(&restore_keys, "填充选项") { self.last_fill = Some(lf); return; }
        // 恢复目标格原始快照后再重填
        for (rr, cc, cell, sid) in &lf.before {
            match cell {
                Some(c) => self.wb.sheet_mut(self.sheet_idx).set(*rr, *cc, c.clone()),
                None => {
                    let _ = self.wb.sheet_mut(self.sheet_idx).remove(*rr, *cc);
                }
            }
            self.wb.sheet_mut(self.sheet_idx).set_style(*rr, *cc, *sid);
        }
        self.sel = (lf.src_rect.0, lf.src_rect.1);
        self.sel_end = (lf.src_rect.2, lf.src_rect.3);
        self.smart_fill_to_mode(lf.target, lf.vertical, mode, false);
        self.last_fill = Some(LastFillState {
            src_rect: lf.src_rect,
            fill_rect: lf.fill_rect,
            target: lf.target,
            vertical: lf.vertical,
            mode,
            sheet_idx: lf.sheet_idx,
            before: lf.before,
            popup_open: false,
        });
    }

    /// 切换浮动「粘贴选项」：0=保留源格式 1=仅值 2=仅公式 3=仅格式 4=转置 5=值和数字格式 6=值和源格式 7=无边框粘贴
    pub(crate) fn reapply_last_paste(&mut self, mode: u8) {
        let Some(lp) = self.last_paste.take() else {
            return;
        };
        if lp.sheet_idx != self.sheet_idx || lp.cells.is_empty() {
            return;
        }
        let si = lp.sheet_idx;
        let (r0, c0, _, _) = lp.rect;
        let orig_h = lp.cells.len();
        let orig_w = lp.cells.iter().map(|r| r.len()).max().unwrap_or(0);
        let (h, w) = if mode == 4 { (orig_w, orig_h) } else { (orig_h, orig_w) };
        if h == 0 || w == 0 || (h as u64) * (w as u64) > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            return;
        }
        if r0.saturating_add(h as u32 - 1) > 1_048_575 || c0.saturating_add(w as u32 - 1) > 16_383 {
            self.status = "转置后超出工作表边界".into();
            return;
        }
        let keys = sheetcore::undo::rect_keys(r0, c0, r0 + h as u32 - 1, c0 + w as u32 - 1);
        if !self.write_preflight(&keys, "粘贴选项") {
            return;
        }
        let old_keys: Vec<_> = lp.before.iter().map(|&(r, c, _, _)| (r, c)).collect();
        if !self.write_preflight(&old_keys, "恢复粘贴区域") { self.last_paste = Some(lp); return; }
        // 先恢复粘前原貌
        for (rr, cc, cell, sid) in &lp.before {
            match cell {
                Some(c) => self.wb.sheet_mut(si).set(*rr, *cc, c.clone()),
                None => {
                    let _ = self.wb.sheet_mut(si).remove(*rr, *cc);
                }
            }
            self.wb.sheet_mut(si).set_style(*rr, *cc, *sid);
        }
        let new_before: Vec<_> = keys
            .iter()
            .map(|&(rr, cc)| {
                let sh = self.wb.sheet(si);
                (rr, cc, sh.get(rr, cc).map(|c| c.into_owned()), sh.style_of(rr, cc))
            })
            .collect();
        let txn = self.txn_begin(&keys);
        let mut touched = Vec::new();
        for i in 0..h {
            for j in 0..w {
                let (cell, sid) = if mode == 4 {
                    lp.cells
                        .get(j)
                        .and_then(|r| r.get(i))
                        .cloned()
                        .unwrap_or_default()
                } else {
                    lp.cells
                        .get(i)
                        .and_then(|r| r.get(j))
                        .cloned()
                        .unwrap_or_default()
                };
                let rr = r0 + i as u32;
                let cc = c0 + j as u32;
                match mode {
                    1 => {
                        let mut v = cell.clone();
                        v.formula = None;
                        self.wb.sheet_mut(si).set(rr, cc, v);
                    }
                    2 => {
                        self.wb.sheet_mut(si).set(rr, cc, cell.clone());
                    }
                    3 => {
                        self.wb.sheet_mut(si).set_style(rr, cc, sid);
                    }
                    5 => {
                        let mut v = cell.clone();
                        v.formula = None;
                        self.wb.sheet_mut(si).set(rr, cc, v);
                        let src_fmt = self.wb.styles.get(sid).format.clone();
                        let dst_sid = self.wb.sheet(si).style_of(rr, cc);
                        let mut st = self.wb.styles.get(dst_sid).clone();
                        st.format = src_fmt;
                        let nsid = self.wb.styles.intern(st) as u16;
                        self.wb.sheet_mut(si).set_style(rr, cc, nsid);
                    }
                    6 => {
                        let mut v = cell.clone();
                        v.formula = None;
                        self.wb.sheet_mut(si).set(rr, cc, v);
                        self.wb.sheet_mut(si).set_style(rr, cc, sid);
                    }
                    7 => {
                        self.wb.sheet_mut(si).set(rr, cc, cell.clone());
                        let mut st = self.wb.styles.get(sid).clone();
                        st.border = [sheetcore::style::Border::default(); 4];
                        let nsid = self.wb.styles.intern(st) as u16;
                        self.wb.sheet_mut(si).set_style(rr, cc, nsid);
                    }
                    _ => {
                        self.wb.sheet_mut(si).set(rr, cc, cell.clone());
                        self.wb.sheet_mut(si).set_style(rr, cc, sid);
                    }
                }
                touched.push((si, rr, cc));
            }
        }
        self.txn_end("更改粘贴方式", txn);
        self.recalc_and_broadcast(&touched);
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.last_paste = Some(LastPasteState {
            sheet_idx: si,
            rect: (r0, c0, r0 + h as u32 - 1, c0 + w as u32 - 1),
            cells: lp.cells,
            mode,
            before: new_before,
            popup_open: false,
        });
    }
}

struct NowSerial {
    serial: f64,
}

fn chrono_free_now(is_time: bool) -> NowSerial {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 默认按本地常见时区偏移（UTC+8，若存在精确需求仍保持 Excel serial 一致性；日期天内秒数准确）
    let local_secs = secs + 8 * 3600;
    if is_time {
        let day_secs = local_secs.rem_euclid(86400) as f64;
        NowSerial { serial: day_secs / 86400.0 }
    } else {
        // 1970-01-01 在 Excel 1900 序列里为 25569
        let days = local_secs.div_euclid(86400);
        NowSerial { serial: (25569 + days) as f64 }
    }
}
