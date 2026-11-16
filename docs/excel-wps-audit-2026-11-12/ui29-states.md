# UI-29.3 空 / 错误 / 加载状态的视觉

日期：2026-11-16　范围：`ui_kit::state_view` / `state_banner` / `loading_overlay` + 3 个真实调用点

## 1. 之前的问题

同一种「这里没内容」在三个地方三种写法：

| 位置 | 原来 | 问题 |
|---|---|---|
| 查找全部 0 处 | `ui.label("共 0 处（点击定位）")` + 空列表 | 看起来像还没加载完，不像「没有结果」 |
| 加载问题对话框 | `ui.colored_label(from_rgb(200,60,40), …)` | 颜色是就地取的，和后面新加的警告色不是一套；没有「几处致命」的层级 |
| 打开/重算中 | 只有状态栏一行字 | 大文件打开时窗口像卡死，用户会重复点保存 |

## 2. 现在的组件

```rust
pub enum StateKind { Empty, Error, Warn, Info }
pub fn state_color(kind) -> Color32        // 灰 / Fluent 红 #C42B1C / 琥珀 #9D5D00 / 绿
pub fn state_view(ui, kind, icon, title, detail) -> Response   // 居中：图标+标题+说明
pub fn state_banner(ui, kind, title, detail) -> Response       // 左对齐：3px 色条 + 文字
pub fn loading_overlay(ctx, title, detail, progress: Option<f32>) // 压暗 + 卡片 + 旋转弧 + 进度条
```

尺寸全部走 UI-29.1 的 token（`space::L/XL`、`icon::XL`、`text::TITLE/BODY/SMALL`），
没有新加字面量。

## 3. 接进了哪三处

| 调用点 | 触发条件 | 变化 |
|---|---|---|
| `find_all_window` | 查找全部命中 0 个 | 走 `state_view(Empty, Icon::Find, "没有找到匹配内容", …)`，不再显示「共 0 处」 |
| `load_diag_ui` | 打开有读取问题 | 顶部 `state_banner`：致命→Error「这个工作簿没有完整读入…已禁止覆盖保存」，非致命→Warn「打开时有读取问题（不影响保存）」 |
| `update` | `tasks.busy && open_rx.is_some()` | 全屏 `loading_overlay`：加载显示文件名 + 已等待秒数（不确定进度）；重算显示 `done / total` 与进度条，Esc 仍可取消 |

## 4. 验证

| 测试 | 断言 |
|---|---|
| `state_view_paints_title_detail_and_kind_color` | 标题/说明文本存在；标题用 `state_color(Error)`；Empty/Warn/Error 三色两两不等 |
| `state_banner_shows_title_and_kind_background` | 标题存在；底色 `#FFF9F0`；左侧 3px 色条存在 |
| `loading_overlay_draws_scrim_card_and_half_progress` | 压暗层 `from_black_alpha(40)`；>300×100 的白卡片；进度填充宽 = 轨道的 0.50±0.05 |
| `find_all_window_with_no_hits_shows_empty_state` | 走真实 `find_all_window` 路径，0 命中时出现「没有找到匹配内容」 |

egui 的 `Window` 第一帧只量尺寸（shape 全是 Noop），集成测试跑了两帧才拿到文字 —— 已写进注释。

截图（1280×800）：
- `state-empty.png`：查找全部 0 处 → 灰色查找图标 + 「没有找到匹配内容」+ 提示语
- `state-loading.png`：压暗整屏（左上角均值 192.8 vs 正常 243.7）+ 居中白卡片（中心 248.1）

两张图都是通过探针环境变量触发真实绘制路径截的：
`SC_FIND_ALL_EMPTY=1` 打开空查找窗口；`SC_DEMO_OVERLAY=1` 注入一次「正在打开 demo.xlsx」。

## 5. 边界

- **没有人工视觉评审**（`vision_analyze` 本轮连续超时）；结论来自形状断言和截图像素统计。
- 加载遮罩的截图是探针注入的状态，不是真的在打开 106MB 文件的中途截的 —— 真机上大文件
  打开耗时 2.7s（QA-07 基线），截图窗口太窄，没有强行去抢那一帧。
- 深色主题未定义状态色（与 UI-29.1 的边界一致）。
- 「已保存/已撤销」这类瞬时提示仍走状态栏文字，没有改成 toast；Excel 也是状态栏，未改。
