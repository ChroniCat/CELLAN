# 复现材料

这些是审计诊断材料，不是已修复功能，也不是完整回归框架。使用合成数据，不打开真实业务文件。核心探针打印当前实际结果，exit0只表示探针运行完成，不表示兼容正确；正确结果见主报告和验收任务。

## 文件

| 文件 | 用途 |
|---|---|
| core_probe.rs | 数组形状、引用、日期、DV与字符串预处理；可传1904合成夹具路径 |
| core_errors.rs | 错误增量传播、空值比较、数组广播、透视空值计数 |
| core_features.rs | 结构化引用、自定义属性保留、CSV刷新；仅写target/audit-core下的合成文件 |
| ui_edit_probe.rs | 附加到app现有测试模块的egui事件探针；按正确行为断言，基线故意失败 |
| run_office_oracle.py | 在本机Excel/WPS新建临时工作簿，获取对拍真值；不打开已有用户文件 |
| make_date_fixture.py | 纯标准库创建最小date1904的OOXML测试包，不依赖Office安装 |
| make_baseline.py | 用Excel COM生成本轮截图所用纯合成表，输出到指定新路径 |

## 核心诊断运行

先从仓库根目录运行当前源码构建，隔离环境按Hermes指南处理。以下仅创建/覆盖审计专属target/audit-core输出；不要把该目录用于存放任何业务文件。

```powershell
$cargoExe = 'C:\Users\Administrator\.cargo\bin\cargo.exe'
$rustcExe = 'C:\Users\Administrator\.cargo\bin\rustc.exe'
& $cargoExe build -p sheetcore --offline
New-Item -ItemType Directory -Force target\audit-core | Out-Null
python docs\excel-wps-audit-2026-11-12\repro\make_date_fixture.py --out target\audit-core\date1904-new.xlsx
$coreLib = Get-ChildItem target\debug\deps\libsheetcore-*.rlib |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
foreach ($probeName in @('core_probe', 'core_errors', 'core_features')) {
    & $rustcExe --edition 2021 ("docs\excel-wps-audit-2026-11-12\repro\$probeName.rs") `
        --extern ("sheetcore=" + $coreLib.FullName) -L dependency=target\debug\deps `
        -o ("target\audit-core\$probeName.exe")
    if ($LASTEXITCODE -ne 0) { throw 'Probe compilation failed' }
    & ("target\audit-core\$probeName.exe") target\audit-core\date1904-new.xlsx
}
```

若target中有多个feature组合的rlib，必须选择刚构建得到的当前库，不能将最旧的探针exe当当前结论。当前实现修复后，诊断输出应发生预期变化；需迁移为正式断言测试。

## Office对拍

需要本机已安装Excel/WPS及Python pywin32：

```powershell
python docs\excel-wps-audit-2026-11-12\repro\run_office_oracle.py --out work\office-oracle-new.json
python docs\excel-wps-audit-2026-11-12\repro\make_baseline.py --out work\baseline-new.xlsx
```

生成器拒绝覆盖已存在的输出文件。只关闭自己新建的临时工作簿，不退出可能包含用户文件的WPS实例。不同WPS版本的COM注册名可能不同，失败要报告，不要擅自操作现有文档替代。当前基准使用了Excel生成的同一份样本并在WPS只读打开。

原始COM日期 `.Text`受区域影响，Excel的`2/1/1904`与WPS的`1/2/1904`不应凭字符串判为日期不同；ISO复核在 `office-oracle-iso.json` 明确为1904-01-02。

## 输入提交探针

`ui_edit_probe.rs`依赖现有 `crates/app/src/edit_regression_tests.rs` 的测试帮助函数，不能独立rustc编译。审计时临时追加唯一测试后运行，再精确删除追加内容；没有修改其他测试。实现阶段建议把它整理为正式Enter/Tab分别测试，并增加Shift、Esc、F2、验证失败等边界。

诊断结果：提交前缓冲为Some("123")；提交后A1仍Empty、dirty=false、undo=(0,0)，选区已移动。关键输出保留在 [ui-edit-probe.txt](../evidence/ui-edit-probe.txt)。这是egui事件级证据，不等于中文IME或所有Windows输入链路已验收。

## 截图复现

项目支持 `SC_SHOT`、`SC_SHOT_EXIT=1`、`SC_SHOT_DELAY_MS=700`、`SC_WIN_W/SC_WIN_H`、`SC_SEL=1,1`、`SC_TAB`、`SC_OPEN_DIALOG` 等显式审计参数。启动前先清空继承的所有SC_*，设置独立 `SC_INSTANCE_PORT` 和LOCALAPPDATA。用新生成的baseline副本；完成后只关闭本轮记录的进程。

这些参数能复现界面状态，不能证明菜单可点击或快捷键已接通。必须另用真实输入验证相应入口。Office截图可通过自己的临时工作簿窗口PrintWindow取得；WPS COM的Hwnd可能是子窗口，应取顶层窗口并等待布局稳定。
