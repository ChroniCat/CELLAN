## 当前状态（最后更新：2026-11-13 UTC+08，C02 之后）

- 分支 `main`，跟踪 `origin/main`
- 已验收：任务 **6/61**（UI-02、C13、QA-03、C03、C01、C02）；切片 **17/148**
- 待验收：UI-01（真实中文输入法与硬件键盘未执行）
- 基线（round11 实测，与本状态同一代码）：
  - sheetcore **60 个测试二进制 / 525 passed / 0 failed / 34 ignored**，EXIT=0
  - app 单测 **141 passed / 0 failed**，EXIT=0
  - GUI `ui_regression` **12 passed / 0 failed / 8 ignored**（round10 值，C02 只动 core）
- 工具链：`C:/Users/Administrator/.cargo/bin/cargo.exe`；`source scripts/audit_env.sh <标签>`

## C01.2 / C01.3 关键结论（不要重复踩）

- XLS FORMULA 缓存布局：**高两字节 num[6..8] == 0xFFFF** 才是特殊值（num[0]=类型、num[2]=取值）；
  低两字节可以正好是 0xFFFF（1.0000000000145517），按低字判断会整格丢数。
- 写端也必须用高字节标号；旧写法 Excel 会读成 NaN。
- FORMULA 的字符串结果在随后的 **STRING 记录**（0x0207）里，之前完全没解析。
- 增量 vs 全量：`recalc_dirty(wb, &deps, &[(sheet,row,col)])`，`Deps::build(&wb)`，CellKey 是元组。

## C02 关键结论（不要重复踩）

- 二元运算操作数必须保留 (rows, cols)：`operand_block`；广播按两个维度判，不能只比元素个数
  （2×2 与 1×4 元素数相同但 Excel 报 #VALUE!）。
- `Sheet::set` 会把被写的格从 `spilled` 摘掉 —— 溢出区里的用户输入靠这个存活，
  否则 `clear_spills` 会把它当旧溢出值删掉再被数组覆盖（已加变异回归）。
- 溢出值不落盘，只存锚点公式；重开后要 `recalc_all` 才看得到子格。

## 下一步（按依赖优先级）

1. **UI-01 收尾**：真实中文输入法（组合中取消、候选窗上屏）与真实硬件键盘的 Alt+Enter 光标位置。
   必须真机人工执行，事件注入不算；做完前 UI-01 保持待验收。
2. 之后按 `task-index.md` 的 P0 顺序推进未开工任务（C 系列 → UI 系列 → QA 系列），
   每完成一个切片就写证据 + 更新台账 + 提交；父任务只在全部切片与整体验收通过后才标已验收。

## 尚未解决的风险 / 未执行

- 物理断电不可模拟（用保存阶段强杀进程代替，边界已在证据里写明）
- 真实 IME、硬件键盘、DPI、打印、Excel/WPS 对拍、跨机 LAN 协作：未执行
- 8 个 GUI 用例仍依赖 `tests/test-work` 外部夹具（ignored）
- 窗口关闭时 dirty 文档无保存提示：既有缺口，登记后续
- 网格级 Ctrl+C/X/V/F/H 未进命令表（留 UI-06/07）
- Supabase 生产数据只读、未接触
