# 本地补丁说明

来源：crates.io `accesskit_windows 0.24.1`（egui 0.31.1 的依赖），原样 vendoring。
许可：MIT OR Apache-2.0（见同目录 LICENSE-MIT / LICENSE-APACHE）。

只改两处，目的：让 UIA 暴露网格的选区语义（UI-24.3）。

1. `src/node.rs`：`is_selection_item_pattern_supported` 额外放行
   `Role::Cell` / `Role::RowHeader` / `Role::ColumnHeader`。
   上游 0.24.1 只认 ListItem/Tab 等；accesskit 0.17 没有 `GridCell` 角色，
   表格单元格就是 `Cell`，不放行则 UIA 客户端拿不到 `SelectionItemPattern`。

2. `src/adapter.rs`：`node_added` 里，新节点自身就是选中态时补发
   `UIA_SelectionItem_ElementSelectedEventId`。
   egui 手工建的单元格节点不会挂进 Grid 的 children，容器级 diff 派发不到事件；
   活动格换地址时节点 id 变化，正好在这里发一次选中事件。

升级 egui/accesskit 后可删除此 vendor 与根 Cargo.toml 的 `[patch.crates-io]`，
改用上游 `Role::GridCell` 的 SelectionItemPattern 实现。
