use super::*;

/// Resolve implicit targets when this call reaches the front of the tool queue.
/// The selection can move while the approval card is open.
impl SheetApp {
    pub(super) fn stage_ai_confirmation(
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
}
