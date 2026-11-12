# Hermes 内 DeepSeek 的实施合同

这份需求适用于本仓库 Rust/eframe 原生程序。不要生成网页替代品、不要迁移到Electron、不要改为另一套表格引擎来绕过当前实现。先修已确认缺陷，再补能力。用户当前委托本轮审查者产出需求；后续实施由用户在Hermes中明确启动。

## 1 可直接粘贴的首次任务提示词

```text
你要在 F:\finance-native-sheets-ready 实施一个有界任务。
先读取当前AGENTS.md、README.md，以及：
docs/excel-wps-audit-2026-11-12/README.md
docs/excel-wps-audit-2026-11-12/task-index.md
docs/excel-wps-audit-2026-11-12/ui-requirements.md 中 UI-01
docs/excel-wps-audit-2026-11-12/repro/ui_edit_probe.rs
docs/excel-wps-audit-2026-11-12/acceptance-scenarios.md 中 A01/A02

本次只修 UI-01：格内编辑后Enter/Tab丢失输入。
源码定位是审计基线df48672的位置，先在当前代码重现，不要机械套行号。
现有确认：handle_keys→draw_grid_pane，Text("123")后Enter或Tab，
editing从Some变None、选区移动，但A1仍Empty、dirty=false、undo为空。
先把可复用探针改成正式有意义的回归，确认当前分支仍失败；
再修复编辑缓冲所有权与提交/取消路径，不进行无关重构。

必须验证：Enter、Tab、Shift反向、Esc、F2、公式栏、验证失败不跳格；
有一次撤销、dirty正确、保存重开正确。真实IME未测时明确写未测。
不得修改期望结果来让测试通过；不得用SC_*直接设置结果代替输入路径。
隔离TEMP/TMP/LOCALAPPDATA，清除继承SC_*；只用合成工作簿。
本机Supabase是真实生产库，任何相关操作默认只读，本任务无需接触它。
不要同时修其他任务，不要升级依赖或批量cargo fmt全仓库。

交付：现状复现、根因、最小diff、通过的命令与测试数量、
原生窗口验证/未验证项、剩余风险、提交哈希。
检查git diff/status；只提交本任务代码和测试，测试失败/分支分叉/
存在不确定他人修改/敏感文件时停止推送并说明。遵循本机AGENTS的Git约定。
```

首次运行前完成QA-03.1所要求的最小环境隔离；不必先重构全部测试基础设施。UI-01通过后再领取UI-02.1。若发现当前分支已修复，提供原失败样本如今通过的证据，把任务标为“已实现待验收”并继续按用户批准的队列，不能为了产生diff重复重写。

## 2 每轮上下文与修改边界

1. 读取本轮任务正文、直接依赖、相关测试和目标函数；主报告用于定位全局，不必每轮加载全部61项。
2. 在 `tasks.json` 选择一个任务切片；L级任务不得一次全部领取。切片没有独立验收文本时，继承父任务中与本切片有关的验收，并在动手前明确列出 acceptance、切片depends_on、改动文件和本轮不实施的边界；缺少这些领取合同不能开始大范围修改。
3. 用最小合成输入证明缺陷。源码风险必须先构造故障或操作路径，不把推测直接写成已复现。
4. 优先复用 `txn_begin/txn_end`、依赖图、现有保存回执、单实例与文档状态机制；确认其真实职责，不能假设事务设施自动完成dirty、重算和广播。
5. 通常控制在一个行为、一个主要模块、少量相关测试。需要数据模型迁移的任务先定义类型/序列化兼容，再接UI；不要临时使用字符串或裸索引伪装类型安全。
6. 修改公共行为时跑受影响模块测试；验证通过后停止无意义重复测试。发现新问题建后续项，除非它阻断当前任务验收。
7. 一轮只在完成全部要求后改任务状态；“按钮出现”“编译通过”“没有崩溃”均不等于功能完成。

## 3 每轮必须回报的模板

```text
任务：UI-01（或明确切片ID）
基线/结束提交：...
现状复现：输入、入口、实际结果、证据文件
根因：代码路径及为什么原测试漏掉
修改：范围与关键行为，哪些现有模块被复用
验收：每条通过/失败/未执行/阻断及证据
测试：完整命令、passed/failed/ignored/filtered数量
原生对拍：Excel版本、WPS版本、差异及采用的规则
风险：仍未覆盖的操作、文件兼容与迁移问题
Git：只包含本任务的diff、commit、push结果或停止原因
后续：仅列直接相关的下一个任务
```

## 4 运行环境和测试命令

PowerShell中的 `cargo` 可能未加入PATH。本机可用绝对路径如下；若换机器应先发现工具链，不要把本机路径写死到产品代码。

