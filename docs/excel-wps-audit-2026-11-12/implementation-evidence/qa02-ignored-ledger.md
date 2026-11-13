# QA-02.3 剩余 ignored 清单与解锁条件

生成时间：2026-11-13 20:20 CST
当前代码版本：main（`7493688` 之后）

## 实际结果

```text
cargo test -p sheetcore --offline
-> 77 bins, 721 passed, 0 failed, 9 ignored, exit 0
cargo test -p app --offline --test ui_regression
-> 20 passed, 0 failed, 0 ignored
cargo test -p app --offline --bin app
-> 232 passed, 0 failed, 0 ignored
```

日常 suite 不再依赖 `crates/core/tests/test-work/` 或 `crates/app/tests/test-work/`
中的散落文件；core 的 CF/DV/PDF/pivot/table/style/refs/filter 和 app 的
grid/icons/compare/textbox/subtotal/sparkline/pictures 都是仓库内合成夹具。

## 保留的 9 个 ignored（全部是 core 非日常项）

| # | 测试 | 分类 | 原因 | 责任人 | 解锁步骤 |
|---:|---|---|---|---|---|
| 1 | `comparison_semantics::make_c05_fixture` | optional generator | 需要 Excel/WPS 实机生成 C05 对拍输入；生成动作不应污染日常 suite | 主实施者 | 在隔离 scratch 目录运行 `cargo test -p sheetcore --test comparison_semantics -- --ignored make_c05_fixture`，确认只写 scratch，再把产物哈希登记到 C05 证据；不需要每天跑。 |
| 2 | `custom_props::make_c08_fixture` | optional generator | 人工驱动的 Excel/WPS 自定义属性对拍生成 | 主实施者 | 明确指定 Office 版本与 scratch 输出目录，运行 `--ignored` 生成器，人工核对后归档。 |
| 3 | `custom_props::excel_roundtrip_keeps_custom_props` | external COM | 需要先用 Excel/WPS 另存出目标文件；无 Office 的 CI 没有该前置 | 主实施者 | 先生成并保存目标样本，再在装有 Excel/WPS 的机器上运行；失败必须显示而非跳过。 |
| 4 | `date_system::make_excel_fixtures` | optional generator | 人工驱动的日期体系对拍夹具生成 | 主实施者 | 在隔离 scratch 运行生成器并登记哈希；不再写 `tests/test-work`。 |
| 5 | `date_system::xlsx_from_excel_1900` | external COM | 需要 Excel 另存文件 | 主实施者 | 用 Excel 生成 1900 体系样本，按测试注释路径放入 scratch，再运行。 |
| 6 | `date_system::xlsx_from_excel_1904` | external COM | 需要 Excel 另存文件 | 主实施者 | 用 Excel 生成 1904 体系样本，按测试注释路径放入 scratch，再运行。 |
| 7 | `date_system::xlsx_from_wps_1900` | external COM | 需要 WPS 另存文件 | 主实施者 | 用 WPS 生成 1900 体系样本，按测试注释路径放入 scratch，再运行。 |
| 8 | `save_failure_injection::killed_during_save_child` | platform helper | 由父测试启动的辅助进程，不是独立用例；单独运行没有意义 | 主实施者 | 只通过父测试 `killed_during_save_leaves_the_original_intact` 调用，不直接解锁。 |
| 9 | `vba_source::reads_modules_from_fixture` | external sample | 需要真实 OLE VBA 容器 `tests/test-work/vba_fixture2.bin`（2 个模块）；当前仓库只有不同模块结构的 `real_macro.xlsm`，不能用它替换断言 | 主实施者/用户提供样本 | 提供真实 OLE VBA `.bin`（Module1 `Sub Hello()`/`MsgBox "hi"`，Module2 `Function Add(a,b)`）后复制到 scratch 夹具目录，去 ignore 运行；缺文件时测试会明确失败。 |

## 发布口径

- **optional generator**：不计入日常 suite 的通过数；生成动作单独登记。
- **external COM**：只在有对应 Office 的机器执行；没有执行时保留 ignored 及原因，不写成通过。
- **platform helper**：由父测试间接执行；单独 ignored 不是缺测试。
- **external sample**：`vba_source` 仍缺真实样本；这是 QA-02.3 追踪项，不是已验收功能。

本清单不改变任何父任务/切片的验收状态；QA-02.1 已提交给监督复核，QA-02.2/02.3 仍待验收。
