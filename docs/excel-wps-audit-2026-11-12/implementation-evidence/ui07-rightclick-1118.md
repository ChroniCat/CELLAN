# UI-07.1 物理右键粘贴（2026-11-18）

## 结论

物理硬件鼠标右键在 A1 上打开真实上下文菜单，菜单项 `粘贴` 的 UIA 矩形为
`349,372,42,64`；先 `Focus-App` 再用物理左键点 `370,404`，系统剪贴板文本
`PASTE_RCLICK2` 写入 A1，UIA 回读 `A1，PASTE_RCLICK2，已选中`。

物理键盘 Ctrl+V 仍未通过：注入的 V 事件能到 winit（早期日志 `v=true`），但
`i.modifiers.command=false`，`clip_paste` 不触发；不把这一半写成已验。

## 现场命令与读数

```text
# 启动 c10/table.xlsx 副本，SC_SEL=0,0
SC_SEL=0,0 target/debug/app.exe work/probe/ui07_menu.xlsx

# UIA 几何
A1，Amount，已选中 | 323,515,1423,574
列 A，已选中 | 352,515,82,19
第 1 行，已选中 | 323,534,29,19

# 物理右键：mouse_event RIGHTDOWN(0x0008) / RIGHTUP(0x0010)，屏幕 393,543
右键后 UIA：
  粘贴 | ControlType.Button | 349,372,42,64
  粘贴 选项 | ControlType.Button
  剪切（Ctrl+X） | ControlType.Button
  条件格式 | ControlType.Button

# 设置剪贴板后，Focus-App + 物理左键 370,404
Set-Clipboard -Value 'PASTE_RCLICK2'
ACTIVE=A1，PASTE_RCLICK2，已选中
```

先前用 UIA `InvokePattern.Invoke()` 点同一项也得到 `A1，PASTE_RCLICK`，
说明菜单项动作本身可用；本轮新增的是**物理右键 + 物理左键**路径。

## 残余

- 物理键盘 Ctrl+V：合成 Ctrl 的 modifiers 不进入 egui，`cmd=false`；仍需真实硬件键盘/交互桌面补测。
- 物理右键菜单的其它项（剪切/粘贴选项/格式）未逐项点选。