```powershell
$cargoExe = 'C:\Users\Administrator\.cargo\bin\cargo.exe'
$auditRun = Join-Path (Get-Location) ('work\hermes-check-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $auditRun | Out-Null
$env:TEMP = $auditRun
$env:TMP = $auditRun
$env:LOCALAPPDATA = Join-Path $auditRun 'profile'
New-Item -ItemType Directory -Path $env:LOCALAPPDATA | Out-Null
Get-ChildItem Env:SC_* | ForEach-Object { Remove-Item ('Env:' + $_.Name) }

& $cargoExe build -p app --no-default-features --features glow --offline
& $cargoExe test -p sheetcore --offline -- --test-threads=1
& $cargoExe test -p app --no-default-features --features glow --offline --bin app -- --test-threads=1 --skip net::tests:: --skip net::presence_tests::
& $cargoExe test -p app --no-default-features --features glow --offline --test ui_regression -- --test-threads=1
```

上述是完整本地非网络基线，不要求每个小切片都跑全部。首次/发布前、跨模型变更需要完整基线；日常运行针对性测试再加相关回归。`--offline`依赖本机完整缓存，缺缓存时按环境获取依赖，不能删测试回避失败。图形测试必须串行，避免多个程序抢占测试单实例端口。4项网络测试未被这些命令验证，涉及协作的任务必须在隔离回环环境另跑。

搬迁仓库后若测试找不到已存在夹具，先检查编译时路径和旧target缓存。可仅更新相关源码mtime触发重编译或选择独立target目录；不要把测试路径改成原开发者目录来“修复”。本轮首次路径错误疑似来自搬迁缓存，旧二进制未保留而未完全确认根因；当前目录重新编译后现有核心套件通过。

严禁设置旧 `SC_B1/SC_B3/SC_B4` 运行探针，它们含固定开发目录写入。只有审阅过的测试参数可以加入显式allowlist。不能把TEMP/LOCALAPPDATA改成真实业务目录，也不能通过“杀掉所有Excel/WPS/app进程”解决文件锁；仅关闭本轮创建且明确记录的实例/合成文件。

## 5 正确性合同

- **事务：** 预检全部目标 → 建立before → 修改内存 → 更新依赖/dirty/doc_gen/修订/广播 → 统一撤销记录；失败不得留下半操作。具体顺序以现有异步文档模型为准，避免把远端变更算成本地undo。
- **保存：** 先写临时输出并验证，失败不改原文件；成功后由回执确认对应文档与版本。不能把日志中的错误当作成功，也不能在插图等编辑步骤提前改磁盘文件。
- **类型：** 数字、文本、布尔、空、错误与日期体系必须明确。文本“00123”和18位账号不能被无提示转浮点；文本“#DIV/0!”不能等同公式错误。
- **兼容：** 已支持的未知XML/关系/宏/图表对象要保留；不能编辑的能力应明确限制。修改普通格不应顺带删除第三方属性或降级对象。
- **UI：** 统一命令处理，按钮/菜单/快捷键/搜索不能各写一套语义；取消不改模型，Esc不泄漏到底层，错误提示可操作。
- **视觉：** 系统DPI和工作表zoom分别负责UI与内容，不用整体放大缩小掩盖网格布局问题；按visual-comparison.md验收。

## 6 多代理与合并规则

用户已要求复杂工作可并行探索，但实现文件归属必须独立。当前 `main.rs`、`formula/mod.rs`、`xlsx.rs` 都是冲突热点；两个实现代理不可同时拥有同一文件。可并行：一个只读行为/测试探索、一个有界实现、一个只读对抗核验。审查者必须挑战保存、撤销、权限旁路、菜单可达性、数据模型与UI不一致；主实施者负责综合判断。

不因发现旧代码复杂就先开展全仓库重构。需要拆模块时，先提交纯搬移且行为不变的独立切片，通过回归后再改行为。禁止把大量未验证的新按钮、空handler和“TODO以后补”作为功能补齐成果。

## 7 Git与任务状态

按用户全局约定检查diff/status、跑相关验证、简洁commit并push当前分支。存在失败测试、分支分叉、不确定他人改动或敏感内容时停止推送并说明，不强推、不悄悄减少断言。本次审查包含一个故意暴露当前缺陷的失败诊断，交付时需区分“原套件通过”与“新增行为失败”，不能用前者掩盖后者。

实施者更新任务状态时保留原现状和证据，在任务尾追加修复提交/验收结论，不把审计历史改成“从来没有问题”。推荐状态：待实施 → 复现确认 → 实施中 → 待原生验收 → 已验收；环境缺失单独记录阻断原因，不标已完成。
