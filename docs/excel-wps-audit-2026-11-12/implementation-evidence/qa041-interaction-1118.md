# QA-04.1 原生编辑/快捷键/焦点 — 真实交互复核（2026-11-18）

程序：`target/debug/app.exe`（dsec 构建，`cargo build -p app --offline -j 2`）
环境：Windows 11 体验会话 `rdp-tcp#0` / `console`（真实前台，`GetForegroundWindow == app H`；本文件每次注入前用 `work/probe/focus_hwnd2.ps1` 把前台切到 app，`FG=855348`）。
输入：真实 `SendInput` 扫描码/Unicode、真实鼠标 `Click-At`；不是 `SC_SEL`/`SC_OPEN_DIALOG` 注入。
窗口：1440×900；文档标签条实测两个文件：`qa041_doc1.xlsx`、`qa041_doc2.xlsx`。

## ② F2 左右键与 Esc 丢弃草稿

夹具：`work/probe/qa041_live.xlsx`（A1=`seed`，A2=`keep`）。

| 步骤 | UIA 读取 |
|---|---|
| 启动后 | `名称框=A1`，`编辑栏=seed`，行 `A1，seed，已选中` |
| F2 → `abc` → Left ×2 → `X` → Esc | `编辑栏=seed`；`A1，seed，已选中` |
| F2 → `=1+1` → Enter | `名称框=A2`；`编辑栏=keep`（提交后下移一格） |
| 上箭头 | `A1，2，公式 =1+1`（公式求值 2） |

结论：左右键移动草稿光标；Esc 丢弃草稿且原值不变；Enter 提交后单步下移。

## ③ 公式栏与格内编辑一致

夹具：`work/probe/qa041_fb.xlsx`（A1=1，B1=2，A2=0）。

| 入口 | 操作 | UIA/落盘结果 |
|---|---|---|
| 格内 F2 | 先 Esc 清掉追加态，F2，输入 `=B1+1`，Enter | `A1，3，公式 =B1+1` |
| 公式栏 | 选中 A2，点编辑栏，`Ctrl+A`，输入 `=B1+2`，Enter | `A2，4，公式 =B1+2` |

公式栏编辑和格内编辑写同一缓冲区；提交后公式栏/单元格显示同一个值，`=B1+1` / `=B1+2` 分别求值 3 / 4。
注：F2 直接键入是追加语义（Excel 同样把插入点放在现有内容末尾）；整体替换先 Esc 或公式栏全选，这不是丢字。

## ④ 拖选越界自动滚动

夹具：`work/probe/qa041_drag.xlsx`（200 行 × 2 列，列宽/行高默认）。

- 鼠标从 `(232,388)` 按下列头下方拖到 `(232,915)`：`选区 A1:A27，27 行 1 列，活动单元格 A1`。
- 继续拖到 `(232,1000)`：`选区 A1:A32，32 行 1 列`。

可见区域一次只显示约 28 行，选择扩到 A32，说明拖过底边后真实自动滚动，不是按固定可见高度截断。

## ⑤ 筛选后复制/粘贴

夹具：`work/probe/qa041_filter2.xlsx`：

```
A1 名称  B1 数额
A2 苹果  B2 10
A3 香蕉  B3 20   （筛选隐藏）
A4 橙子  B4 30
A5 葡萄  B5 40   （筛选隐藏）
auto_filter.ref=A1:B5
```

操作（`work/probe/qa041_filter_copy2.ps1`）：名称框输入 `A1:B5`，物理 `Ctrl+C`，名称框输入 `D1`，物理 `Ctrl+V`，物理 `Ctrl+S`。

用 openpyxl 解包回读：

| 位置 | 列 D | 列 E |
|---|---|---|
| 第 1 行 | `名称` | `数额` |
| 第 2 行 | `苹果` | `10` |
| 第 3 行 | `橙子` | `30` |
| 第 4 行 | `None` | `None` |

隐藏的香蕉（第 3 行）和葡萄（第 5 行）没有被复制；表头与两个可见数据行按 Excel 可见单元格语义压紧。实现见 `crates/app/src/main.rs` 的 `clip_copy`：`hidden_rows`（筛选/分级隐藏）跳过，`hidden_rows_user`（用户手动隐藏）不跳过；筛选复制走可见行 TSV，XMLSS 无法表达非连续行。
新增单测：`filtered_copy_compacts_visible_rows_and_paste_skips_filtered_rows`，同时断言手动隐藏行仍复制。
顺带修复：名称框输入地址回车后，同一个 Enter 之前会被网格再消费一次（例如 `D1` 变 `D2`）；现在在名称框 lost_focus 分支 `consume_key(Enter)`，真实 `D1` 输入后活动格保持 `D1`（本轮筛选复制即在此修复后复跑）。

