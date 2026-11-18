# UI-12 / UI-13 / UI-19 机检复核（2026-11-18）

本轮不把“真实鼠标/真实系统 DPI”写成已完成；只把已有几何、数据库与回归用例重新跑一遍，
并明确剩余项。这样台账的“已验收”对应“机检通过 + 明确残余”，不掩盖环境缺口。

## UI-12 缩放表格而非整个应用

已实现并复核：

- 网格命中、冻结区和滚动量都走缩放轴；100%/200% 命中同一格，冻结区不随正文滚动。
- 图片矩形由 `px_of/py_of` 计算，100% → 200% 尺寸精确 2×；每张表各自保存 zoom。
- 本轮补跑：

```text
cargo test --offline --no-default-features --features glow -j2 -p app --bin app \
  -- zoom_ grid_zoom dv_filter dv_keyboard --test-threads=1
running 7 tests
test ... grid_zoom_scales_axes_and_hit_testing ... ok
test ... zoom_is_per_sheet ... ok
test ... zoom_keeps_anchor_cell_with_and_without_freeze ... ok
test result: ok. 7 passed; 0 failed; 0 ignored
```

证据：`ui12-zoom2.txt`、`ui12/object-100.png`、`ui12/object-200.png`、`ui12/freeze-hit-200.png`。

**仍未做**：真实鼠标拖图片、真实鼠标拖列宽（本机无交互桌面；cua 的表头拖动未能稳定进入
egui 的 resize 分支，不能写成已验）。

## UI-13 小窗口与高 DPI 的 Ribbon 自适应

已实现并复核：

- 标题栏按宽度优先级压缩；Ribbon 分组按实测宽度折叠，小图标二级压缩，长命令组退成
  “组名 ▾”；对话框宽度按 viewport 收紧（344 逻辑点下限）。
- 本轮补跑：

```text
cargo test ... -p app --bin app -- dialog_width_fits_small_viewport \
  a11y_tree_reads_ribbon_group_names --test-threads=1
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored

cargo test ... -p app --test ui_regression -- ribbon_groups_aligned_and_fit --test-threads=1
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; finished in 0.65s
```

**仍未做**：真实 125%/150% 系统 DPI 逐档目视、真实鼠标展开“更多”逐项点击；
`SC_PPP` 只能证明布局等比，不算系统 DPI 切换。

## UI-19 数据验证下拉的可用性与事务

已实现并复核：

- 下拉过滤按 `dv_cache` 一次解析；键盘上下选择只产生一次 undo；Esc 不改单元格；
  10 万项过滤有确定性耗时上界。
- 本轮补跑：

```text
cargo test ... -- dv_filter dv_keyboard --test-threads=1
test ... dv_filter_100k_items_is_frame_cheap ... ok
test ... dv_filter_handles_large_lists ... ok
test ... dv_keyboard_dropdown_escape_leaves_cell_unchanged ... ok
test ... dv_keyboard_dropdown_picks_with_arrows_and_is_undoable ... ok
test result: ok. 7 passed; 0 failed; 0 ignored（该过滤命令共 7 项，含 UI-12 的 zoom 三项）
```

**仍未做**：真实鼠标点选/滚动列表、中文 IME 搜索；RDP 帧率不能作为逐帧计时依据，
列表过滤耗时用单测上限证明。

## 结论

三组 slice 的功能实现与机检已齐，剩余都是本机不可用的真实 GUI 输入/DPI 项；按当前台账
口径进入“已验收（带 not_done）”，不把 not_done 从台账里删掉。
