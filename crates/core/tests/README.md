# 测试数据与隔离

`data/` 中的工作簿是随仓库提供的测试夹具。`compat_in.xlsx` 是 OpenPyXL 生成的模拟数据；`two_sheets.xlsx` 是 Excel 生成的两张空白测试表，作者与本机路径元数据已清理。

部分旧回归测试依赖未随仓库提供的、具有特定内容和格式的样本。它们明确标记为 `ignored`，默认测试不会把缺少样本算作成功。若需要运行其中某项，在对应 crate 的 `tests/test-work/` 下准备符合测试断言的**合成样本**，然后使用 `cargo test -p sheetcore --test <测试文件名> <测试名> -- --ignored`（应用 UI 测试的包名是 `app`）。缺少样本会明确失败。不要在此目录放生产文件：测试会在这里生成、覆盖及删除输出文件。

核心测试可能需要 `array.xlsx`、`table.xlsx`、`notes.xlsx`、`cf.xlsx`、`icons.xlsx`、`dv.xlsx`、`styled.xlsx`、`freeze.xlsx`、`page.xlsx`、`chart.xlsx`、`table_obj.xlsx` 或 `vba_fixture2.bin`，具体见对应测试；应用 UI 测试使用其自身 crate 目录下的 `tests/test-work/`，可能需要 `table.xlsx`、`chart.xlsx`、`icons.xlsx`、`pic.xlsx`。只放同名文件不足以满足断言，内容和格式也必须匹配。

`pivot_parts_are_well_formed` 是单独的历史遗留情况：它依赖另一项测试留下的中间文件，而非自行生成输入，因此保持忽略，需先重写为独立测试。

一些测试使用系统临时目录，并会覆盖或删除其中固定名称的测试文件。运行测试前请将 `TEMP` 和 `TMP` 指向专门为本次运行新建的空目录。UI 测试会启动程序窗口；应用网络单元测试会在测试端口创建网络监听，应在合适的隔离环境中运行。
