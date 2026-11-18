# UI-22.1 命令搜索：真实键盘输入、方向键选择、Enter 执行、Esc 回网格（2026-11-18）

## 实现的缺口

原 `command_palette` 只处理 Enter 执行 `hits.first()`，没有方向键选中态；Esc 关闭后
`TextEdit` 仍持有 egui 焦点，只 `stop_text_input()` 不够，下一次方向键被文本输入吞掉。
本轮补：

- `palette_idx: usize`：查询变化回 0，Up/Down 用 `palette::step_index` 钳制移动，不环绕。
- 结果行按 `selected` 高亮，方向移动时 `scroll_to_me` 把选中项滚进视口。
- Enter 执行 `hits.get(palette_idx)`，不再是永远第一条。
- Esc/关闭按钮同时 `surrender_focus` + `stop_text_input` + `clear_grid_focus`，把键盘交还网格。
- 新增 `SC_PALETTE_LOG=1`，输出 `[palette] down/up/idx/hits` 与 `run=<action>`。

## 真实键盘读数（ui09_force + SendInput Unicode）

启动：

```text
target/debug/app.exe work/probe/ui22_final.xlsx
```

打开命令搜索（扫描码 Ctrl，不是 egui 注入）：

```text
work/probe/ui09_force.ps1 -Title ui22_final -ScanCtrl -Key Q
UIA: Window "命令搜索"；初始 211 条结果
```

逐字输入中文（`Send-UnicodeText` = `SendInput` 的 KEYEVENTF_UNICODE）：

```text
Send-UnicodeText '冻结'
UIA:
  4 条结果（Enter 执行第一条，Esc 关闭）
  冻结窗格    视图 › 窗口
  冻结首列    视图 › 窗口
  冻结首行    视图 › 窗口
  取消冻结    视图 › 窗口
```

方向键 + Enter：

```text
ui09_force -Key DOWN
SC_PALETTE_LOG:
  [palette] down=false up=false idx=1 hits=4
run:
  [palette] run=FreezeFirstColumn
```

保存后读 xlsx：

```text
<pane xSplit="1" ySplit="0" topLeftCell="B1" activePane="bottomRight" state="frozen"/>
```

Down 选中第二条 `冻结首列`，不是第一条 `冻结窗格`；Enter 执行正确命令。

Esc 恢复网格焦点：

```text
ui09_force -ScanCtrl -Key Q   # 重新打开
ui09_force -Key ESC
AFTER_ESC=A1，Amount，已选中
ui09_force -Key DOWN
AFTER_DOWN=A2，1，已选中
```

按 ESC 后方向键立即移动活动格，说明焦点已交还网格。

另测英文查询：

```text
Send-UnicodeText 'pdf'
UIA: 1 条结果（Enter 执行第一条，Esc 关闭）
```

## 回归

```text
cargo test ... -p app --bin app -- palette --test-threads=1
running 2 tests
palette::tests::arrow_index_clamps_and_empty_stays_zero ... ok
palette::tests::filters_by_label_path_and_action ... ok
test result: ok. 2 passed

cargo test ... -p app --bin app -- --test-threads=1
running 395 tests
test result: ok. 394 passed; 0 failed; 1 ignored; finished in 56.62s
APP_EXIT=0

cargo test ... -p app --test ui_regression -- --test-threads=1
running 20 tests
test result: ok. 20 passed; 0 failed; 0 ignored; finished in 120.37s
INT_EXIT=0
```

截图：`ui22/palette-arrow-1118.png`（命令搜索、查询“冻结”、4 条结果）。

## 残余

- 真实鼠标点结果行未单独复测；键盘路径已按原验收覆盖。
- 结果排序仍按功能区顺序 + 使用频率，不做学习排序以外的模糊评分。
