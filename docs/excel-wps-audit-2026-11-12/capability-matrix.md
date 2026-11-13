# 能力支持矩阵（C21）

本文件是「用户在执行前能辨认能力」的依据：按钮存在不等于能力等价。
更新于 2026-11-14，对应 `main` 分支 HEAD 附近的实现。

## 宏

| 能力 | 本应用 | 说明 |
| --- | --- | --- |
| 查看/保留 VBA 工程 | ✅ | `xl/vbaProject.bin` 原字节保留；改普通格保存后字节不变（测试 `c21_parts::vba_project_survives_editing_a_normal_cell`） |
| 内置宏引擎执行 VBA | ❌ | 不提供。界面上没有任何「运行」入口是应用自己解释 VBA 的 |
| 转交本机 Excel 执行宏 | ⚠️ 显式操作 | 菜单「用本机 Excel 执行宏…」，需用户填宏名并确认；弹窗注明「本应用不内置宏引擎」。没有本机 Excel 时不可用 |
| 打开文件时自动执行宏 | ❌ | 不执行 `Auto_Open`/`Workbook_Open`；加载只读数据 |

**另存策略：** 含宏工作簿另存为 `.xlsx` 时提示会丢宏，并改存 `.xlsm`（`main.rs` 保存路径已有提示）。

## 外部数据 / 查询

| 能力 | 本应用 | 说明 |
| --- | --- | --- |
| 导入 CSV/文本为工作表 | ✅ | 一次导入，不建立刷新链路 |
| 生成 Excel 原生查询表（queryTable + connection） | ✅ | 保存后 Excel「数据 → 查询和连接」可见；仅覆盖本应用生成的连接 |
| 读取/执行 M / Power Query | ❌ | 不解析 M 公式，不做查询折叠、多表连接、参数化刷新 |
| 刷新既有外部数据连接 | ❌ | 打开别人的工作簿时既有连接原样保留，但应用内不能刷新 |

## 预测

| 能力 | 本应用 | 说明 |
| --- | --- | --- |
| 线性回归外推 | ✅ | 最小二乘；输出期/原值/拟合值/预测值 + 回归参数；界面标题为「线性预测工作表」 |
| ETS / 季节性 / 置信区间预测 | ❌ | 不等同 Excel 的 FORECAST.ETS 系列 |

## 快速填充

| 能力 | 本应用 | 说明 |
| --- | --- | --- |
| 规则快速填充 | ✅ | 从示例识别有限规则（拆分/拼接/大小写/序号等）后批量填 |
| 通用程序合成（Excel Flash Fill 级别） | ❌ | 界面与状态栏均标为「规则快速填充」，不宣称完全等价 |

## 未建模部件的保存行为

| 部件 | 保存后 |
| --- | --- |
| `xl/vbaProject.bin` | 字节保留 |
| `xl/threadedComments/*`、`xl/persons/*` | 从基准文件原字节恢复（测试 `c21_parts::threaded_comment_parts_survive_editing_a_normal_cell`） |
| 线程批注工作簿里的 `xl/comments*.xml` 镜像 | 同上，原字节恢复（该场景下应用内不编辑批注） |
| `customXml/*` | 原字节保留，同一测试断言；Excel 回读 CustomXMLParts 仍含该部件 |
| 既有 queryTable / connections | 原字节保留（测试 `c21_parts::query_table_and_custom_xml_survive_editing_a_normal_cell`；Excel/WPS 回读 QueryTables=1） |

## 不使用「完全兼容」

界面与文档中不再出现「完全兼容」字样；能力差异按上表命名。
