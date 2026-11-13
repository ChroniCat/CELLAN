# 恢复记录

## 2026-11-14 04:0x 监督复核后的检查点

- 分支 `main`，HEAD `723c6cd` 已 push。
- 台账：**已验收 24 / 61**，待验收 4（C07、UI-01、C15、C17），实施中 8，待实施 25。
  监督明确指出：28/61 是上一轮自报，尚未全认可；本次按复核意见把四项回退待验收，
  不以改测试期望或删需求消缺口。
- 本轮修复（都有实测/回归，见 `implementation-evidence/`）：
  - **C07 OFFSET**（`c07-dv-offset.txt`）：省略 height/width 继承 reference 尺寸；
    跨表 reference 不改变 COUNTA($A:$A) 的表上下文；负数 height 反向展开；reference 可名称。
    Excel 16.0 COM 实测 ROWS=3 / 跨表 ROWS=2 / OFFSET($A$3,0,0,-2,1)=$A$2:$A$3；
    夹具 `fixtures/c07/offset.xlsx` 往返 + 两条单测 + 两条集成测试。
  - **C15 选择权限**（`c15-selection-perms.txt`）：selectLockedCells/selectUnlockedCells
    在鼠标按下、拖拽扩选、方向键、名称框四个入口执行；回归
    `selection_permissions_block_jumping_into_forbidden_cells`。
  - **C17 Dxf 四边**（`c17-dxf-borders.txt`）：`DxfBorder.sides[4]` 逐边 style/color，
    解析/保存/绘制都按边；对角/内部/起止边标 `dxf_unsupported` 并在管理窗口提示；
    夹具 `fixtures/c17/borders.xlsx`，Excel 重开我们保存的文件四边颜色各自保留。
  - **UI-01**：按监督意见回退，not_done 明确「中文 IME 实机确认未执行，转 QA-04」。
- 测试基线（同一 cargo 串行，`--offline -j 2`，测试 `--test-threads=2`）：
  - sheetcore 全量 → 757 passed / 0 failed
  - app `--bin app` → 263 passed / 0 failed
  - `--test ui_regression` → 20 passed
- 真实 GUI 约束（监督新要求，长期有效）：不得用 `SC_*` 注入或截图替代实际键鼠/IME；
  本机 RDP 会话不可用，需要真实输入的验收（UI-01 IME、C15/C17 交互）一律留待验收，
  改用可在无界面验证的部分推进其它任务。
- 下一步：C16 打印/PDF（数字格式已共享，缺合并/换行/对齐/分页重复标题/图片/图表/
  预览与 PDF 一致），先做 `print` 共享显示模型 + 逐页 PDF 对拍；不触碰已验收项。
