use crate::{cell_name, ui_kit, SheetApp};
use eframe::egui;

/// 迷你图对话框：新建（edit 为空）或编辑已有几条
#[derive(Clone)]
pub(super) struct SparkDlg {
    pub(super) kind: sheetcore::sparkline::SparkKind,
    /// 数据范围文本（可带 `表名!`）
    pub(super) source: String,
    /// 位置范围文本
    pub(super) location: String,
    color: [u8; 3],
    markers: bool,
    pub(super) high: bool,
    low: bool,
    first: bool,
    last: bool,
    negative: bool,
    axis: bool,
    empty: sheetcore::sparkline::EmptyAs,
    /// 正在编辑的锚点（空 = 新建）
    edit: Vec<(u32, u32)>,
    msg: String,
}

impl Default for SparkDlg {
    fn default() -> Self {
        Self {
            kind: sheetcore::sparkline::SparkKind::Line,
            source: String::new(),
            location: String::new(),
            color: [0x37, 0x60, 0x92],
            markers: false,
            high: false,
            low: false,
            first: false,
            last: false,
            negative: false,
            axis: false,
            empty: sheetcore::sparkline::EmptyAs::Gap,
            edit: Vec::new(),
            msg: String::new(),
        }
    }
}

impl SheetApp {
    /// 选区里第一条迷你图（上下文页签与状态都用它；`sh.sparklines` 按 `(c, r)` 有序，二分定位 O(log S)）
    pub(super) fn spark_at_sel(&self) -> Option<&sheetcore::sparkline::Sparkline> {
        let sp = &self.wb.sheet(self.sheet_idx).sparklines;
        if sp.is_empty() {
            return None;
        }
        let (r1, c1, r2, c2) = self.sel_rect();
        if r1 == r2 && c1 == c2 {
            let idx = sp.binary_search_by_key(&(c1, r1), |s| (s.c, s.r)).ok()?;
            return sp.get(idx);
        }
        let start = sp.partition_point(|s| (s.c, s.r) < (c1, r1));
        for s in &sp[start..] {
            if s.c > c2 {
                break;
            }
            if s.r >= r1 && s.r <= r2 && s.c >= c1 {
                return Some(s);
            }
        }
        None
    }

    /// 选区里所有迷你图的锚点（排好序，编辑多条时用）
    fn spark_anchors_in_sel(&self) -> Vec<(u32, u32)> {
        let (r1, c1, r2, c2) = self.sel_rect();
        let mut v: Vec<(u32, u32)> = self
            .wb
            .sheet(self.sheet_idx)
            .sparklines
            .iter()
            .filter(|s| s.r >= r1 && s.r <= r2 && s.c >= c1 && s.c <= c2)
            .map(|s| (s.r, s.c))
            .collect();
        v.sort_unstable();
        v
    }

    /// 打开「创建迷你图」：数据默认取当前选区，位置默认选区右侧一格。
    pub(super) fn spark_open_create(&mut self, kind: sheetcore::sparkline::SparkKind) {
        let (r1, c1, r2, c2) = self.sel_rect();
        let single = r1 == r2 && c1 == c2;
        let source = if single {
            String::new()
        } else {
            format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
        };
        let location = if single {
            cell_name(r2, c2 + 1)
        } else {
            cell_name(r2, c2 + 1)
        };
        self.spark_dlg = Some(SparkDlg {
            kind,
            source,
            location,
            ..Default::default()
        });
    }

