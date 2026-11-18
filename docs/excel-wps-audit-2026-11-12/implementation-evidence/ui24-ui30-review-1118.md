# UI-24.1 / UI-30 机检复核（2026-11-18）

## UI-24.1 模态焦点、默认按钮、关闭恢复

`ui24-modal-focus.txt` 已有 egui 事件循环用例；本轮把相关用例全部重跑：

```text
cargo test --offline --no-default-features --features glow -j2 -p app --bin app \
  -- modal_dialog default_button shift_tab_cycles tabbing_into_scrolled \
  every_dialog_primary click_matrix_every_ribbon_action legacy_cf_and_dv \
  --test-threads=1
running 9 tests
test ... app_shortcuts_are_ignored_while_a_modal_dialog_is_open ... ok
test ... default_button_answers_enter_only_when_no_field_has_focus ... ok
test ... every_dialog_primary_action_uses_the_default_button_helper ... ok
test ... modal_dialog_click_does_not_select_grid_behind ... ok
test ... modal_dialog_traps_tab_focus_and_restores_it_on_escape ... ok
test ... shift_tab_cycles_backwards_inside_modal_and_never_escapes ... ok
test ... tabbing_into_scrolled_dialog_scrolls_the_focused_button_into_view ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; finished in 4.84s
```

**仍未做**：真实物理键盘在 GUI 里按 Tab/Shift+Tab/Enter；本机无交互桌面，`SC_KEYSEQ`
是 egui 事件注入，不能冒充物理键盘。

## UI-30.1 旧菜单删除与命令可达性

已实现：旧 `toolbar_notes/view/data/page` 与 `set_freeze/export_pdf` 死代码删除前，先把
CompareBooks/CompareSync/CfQuick/DvQuick 接进功能区；283 个动作逐项点击矩阵覆盖。

```text
test ... click_matrix_every_ribbon_action_is_handled ... ok
test ... legacy_cf_and_dv_menus_are_reachable_from_the_ribbon ... ok
```

证据：`ui30-legacy-menu-removal.txt`、`ui30-click-matrix.txt`、`ui30-command-reachability.txt`、
`ui30/ui30-ribbon3.png`、`ui30/ui30-cfquick2.png`。

**仍未做**：14 个需要文件对话框/导入/共享/联网检查的动作没有真人点击；点击矩阵证明
“有实现、不落兜底”，不等于每个业务效果都人工复核。

## 结论

UI-24.1、UI-30.1、UI-30 转入已验收（带残余 not_done）；UI-24 父项仍因 UI-24.2
真实屏幕阅读器未测保持实施中。