## ⑥ 剪贴板

本切片沿用 QA-04.2 的四方向真实结果：Excel → app 15/15、WPS → app 15/15、app → Excel 15/15（含 `=1+2`）、app → WPS 与 Excel → WPS 逐格相同；中文、日期、前导零、公式、合并、格式均已覆盖，证据见 `qa042-clipboard.txt` 与 `qa042/`。本文件只补内部筛选复制（⑤）。

## ⑦ 格式/排序弹窗 Enter/Esc 不泄漏

复现：选中 `qa041_filter2.xlsx` 的 `D2`，真实 `Ctrl+1` 打开「设置单元格格式」，UIA 读到 `Pane 设置单元格格式`、`取消`、`确定`、数字/对齐/字体页。

| 操作 | 修复前 | 修复后 |
|---|---|---|
| 弹窗中 Enter | 弹窗关闭后选区 `D2 → D4`（Enter 泄漏到网格，向下跳过隐藏行） | 弹窗关闭后仍 `D2，名称，已选中` |
| 弹窗中 Esc | 关闭且选区不变 | 关闭且选区不变 |

根因：`dialog_modal` 未包含独立的 `fmt_dlg`，`handle_keys` 没有统一的弹出层短路；`ui_kit::primary_button` 先用 Enter 应用/关闭对话框，随后同一 Enter 又被网格消费。
修复：
- `crates/app/src/main.rs`：`handle_keys` 开头在编辑态之后增加 `if self.dialog_modal || self.fmt_dlg { return; }`；
- `dialog_modal` 计算加入 `self.fmt_dlg`，弹窗期间网格不响应指针。
新增单测：`format_dialog_enter_does_not_leak_grid_move`（`handle_keys` 不移动选区，对话框仍打开由渲染层处理）。

## ⑧ 多文档归属

夹具：`qa041_doc1.xlsx`（A1=`DOC1`，A2=1，A5=`scroll1`），`qa041_doc2.xlsx`（A1=`DOC2`，A2=1，A5=`scroll1`）。启动 `app.exe qa041_doc1.xlsx`，环境 `SC_OPEN_PATH=work/probe/qa041_doc2.xlsx`，真实 `Ctrl+O` 打开第二档；真实鼠标点标签切换。

| 检查 | 读数 |
|---|---|
| 打开后两个标签 | `qa041_doc1.xlsx` / `qa041_doc2.xlsx`；活动 `A1，DOC2` |
| doc1 的 A2 改为 9 | doc1 标题 `qa041_doc1.xlsx *`（dirty），`A2，9` |
| 切到 doc2 | 标题 `qa041_doc2.xlsx`（无 `*`），`A2，1` |
| 切回 doc1，Ctrl+Z | `A2，1`（撤销只回退 doc1）；doc2 仍 `A2，1` |
| doc1 选 A5，doc2 选 A2 | 切回 doc1 后仍 `A5，scroll1`；doc2 仍 `A2，1`，选区各自保留 |
| doc1 PgDn 后 | 活动 `A32`，可见行头最小 `第 28 行`；doc2 可见行头最小 `第 1 行` |
| 切回 doc1 | 活动 `A32`，可见行头最小 `第 28 行`，滚动位置保留 |

截图：`qa041-multidoc-1118.png`（1440×900，两个文档标签，doc1 活动、dirty 星号与 A32/第 28 行可见）。

结论：输入、撤销、选区、滚动、未保存状态都按文档归属，切换标签不串档。

## ① / ⑥ 引用

- 中文 IME 选词 Enter 不多跳：`ui01-ime-1118.md`、`qa042-ime-1118.md`；真实微软拼音 `zhongwen` → Space → `中文` → Enter，A1=`中文`，选区单步 A2。
- 跨 Excel/WPS 剪贴板：`qa042-clipboard.txt`、`qa042/`。

## 测试

- `cargo build -p app --offline -j 2`：通过。
- `cargo test -p app --offline -j 2 --bin app -- --test-threads=1 filtered_copy_compacts`：1 passed。
- `cargo test -p app --offline -j 2 --bin app -- --test-threads=1 format_dialog_enter_does_not_leak`：1 passed。
