# UI-24.2 二级菜单 a11y：性质更正 + 读取通道落地

## 结论先说

「二级菜单命令列表不在 accesskit 树」**不是控件缺陷**，而是当时的扫描没有展开菜单。
现在有了读取通道，实测：**菜单展开后，二级命令确实进树**，名称完整。

## 读取通道

`crates/app/src/edit_regression_tests.rs`：

- `a11y_nodes(app, ctx)`：`ctx.enable_accesskit()` → 跑一帧 → 取 `platform_output.accesskit_update`，
  输出 `(role, label, bounds)` 列表；必须先 `install_fonts(&ctx)`，否则 `FontFamily::Name("latin")` panic。
- `a11y_tree_reads_ribbon_group_names`：首屏树 **198 个节点**，带名称的按钮里能看到
  `条件格式`、`套用表格格式`、`排序和筛选`、`单元格样式 展开`、`粘贴 选项`、`排序和筛选 选项` 等。
- `a11y_secondary_menu_items_appear_when_open`：按 `条件格式` 的 bounds 注入
  `PointerMoved` + `PointerButton`(按下/抬起)，跑两帧展开菜单，再读树：

```
after open: 104 labelled nodes
  MENU | 管理规则…
  MENU | 快捷规则…
  MENU | 新建规则…
```

三条二级命令全部带名称，断言通过。两次用例：`2 passed`。

## 仍未覆盖

- **split 按钮 / 画廊**（「套用表格格式 选项」「单元格样式 展开」「排序和筛选 选项」）：
  点击热点是箭头子区域，不是按钮整体；用整体中心点点击不展开（实测 `101 -> 101`），
  需要按 `Response` 的箭头 rect 单独定位，或直接调用对应 popup 的开关状态。属独立小任务。
- **真实屏幕阅读器**（NVDA/讲述人）未测：环境无交互桌面，UIA 客户端连不上。

## 台账口径

UI-24.2 从「二级菜单缺标签（真实缺口）」改为「读取通道已建立；条件格式二级菜单已验证；
split/gallery 组与真实屏幕阅读器未验」——不抬高为已验收。
