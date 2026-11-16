# UI-11：函数向导的类别筛选与帮助链接（2026-11-17）

## 类别数据从哪来

不能凭印象给 475 个函数分类。做法是**机械抽取 + 人工补齐**：

1. `functions.rs` / `extra.rs` 里每个分派段都带 `// ---- 段名` 注释（数学/三角、统计、文本、逻辑、
   查找、日期时间、财务、信息、复数、工程进制、数据库…），脚本按段落注释把该段里的
   `"NAME" =>` 与 `"A" | "B" =>` 链全部收进表；
2. 段名归一成 Excel 的中文类别（数学 / 三角 → 数学与三角函数，查找 → 查找与引用，
   日期时间 → 日期与时间，复数 / 工程进制 / 单位换算 → 工程，求值 → 数学与三角函数）；
3. 抽取不到的 138 个（分布函数、工程贝塞尔、动态数组、财务债券等）**逐条人工归类**；
4. 少数按前缀规则兜底（`IM*`→工程、`BIN2*`/`BIT*`→工程、`NORMS*`/`NORM*`→统计…）。

结果：`IMPLEMENTED` 里 475 个函数，**0 个落进「其他」**（测试 `fncat_covers_most_implemented_functions`
断言 `other == 0`）。表在 `crates/core/src/formula/fncat.rs`，前缀规则在 `RULES`。

## 界面

搜索行前面加类别下拉（宽 140）：全部 / 数学与三角函数 / 统计 / 文本 / 逻辑 / 查找与引用 /
日期与时间 / 财务 / 信息 / 工程 / 数据库 / 其他。选中类别后列表按 `fncat::category()` 过滤，
与搜索框是**与**关系。

函数名旁边的「帮助」链接（`help_url()`）跳到官方文档搜索：
`https://support.microsoft.com/zh-cn/search/results?query=<NAME>%20函数`。
函数名与「函数」之间用百分号转义空格——不转义的话链接会断在空格处，测试里断言了这一点。

## 验证

- `fncat_spot_checks`：SUM/ABS→数学、AVERAGE→统计、VLOOKUP→查找与引用、TEXT→文本、IF→逻辑、
  TODAY→日期与时间、PMT→财务、ISBLANK→信息、IMABS→工程、DSUM→数据库、SUMIF→数学、
  小写 `vlookup` 也能命中、未知函数→其他；
- `fncat_covers_most_implemented_functions`：475/475 已归类（`[fncat] total=475 other=0`）；
- `wizard_help_url_encodes_function_name`：URL 以 `...query=VLOOKUP%20` 开头且不含裸空格；
- app 全量 **363 passed**、core `fncat` **2 passed**。

## 未验

本机无交互桌面，**没有真人拉过类别下拉、也没有真的点开过帮助链接**（浏览器能不能起来取决于
默认浏览器，本机是 RDP 断连状态）。按既有口径登记「已接线、待真人点击」，不写成已验收。
