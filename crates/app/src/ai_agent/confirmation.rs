use super::*;

/// Resolve implicit targets when this call reaches the front of the tool queue.
/// The selection can move while the approval card is open.
impl SheetApp {
    pub(crate) fn stage_ai_confirmation(
        &self,
        call: &mut ToolCallSpec,
    ) -> Result<Option<String>, String> {
        if !matches!(
            call.name.as_str(),
            "data_operation" | "set_cells" | "fill_formula_range"
        ) {
            return Ok(None);
        }
        let args = &mut call.arguments;
        let requested_sheet = args
            .get("sheet")
            .map(|value| value.as_str().ok_or_else(|| "工作表名称无效".to_string()))
            .transpose()?;
        let index = self
            .resolve_sheet_index(requested_sheet)
            .ok_or("未找到指定工作表")?;
        let sheet = self.wb.sheet(index);
        let label = match call.name.as_str() {
            "set_cells" => {
                let edits = args
                    .get("edits")
                    .and_then(JsonValue::as_array)
                    .ok_or("缺少 edits 数组")?;
                if edits.is_empty() || edits.len() > 2000 {
                    return Err("单次 edits 数量需在 1..=2000 之间".into());
                }
                let mut overwritten = BTreeSet::new();
                for edit in edits {
                    let addr = edit
                        .get("cell")
                        .and_then(JsonValue::as_str)
                        .ok_or("缺少单元格地址")?;
                    let (r, c) = checked_address(addr).ok_or("单元格地址无效")?;
                    if (edit.get("value").is_some()
                        || edit.get("formula").and_then(JsonValue::as_str).is_some())
                        && sheet.get(r, c).is_some_and(|cell| !cell.is_blank())
                    {
                        overwritten.insert((r, c));
                    }
                }
                if overwritten.is_empty() {
                    return Ok(None);
                }
                let cells = overwritten
                    .iter()
                    .map(|&(r, c)| cell_name(r, c))
                    .collect::<Vec<_>>()
                    .join("、");
                format!(
                    "覆盖「{}」中已有内容的 {} 个单元格：{cells}",
                    sheet.name,
                    overwritten.len()
                )
            }
            name => {
                let action = if name == "fill_formula_range" {
                    "填充公式"
                } else {
                    match args.get("op").and_then(JsonValue::as_str) {
                        Some("clear_contents") => "清空内容",
                        Some("deduplicate") => "删除重复行",
                        Some("merge_same") => "合并相同单元格",
                        Some("sort_asc") => "升序排序",
                        Some("sort_desc") => "降序排序",
                        _ => return Ok(None),
                    }
                };
                let (r1, c1, r2, c2) = match args.get("range") {
                    Some(value) => {
                        let range = value.as_str().ok_or("区域格式无效")?;
                        checked_ai_range(range).ok_or("区域地址无效")?
                    }
                    None if name == "data_operation" => self.sel_rect(),
                    None => return Err("填充公式需要指定区域".into()),
                };
                let range = range_label((r1, c1), (r2, c2));
                if name == "fill_formula_range" {
                    if u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1) > 50_000 {
                        return Err("公式填充区域超过 50000 格".into());
                    }
                    if !sheetcore::sync::iter_sheet_rect(sheet, (r1, c1, r2, c2))
                        .any(|(_, v)| !v.is_blank())
                    {
                        return Ok(None);
                    }
                }
                args["range"] = json!(range);
                format!("{action} · {}!{range}", sheet.name)
            }
        };
        args["sheet"] = json!(sheet.name);
        Ok(Some(label))
    }

    /// 构建待确认 AI 工具调用的网格可视化预览（高亮受影响区域与修改前后值对比）。
    pub(crate) fn build_ai_pending_preview(
        &self,
        call: &ToolCallSpec,
    ) -> Option<AiPendingPreview> {
        if !matches!(
            call.name.as_str(),
            "data_operation" | "set_cells" | "fill_formula_range"
        ) {
            return None;
        }
        let args = &call.arguments;
        let requested_sheet = args.get("sheet").and_then(JsonValue::as_str);
        let sheet_idx = self.resolve_sheet_index(requested_sheet)?;
        let sheet = self.wb.sheet(sheet_idx);
        match call.name.as_str() {
            "set_cells" => {
                let edits = args.get("edits").and_then(JsonValue::as_array)?;
                let mut rects = Vec::new();
                let mut cells = Vec::new();
                for edit in edits {
                    let Some(addr) = edit.get("cell").and_then(JsonValue::as_str) else {
                        continue;
                    };
                    let Some((r, c)) = checked_address(addr) else {
                        continue;
                    };
                    let old_disp = sheet
                        .get(r, c)
                        .map(|cell| {
                            cell.formula
                                .as_deref()
                                .map(str::to_string)
                                .unwrap_or_else(|| cell.value.display())
                        })
                        .unwrap_or_default();
                    let new_disp = if let Some(f) = edit.get("formula").and_then(JsonValue::as_str) {
                        if f.starts_with('=') {
                            f.to_string()
                        } else {
                            format!("={f}")
                        }
                    } else if let Some(v) = edit.get("value") {
                        match v {
                            JsonValue::Null => "（清空）".to_string(),
                            JsonValue::String(s) => s.clone(),
                            JsonValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                            JsonValue::Number(n) => n.to_string(),
                            other => other.to_string(),
                        }
                    } else {
                        "（格式）".to_string()
                    };
                    if rects.len() < 256 {
                        rects.push((r, c, r, c));
                    }
                    if cells.len() < 512 {
                        cells.push((r, c, old_disp, new_disp));
                    }
                }
                if rects.is_empty() {
                    return None;
                }
                Some(AiPendingPreview {
                    sheet_idx,
                    rects,
                    cells,
                })
            }
            name => {
                let (r1, c1, r2, c2) = match args.get("range").and_then(JsonValue::as_str) {
                    Some(range) => checked_ai_range(range)?,
                    None if name == "data_operation" => self.sel_rect(),
                    None => return None,
                };
                let after_hint = if name == "fill_formula_range" {
                    args.get("formula")
                        .and_then(JsonValue::as_str)
                        .map(|f| if f.starts_with('=') { f.to_string() } else { format!("={f}") })
                        .unwrap_or_else(|| "（填充公式）".to_string())
                } else {
                    match args.get("op").and_then(JsonValue::as_str) {
                        Some("clear_contents") => "（清空）".to_string(),
                        Some("deduplicate") => "（去重）".to_string(),
                        Some("merge_same") => "（合并相同项）".to_string(),
                        Some("sort_asc") => "（升序重排）".to_string(),
                        Some("sort_desc") => "（降序重排）".to_string(),
                        _ => "（批量操作）".to_string(),
                    }
                };
                let mut cells = Vec::new();
                for (key, cell) in sheet.iter_rect((r1, c1, r2, c2)) {
                    if !cell.is_blank() {
                        cells.push((key.row, key.col, cell.value.display(), after_hint.clone()));
                        if cells.len() >= 128 {
                            break;
                        }
                    }
                }
                Some(AiPendingPreview {
                    sheet_idx,
                    rects: vec![(r1, c1, r2, c2)],
                    cells,
                })
            }
        }
    }
}
