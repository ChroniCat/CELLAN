# 修订历史可追踪性（QA-08）

日期：2026-11-14　复核 HEAD：`1a2a2c7`

## 为什么有这份文档

早期三份历史文档（`ui-parity-audit.md`、`native-v147-plan.md`、`native-rewrite.md`）
里的数字和“还没做”是**当时快照**。后续实现改变了结论，但旧文不能删（保留决策依据），
也不能直接拿来排期。这里给每份旧文标注快照边界和当前归属，避免把历史缺口当现状。

## 旧文档快照

| 文档 | 快照性质 | 旧结论 | 当前处理 |
|---|---|---|---|
| `docs/ui-parity-audit.md` | Excel UIA 初次逐控件快照，基线 `df48672` 前后 | 90/257，缺 167 | 文首已标注：90 是初次数，同轮收尾实为 123/257，两者都不是当前完成率 |
| `docs/native-v147-plan.md` | v1.47 三项提案（连续轮次，无单一 commit） | 正文“这三项都还没做” | 文首加历史方案声明 + 三项当前状态表（跨工作簿引用/修订日志/命名视图均已实现） |
| `docs/native-rewrite.md` | 按轮次的实现日志 | 按日期记载当时进度 | 保留为日志；进度条目不是任务清单，当前状态以台账为准 |

## 当前唯一状态源

- 任务清单、切片、状态、证据、文件、`not_done`、失败与阻塞：
  `docs/excel-wps-audit-2026-11-12/implementation-progress.json`
  （61 父任务 / 148 切片；状态仅：待实施/实施中/待验收/阻塞/已验收）。
- 每项的验收依据：`docs/excel-wps-audit-2026-11-12/core-requirements.md` 等合同文件，
  以及 `implementation-evidence/` 下的实测记录。
- 历史审计的差异条目只用于辨认控件，不再直接产生“缺失”结论；
  任何“缺失”必须能指到当前源码文件或一次当前实测。

## 已解决的具体矛盾

1. **90 vs 123**：`ui-parity-audit.md` 文首原只写 90/257，与 `native-rewrite.md:2795`
   同轮收尾的 123/257 冲突。现文首并列两个数并注明“初次/收尾，均非当前”。
2. **“已全部实现” vs “都还没做”**：`native-v147-plan.md` 标题与正文冲突。
   现文首声明正文是提案时状态，并列出三项各自的当前实现与证据入口。
3. **旧清单当需求**：旧文的 ✅/❌ 仅是控件存在性，不等于行为验收；
   现明确以台账状态与证据等级为准，避免重复实现已有模块。

## 复核方法

```text
rg -n "90/257|123/257|这三项都还没做|已全部实现" docs/
rg -n '"status"' docs/excel-wps-audit-2026-11-12/implementation-progress.json
```

## 旧日志的时间锚点（2026-11-17 补齐）

三份旧文（`ui-parity-audit.md`、`native-v147-plan.md`、`native-rewrite.md`）的每个
二~四级标题后都加了标记：

```text
〔改于 2026-11-06T17:28+08:00 / 2ffdbeb〕
```

- 时间 = 该标题行最后一次被改动的提交时间（`git blame` 的 committer-time，+08:00）；
- `2ffdbeb` = 那次提交的短 SHA，可直接 `git show 2ffdbeb -- <文件>` 定位；
- 标记加完后单独提交（本次），之后标题若再变，`git blame` 会指向新提交，标记即过时，
  需要按同样方法重刷——用脚本重刷：见本文件末尾命令。

这样「旧日志逐轮条目」不再只有日期，而是每条可追到具体提交与时刻；
当前状态仍以 `implementation-progress.json` 为准（时间锚点只解决可追踪性，不改结论）。

复核命令：

```bash
git blame -l --line-porcelain HEAD -- docs/native-rewrite.md | grep -A2 '^[0-9a-f]\{40\}'
rg -n "〔改于 " docs/
```
