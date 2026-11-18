# UI-11.2 函数候选与参数提示（物理键盘，2026-11-18）

真实窗口：`ui09_keys.xlsx`，`SC_INSTANCE_PORT=49981`、`SC_SEL=0,0`、1440×900。
输入链路：`computer_use type` 逐字符送入真实窗口；候选选择用
`ui09_force.ps1 -Key DOWN/ENTER`（`keybd_event` 扫描码）。

## 1. 输入 `=SU`

逐字符键入 `=`、`S`、`U`，公式栏读数为 `=SU`，候选框出现 8 项：

```text
SUM 求和
SUMIF 条件求和
SUMSQ 函数
SUMIFS 多条求和
SUMXMY2 函数
SUBTOTAL 函数
SUMX2MY2 函数
SUMX2PY2 函数
```

截图：`ui112-candidates.png`。

## 2. 键盘选择

当前高亮第一项 SUM。按一次 Down（真实扫描码）：

```text
FG=21376934 BEFORE=A1，S1-A1，已选中 AFTER=A1，S1-A1，已选中 TABS=*S1,S2,S3
```

按 Enter 接受候选，UIA 回读：

```text
A1，S1-A1，已选中
EDIT==SUMIF(
```

说明：Down 从 SUM 移到第二项 SUMIF，Enter 接受后公式为 `=SUMIF(`，光标在末尾。

## 3. 参数签名

`=SUMIF(` 状态下截图 `ui112-signature.png`，候选框显示：

```text
SUMIF(range,criteria,[sum_range])
```

## 4. 接受后光标在末尾

在 `=SUMIF(` 状态下继续键入 `A1`，UIA 回读：

```text
EDIT==SUMIF(A1
```

追加发生在末尾，不是插到括号前，证明接受候选后光标在公式末尾。

## 5. 收尾

按 Esc 取消编辑，未保存该临时输入；关闭 app。