    /// 打开「编辑迷你图」：用选区内第一条的属性填表，多格编辑时自动合并源数据包围盒。
    pub(super) fn spark_open_edit(&mut self) {
        let Some(sp) = self.spark_at_sel().cloned() else {
            self.status = "先选中一个带迷你图的单元格".into();
            return;
        };
        let own = self.wb.sheet(self.sheet_idx).name.clone();
        let anchors = self.spark_anchors_in_sel();
        let (location, source) = if anchors.len() == 1 {
            (cell_name(sp.r, sp.c), sp.data_ref(&own))
        } else {
            let rs: Vec<u32> = anchors.iter().map(|a| a.0).collect();
            let cs: Vec<u32> = anchors.iter().map(|a| a.1).collect();
            let loc = format!(
                "{}:{}",
                cell_name(*rs.iter().min().unwrap(), *cs.iter().min().unwrap()),
                cell_name(*rs.iter().max().unwrap(), *cs.iter().max().unwrap())
            );
            let sh = self.wb.sheet(self.sheet_idx);
            let matched: Vec<&sheetcore::sparkline::Sparkline> = sh
                .sparklines
                .iter()
                .filter(|s| anchors.contains(&(s.r, s.c)))
                .collect();
            let same_sheet = matched.iter().all(|s| s.source_sheet == sp.source_sheet);
            let src = if same_sheet && !matched.is_empty() {
                let r1 = matched.iter().map(|s| s.rect.0).min().unwrap();
                let c1 = matched.iter().map(|s| s.rect.1).min().unwrap();
                let r2 = matched.iter().map(|s| s.rect.2).max().unwrap();
                let c2 = matched.iter().map(|s| s.rect.3).max().unwrap();
                let range_str = format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2));
                match sp.source_sheet.as_deref() {
                    Some(sn) if sn != own => format!("'{sn}'!{range_str}"),
                    _ => range_str,
                }
            } else {
                sp.data_ref(&own)
            };
            (loc, src)
        };
        self.spark_dlg = Some(SparkDlg {
            kind: sp.kind,
            source,
            location,
            color: sp.color,
            markers: sp.markers,
            high: sp.high,
            low: sp.low,
            first: sp.first,
            last: sp.last,
            negative: sp.negative,
            axis: sp.axis,
            empty: sp.empty,
            edit: anchors,
            msg: String::new(),
        });
    }

    /// 对选区内所有迷你图改一处属性
    pub(super) fn spark_apply_sel<F: Fn(&mut sheetcore::sparkline::Sparkline)>(&mut self, f: F) {
        let anchors = self.spark_anchors_in_sel();
        if anchors.is_empty() {
            self.status = "先选中带迷你图的单元格".into();
            return;
        }
        let si = self.sheet_idx;
        let before = self.wb.sheet(si).sparklines.clone();
        let sh = self.wb.sheet_mut(si);
        for sp in sh.sparklines.iter_mut() {
            if anchors.contains(&(sp.r, sp.c)) {
                f(sp);
            }
        }
        let after = sh.sparklines.clone();
        sh.sparklines_dirty = true;
        self.undo.push_meta(
            "修改迷你图",
            (si, self.sel.0, self.sel.1),
            sheetcore::undo::MetaEdit {
                sheet: si,
                sparklines: Some((before, after)),
                ..Default::default()
            },
        );
        self.wb.dirty = true;
        // 装载期的后台重算会整份替换 wb：改过模型必须同时推进 doc_gen
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已修改 {} 条迷你图", anchors.len());
    }

    /// 清除选区内的迷你图
    pub(super) fn spark_delete_in_sel(&mut self) {
        let anchors = self.spark_anchors_in_sel();
        if anchors.is_empty() {
            self.status = "选区内没有迷你图".into();
            return;
        }
        let si = self.sheet_idx;
        let n = anchors.len();
        let before = self.wb.sheet(si).sparklines.clone();
        let sh = self.wb.sheet_mut(si);
        sh.sparklines.retain(|s| !anchors.contains(&(s.r, s.c)));
        let after = sh.sparklines.clone();
        sh.sparklines_dirty = true;
        self.undo.push_meta(
            "清除迷你图",
            (si, self.sel.0, self.sel.1),
            sheetcore::undo::MetaEdit {
                sheet: si,
                sparklines: Some((before, after)),
                ..Default::default()
            },
        );
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.status = format!("已清除 {n} 条迷你图");
    }

    /// 把对话框里的设置写进模型。返回错误文本（对话框里显示）。
    pub(super) fn spark_commit(&mut self, d: &SparkDlg) -> Result<usize, String> {
        let own = self.wb.sheet(self.sheet_idx).name.clone();
        let src_txt = d.source.trim();
        if src_txt.is_empty() {
            return Err("请填写数据范围，例如 A1:E1".into());
        }
        let Some((sheet_name, srect)) = sheetcore::sparkline::parse_ref(src_txt) else {
            return Err(format!("数据范围「{src_txt}」看不懂，写成 A1:E1 或 Sheet1!A1:E1"));
        };
        let src_sheet = match sheet_name {
            Some(n) if n != own => {
                if !self.wb.sheets.iter().any(|s| s.name == n) {
                    return Err(format!("没有名为「{n}」的工作表"));
                }
                Some(n)
            }
            _ => None,
        };
        let loc_txt = d.location.trim();
        if loc_txt.is_empty() {
            return Err("请填写位置范围（迷你图放在哪个单元格）".into());
        }
        let Some((loc_sheet, loc)) = sheetcore::sparkline::parse_ref(loc_txt) else {
            return Err(format!("位置范围「{loc_txt}」看不懂"));
        };
        if let Some(n) = loc_sheet {
            if n != own {
                return Err("迷你图只能放在当前工作表上".into());
            }
        }
        let loc_rows = loc.2 - loc.0 + 1;
        let loc_cols = loc.3 - loc.1 + 1;
        let n_loc = loc_rows * loc_cols;
        let src_rows = srect.2 - srect.0 + 1;
        let src_cols = srect.3 - srect.1 + 1;
        if n_loc > 4096 {
            return Err("位置范围太大（上限 4096 格）".into());
        }
        // 数据区与位置怎么一一对应：单格 → 整段；多行单列 → 逐行；单行多列 → 逐列
        let mut plan: Vec<((u32, u32), (u32, u32, u32, u32))> = Vec::new();
        if n_loc == 1 {
            plan.push(((loc.0, loc.1), srect));
        } else if loc_cols == 1 && src_rows == loc_rows {
            for i in 0..loc_rows {
                plan.push(((loc.0 + i, loc.1), (srect.0 + i, srect.1, srect.0 + i, srect.3)));
            }
        } else if loc_rows == 1 && src_cols == loc_cols {
            for j in 0..loc_cols {
                plan.push(((loc.0, loc.1 + j), (srect.0, srect.1 + j, srect.2, srect.1 + j)));
            }
        } else if d.edit.len() > 1 {
            // 多格批量编辑且各迷你图源区域无法按单一矩形 1:1 重映射时：保留各条原数据范围，批量同步类型与样式属性
            let si = self.sheet_idx;
            let before_sparks = self.wb.sheet(si).sparklines.clone();
            let mut updated = 0usize;
            {
                let sh = self.wb.sheet_mut(si);
                for sp in sh.sparklines.iter_mut() {
                    if d.edit.contains(&(sp.r, sp.c)) {
                        sp.kind = d.kind;
                        sp.color = d.color;
                        sp.markers = d.markers;
                        sp.high = d.high;
                        sp.low = d.low;
                        sp.first = d.first;
                        sp.last = d.last;
                        sp.negative = d.negative;
                        sp.axis = d.axis;
                        sp.empty = d.empty;
                        updated += 1;
                    }
                }
                if updated > 0 {
                    sh.sparklines_dirty = true;
                }
            }
            if updated > 0 {
                let after_sparks = self.wb.sheet(si).sparklines.clone();
                self.undo.push_meta(
                    "批量修改迷你图",
                    (si, self.sel.0, self.sel.1),
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        sparklines: Some((before_sparks, after_sparks)),
                        ..Default::default()
                    },
                );
                self.doc_gen = self.doc_gen.wrapping_add(1);
                self.wb.dirty = true;
                self.status = format!("已批量更新 {updated} 条迷你图的样式属性");
                return Ok(updated);
            }
            return Err(format!(
                "位置范围 {loc_rows} 行 × {loc_cols} 列，数据范围 {src_rows} 行 × {src_cols} 列：\
                 多格位置要和数据的行数或列数一一对应"
            ));
        } else {
            return Err(format!(
                "位置范围 {loc_rows} 行 × {loc_cols} 列，数据范围 {src_rows} 行 × {src_cols} 列：\
                 多格位置要和数据的行数或列数一一对应"
            ));
        }
        let base = sheetcore::sparkline::Sparkline {
            kind: d.kind,
            color: d.color,
            markers: d.markers,
            high: d.high,
            low: d.low,
            first: d.first,
            last: d.last,
            negative: d.negative,
            axis: d.axis,
            empty: d.empty,
            source_sheet: src_sheet,
            ..Default::default()
        };
        let si = self.sheet_idx;
        let before_sparks = self.wb.sheet(si).sparklines.clone();
        let sh = self.wb.sheet_mut(si);
        // 编辑模式先摘掉被编辑的那几条，再按新设置写回去
        if !d.edit.is_empty() {
            sh.sparklines.retain(|s| !d.edit.contains(&(s.r, s.c)));
        }
        let mut made = 0usize;
        for ((r, c), rect) in plan {
            sh.sparklines.retain(|s| !(s.r == r && s.c == c));
            sh.sparklines.push(sheetcore::sparkline::Sparkline {
                r,
                c,
                rect,
                ..base.clone()
            });
            made += 1;
        }
        // 同一锚点保持按行列有序，写回时同格式的自动成组
        sh.sparklines.sort_by_key(|s| (s.c, s.r));
        sh.sparklines_dirty = true;
        let after_sparks = sh.sparklines.clone();
        let first = sh.sparklines.iter().find(|s| s.c == loc.1 && s.r >= loc.0).map(|s| (s.r, s.c));
        self.undo.push_meta(
            "创建迷你图",
            (si, loc.0, loc.1),
            sheetcore::undo::MetaEdit {
                sheet: si,
                sparklines: Some((before_sparks, after_sparks)),
                ..Default::default()
            },
        );
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.wb.dirty = true;
        if let Some((r, c)) = first {
            self.sel = (r, c);
            self.sel_end = (r, c);
        }
        self.status = format!("已创建 {made} 条迷你图");
        Ok(made)
    }

    /// 「创建/编辑迷你图」对话框
    pub(super) fn spark_dialog(&mut self, ctx: &egui::Context) {
        let Some(d0) = self.spark_dlg.clone() else { return };
        let mut d = d0;
        let mut open = true;
        let mut ok = false;
        let editing = !d.edit.is_empty();
        let title = if editing { "编辑迷你图" } else { "创建迷你图" };
        ui_kit::window(ctx, title, 460.0)
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui_kit::field(ui, "数据范围", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut d.source)
                            .desired_width(300.0)
                            .hint_text("A1:E1（可带表名：Sheet2!A1:E1）"),
                    );
                });
                ui_kit::field(ui, "位置范围", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut d.location)
                            .desired_width(300.0)
                            .hint_text("F1"),
                    );
                });
                ui.add_space(4.0);
                ui.label(egui::RichText::new("类型").size(12.0).weak());
                ui.horizontal(|ui| {
                    for k in [
                        sheetcore::sparkline::SparkKind::Line,
                        sheetcore::sparkline::SparkKind::Column,
                        sheetcore::sparkline::SparkKind::WinLoss,
                    ] {
                        if ui.radio(d.kind == k, k.label()).clicked() {
                            d.kind = k;
                        }
                    }
                    // 右侧缩略预览：改什么都立刻能看到
                    let (rect, _) = ui.allocate_exact_size(
                        egui::Vec2::new(150.0, 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect(
                        rect,
                        2.0,
                        egui::Color32::WHITE,
                        egui::Stroke::new(1.0, ui_kit::BORDER),
                        egui::StrokeKind::Inside,
                    );
                    let mut prev = sheetcore::sparkline::Sparkline {
                        kind: d.kind,
                        color: d.color,
                        markers: d.markers,
                        high: d.high,
                        low: d.low,
                        first: d.first,
                        last: d.last,
                        negative: d.negative,
                        axis: d.axis,
                        ..Default::default()
                    };
                    prev.empty = d.empty;
                    crate::spark_view::preview(&ui.painter().clone(), rect.shrink(3.0), &prev);
                });
                ui.add_space(2.0);
                ui.label(egui::RichText::new("颜色").size(12.0).weak());
                ui.horizontal(|ui| {
                    let palette: [[u8; 3]; 8] = [
                        [0x37, 0x60, 0x92],
                        [0x00, 0x70, 0xC0],
                        [0x00, 0xB0, 0x50],
                        [0xFF, 0xC0, 0x00],
                        [0xFF, 0x00, 0x00],
                        [0x70, 0x30, 0xA0],
                        [0x59, 0x59, 0x59],
                        [0x00, 0x00, 0x00],
                    ];
                    for c in palette {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::click());
                        let on = d.color == c;
                        ui.painter().rect_filled(
                            rect.shrink(1.0),
                            2.0,
                            egui::Color32::from_rgb(c[0], c[1], c[2]),
                        );
                        if on {
                            ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(2.0, ui_kit::ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if resp.clicked() {
                            d.color = c;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(egui::RichText::new("显示").size(12.0).weak());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.markers, "标记");
                    ui.checkbox(&mut d.high, "高点");
                    ui.checkbox(&mut d.low, "低点");
                    ui.checkbox(&mut d.first, "首点");
                    ui.checkbox(&mut d.last, "末点");
                    ui.checkbox(&mut d.negative, "负点");
                    ui.checkbox(&mut d.axis, "坐标轴");
                });
                ui_kit::field(ui, "空单元格", |ui| {
                    egui::ComboBox::from_id_salt("spark_empty")
                        .selected_text(d.empty.label())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for e in [
                                sheetcore::sparkline::EmptyAs::Gap,
                                sheetcore::sparkline::EmptyAs::Zero,
                                sheetcore::sparkline::EmptyAs::Connect,
                            ] {
                                if ui.selectable_label(d.empty == e, e.label()).clicked() {
                                    d.empty = e;
                                }
                            }
                        });
                });
                if !d.msg.is_empty() {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(&d.msg)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(192, 0, 0)),
                    );
                }
                ui_kit::footer(ui, |ui| {
                    if ui_kit::secondary_button(ui, "取消").clicked() {
                        open = false;
                    }
                    if ui_kit::primary_button(ui, "确定").clicked() {
                        ok = true;
                    }
                });
            });
        if ok {
            match self.spark_commit(&d) {
                Ok(_) => {
                    eprintln!(
                        "[spark] 已写 {} 条，dirty={}",
                        self.wb.sheet(self.sheet_idx).sparklines.len(),
                        self.wb.sheet(self.sheet_idx).sparklines_dirty
                    );
                    self.spark_dlg = None;
                }
                Err(e) => {
                    d.msg = e;
                    self.spark_dlg = Some(d);
                }
            }
        } else {
            self.spark_dlg = if open { Some(d) } else { None };
        }
    }
}
