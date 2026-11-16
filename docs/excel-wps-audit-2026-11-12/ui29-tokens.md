# UI-29.1 设计 token 与跨 DPI 布局基线

日期：2026-11-16　范围：`crates/app/src/tokens.rs`、`ui_kit.rs` 共享组件

## 1. 为什么要有 token

此前间距/圆角/描边/字号散落在 `ui_kit.rs`、`ribbon/ui.rs`、`main.rs` 的字面量里：
同一个「图标和文字之间」在功能区是 6、在标签页是 4、在对话框是 5，改一类要全局搜；
也没法回答「这套 UI 是 4 的倍数栅格吗」。UI-29.1 把这些收进一个文件，并给出
可机械校验的约束（2pt 栅格、固定四档图标、跨 DPI 严格按倍率）。

## 2. token 表（逻辑点，DPI 由 `pixels_per_point` 统一乘）

| 组 | 名字 | 值 | 用途 |
|---|---|---|---|
| space | HAIR | 2 | 半分：图标与标签、分隔线两侧 |
| space | XS | 4 | 控件内部 padding、分组顶部 |
| space | M | 6 | 图标与文字、分组内相邻控件 |
| space | L | 8 | 控件之间、行距 |
| space | XL | 12 | 分组之间、对话框内容边距 |
| space | XXL | 16 | 对话框分节 |
| space | XXXL | 24 | 窗口级留白 |
| space | GROUP_LABEL_GAP | 7 | 分组名与命令区（Excel 实测 6+1） |
| space | GROUP_PAD_X / GROUP_PAD_TOP | 6 / 4 | 分组内边距（Excel 实测） |
| radius | SM / MD / LG | 2 / 3 / 4 | 小控件 / 输入框 / 对话框 |
| stroke | HAIR / FOCUS | 1 / 1.5 | 常规线 / 焦点圈 |
| text | SMALL / BODY / TITLE | 11 / 12 / 13 | 次要 / 正文 / 标题 |
| size | BTN_MIN_W / BTN_H | 75 / 24 | 对话框按钮（Excel 同尺寸） |
| size | SHEET_TAB_H / SHEET_TAB_PAD | 32 / 26 | 工作表标签 |
| icon | XS / S / M / L / XL | 12 / 16 / 20 / 24 / 32 | 启动器斜箭头 + 四档图标 |

已有常量继续作为 token 的「结构值」：`TITLE_H 48`、`TAB_H 30`、`RIBBON_BODY_H 102`、
`FX_H 44`、`GROUP_BODY_H 76`、`GROUP_LABEL_H 14`。

## 3. 已接入 token 的组件

| 组件 | 接入的 token |
|---|---|
| `ui_kit::group`（功能区分组） | 分隔线 1px、顶/左内边距 4/6、组内 2pt 间距、组名间隙 7、组名 12pt |
| `ui_kit::launcher_at`（分组对话框启动器） | 图标 12、描边 1 |
| `ui_kit::primary_button` / `secondary_button` | 字号 12、描边 1、圆角 2、75×24、焦点圈 1.5 |
| `ui_kit::pill` | 字号 12、高 24、左右 12、圆角 3 |
| `ui_kit::sheet_tab` | 字号 12、高 32、左右 26 |
| `ui_kit::big_sized` / `color_button` / 字体样式切换 | 焦点圈 1.5（QA-05.3 已接） |

**未迁移**：`main.rs` 里标题栏、状态栏、对话框各自的 padding 仍是字面量（这轮不假装
全仓统一）。这些属于同值不同名的重复，下一轮按「标题栏 → 状态栏 → 对话框」逐类替换，
每类都要重跑本文件的跨 DPI 测量。

## 4. 跨 DPI 测量（真窗口，UIA 实测）

同一逻辑布局（1280×800 点），物理窗口 = 1280×ppp × 800×ppp，逐元素比对
`uia_dump.ps1` 的矩形：

| 档位 | 物理窗口 | 可比元素 | 达标 | 偏离 |
|---|---|---|---|---|
| 100% | 1280×800 | 基线 | — | — |
| 125% | 1600×1000 | 101 | 101 | 0 |
| 150% | 1920×1200 | 101 | 101 | 0 |
| 200% | 2560×1600（被屏幕裁到 1440 高） | 55 | 55 | 0（网格类 41 个因裁切排除） |

判定口径：盒子类 `|w2 − w1×ppp| ≤ max(2.5, 4%)`；文本节点放宽到 6%（字形整像素取整，
200% 下「seed」31→59 而不是 62，差 3px）。

样例（100% → 150%）：粘贴按钮 42×64 → 63×96；字体框 147×20 → 220×30；
名称框 76×20 → 114×30；工作表标签 Sheet1 64×32 → 97×48；状态栏「计数: 1」32×13 → 50×20。

截图：`implementation-evidence/ui29-100.png`（1280×800）、`ui29-150.png`（1920×1200）。

## 5. 这轮抓到的真缺陷

**可访问矩形被乘了两次 `pixels_per_point`。** `a11y_name_rect`、网格 `Role=Grid`、
行列 `RowHeader/ColumnHeader` 三处都把 egui 逻辑点再乘了一次 ppp，而 egui 的
accesskit 树本身已经乘过。后果：100% 下完全正常（ppp=1），125%/150%/200% 下
读屏拿到的矩形是应有尺寸的 ppp 倍 —— 例如 150% 时粘贴按钮报 94×144（应为 63×96），
网格报 2841×1066（比窗口还宽）。UI-24.2 当时只在 100% 下验过，所以没暴露。

修法：三处直接传逻辑点，让 egui 统一换算。修完后四档测量全部按倍率走（上表）。

## 6. 复跑方法

```bash
for p in 1.0 1.25 1.5 2.0; do
  w=$(python -c "print(int(1280*$p))"); h=$(python -c "print(int(800*$p))")
  powershell -File work/probe/qa07/uia_dump.ps1 -File .../fixture.xlsx \
    -Tag "u29-$p" -Ppp $p -WinW $w -WinH $h > work/probe/qa05/u29-$p.txt
done
python work/probe/ui29/compare_scaling.py work/probe/qa05/u29-1.0.txt 1.25 1.5 2.0
```

`compare_scaling.py` 的阈值就是上表口径；改 token 后这个脚本必须仍然 PASS。

## 7. 边界

- 200% 档受本机屏幕（2560×1440）限制，窗口高度被裁到 1440，网格类元素不可比，已单列排除。
- 只测浅色主题；深色主题的 token 尚未定义（`ui_kit` 里仍有 `dark_mode` 分支的临时色）。
- 文本节点容差 6% 是字形取整造成的，不是布局误差；盒子类没有放宽。
- 未做三端（Excel/WPS/本应用）同场景截图对比 —— 那是 UI-29 父项的验收，需要 Excel/WPS
  侧的可控截图通道（本机无交互桌面，窗口截图全白），留给 UI-29.3 之后的父项收口。
