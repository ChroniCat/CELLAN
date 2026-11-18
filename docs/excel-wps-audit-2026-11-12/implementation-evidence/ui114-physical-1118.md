# UI-11.4 插入函数向导：真实控件、分类、拾取与公式落位（2026-11-18）

## 本轮修复的三个真实缺陷

1. **点 fx 会崩 app（accesskit panic）**：`fx` 分支在 `fx_edit` 这个 TextEdit 还没画出来的同一帧就
   `request_focus("fx_edit")`；egui 的 accesskit 更新拿到一个不在节点表里的 focused id，直接
   `Focused id ... not in node list` 退出。已删掉两处“提前抢焦点”（fx 按钮、编辑栏输入行），
   改由下一帧 `TextEdit` 绘制时自己 `request_focus`。
2. **向导确定后把旧单元格内容当前缀**：A1 原值 `S1-A1`，向导确定后编辑栏变成
   `S1-=AVERAGE(A1:A3)`。`insert_formula_at_caret` 在非编辑态原来调用 `begin_edit(None)`，
   而 `begin_edit(None)` 会装载当前格文本；现在改成 `begin_edit(Some(String::new()))`，
   非编辑态=替换当前格。回归用例 `ui114_wizard_inserts_at_caret_without_double_equals`
   新增了“原格有文本时不得混入”的断言。
3. **向导的「拾取」被模态层吃掉**：对话框的遮罩 Area 覆盖全屏并且 `dialog_modal` 让网格
   `Sense::hover()`，点「拾取」后根本无法在表格里拖选。`ui_kit::Dlg` 新增
   `block_background(bool)`；`fn_wizard_dialog` 在 `wiz_pick.is_some()` 时关闭遮罩，
   `dialog_modal` 也在拾取期间放行鼠标，松手后自动恢复。

另外：`fx` 按钮原来只进入编辑栏编辑，不符合 Excel 的「fx=插入函数」；现在 `fx` 和功能区
`InsertFunction` 共用 `open_fn_wizard()`，都打开向导。

## 真实界面验证（不是纯单测）

### 1) fx 真实控件点击 → 向导打开

```text
computer_use capture som -> 公式栏 fx (Custom, 253,247,24,24)
computer_use click element=88 delivery_mode=background
  -> Performed UIA Invoke on [88] (screen (265,259))
capture som -> Dialog / 插入函数 / 类别 / 475 个 / 右侧 SUM 参数 / 确定、取消
```

截图：`ui114-probe/wizard-ui-1118-live.png`
（vision 复核：标题「插入函数」；类别「全部」；搜索框占位「输入函数名或首字母」；475 个；
右侧 SUM 签名 `SUM(number1,[number2],...)`、number1/number2、拾取、`=SUM()`、取消/确定。）

### 2) 类别下拉真实打开并筛选

```text
UIA invoke ComboBox -> 12 个类别按钮（全部/数学与三角函数/统计/文本/逻辑/查找与引用/
日期与时间/财务/信息/工程/数据库/其他）
UIA toggle 统计 -> UIA 回读：
CAT=统计
COUNT=141 个
FUNCS=138
```

截图：`ui114-probe/wizard-category-1118.png`（vision 复核：统计选中、141 个、列表从 AVEDEV 开始）。

### 3) 列表真实选中 AVERAGE，确定插公式（旧格有文本）

```text
UIA toggle AVERAGE -> 预览 =AVERAGE()
UIA invoke 确定
EDIT==AVERAGE()
STATUS=单元格模式 输入
（A1 原值 S1-A1，未被拼成 S1-=AVERAGE()）
```

截图：`ui114-probe/wizard-apply-fixed-1118.png`（vision 复核：编辑栏 `=AVERAGE()`、状态「已插入函数 AVERAGE」）。

### 4) 拾取按钮 + 真实鼠标拖选区域 → 参数 B25:B28 → 确定

```text
UIA invoke 第一行「拾取」
STATUS=在表格里拖选区域，松手后填进参数

computer_use drag background
  from window-pixel (260,760) -> (260,820)
  screen (350,787) -> (350,847) in 500ms / 20 steps
UIA 回读参数框：EDITVAL=B25:B28
STATUS=拾取 B25:B28

UIA invoke 确定
EDIT==SUM(B25:B28)
STATUS=单元格模式 输入
STATUS=已插入函数 SUM
```

截图：`ui114-probe/wizard-pick-1118.png`
（vision 复核：B25:B28 绿色选中；名称框 B25:B28；编辑栏 `=SUM(B25:B28)`；状态「已插入函数 SUM」、计数 4。）

这条同时验证了第 3 个缺陷的修复：拾取期间能在对话框外的网格真实拖选，松手后参数自动落位。

## 自动探针交叉验证

```text
SC_CMD='6:InsertFunction' SC_KEYSEQ='20:enter' SC_WIZ_LOG=1 ...
[keyseq] frame 20 -> enter
[wiz] applied formula="=SUM()" editing=Some(("=SUM()", 5)) sel=(0, 0)
```

A1 原值 `S1-A1`，应用后缓冲是 `=SUM()`，光标 5（括号内），再次证明不再拼前缀。

## 回归

```text
cargo test --offline --no-default-features --features glow -p app --bin app -- --test-threads=1
running 394 tests
test result: ok. 393 passed; 0 failed; 1 ignored; finished in 64.93s
APP_EXIT=0

cargo test ... -p app --test ui_regression -- --test-threads=1
running 20 tests
test result: ok. 20 passed; 0 failed; 0 ignored; finished in 119.70s
INT_EXIT=0
```

## 还没做（不写成已验收）

- 「帮助」超链接真实打开默认浏览器未复核；URL 编码已有单测（`wizard_help_url_encodes_function_name`）。
- 真实中文 IME 在向导搜索框里的组合输入未测（本机 RDP 断连，无法保持前台输入法组合窗口）。

## 证据文件

- `ui114-probe/wizard-ui-1118-live.png` — fx 真实控件打开向导
- `ui114-probe/wizard-category-1118.png` — 统计类别筛选 141 个
- `ui114-probe/wizard-apply-fixed-1118.png` — AVERAGE 确定、旧格无前缀
- `ui114-probe/wizard-pick-1118.png` — 拾取拖选 B25:B28、=SUM(B25:B28)
- `work/probe/ui114-pick-1118.txt` — UIA 拾取/确定原始读数
- `work/probe/ui114-category-1118.txt` — 类别筛选原始读数
- `work/probe/ui114_apply1118.log` — SC_KEYSEQ 探针原始日志
- `work/goal-logs/app-ui114-full.log`、`work/goal-logs/app-ui114-integration.log` — 回归原始日志
