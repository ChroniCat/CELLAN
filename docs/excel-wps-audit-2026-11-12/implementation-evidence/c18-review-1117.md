# C18 复核（无监督，机检 + 双端回读）

原台账 C18.1~C18.3 都停在「等待复核」。监督已取消，这次由我按原合同逐条机检，证据如下。

## 1. 主题色 / tint（C18.1）

夹具 `c18/theme.xlsx` → 我们只改 B1 数字格式再保存 → `sc_c18_theme_edit.xlsx`，双端读 A1：

```
EXCEL theme FONT_THEME=5 FONT_TINT=0.399975585192419 FILL_THEME=6 FILL_TINT=-0.249977111117893 BOLD=True
WPS   theme FONT_THEME=5 FONT_TINT=0.399975597858429 FILL_THEME=6 FILL_TINT=-0.249977111816406 BOLD=True
```

两个产品读数一致（tint 尾差是 float32 存储），说明「改无关属性不丢主题链接」。

## 2. 图案填充 / 对角线（C18.2）

专用脚本 `scripts/c18_pattern_verify.ps1`（第二次保存产物 `sc_c18_pattern_rt2.xlsx`）：

```
EXCEL: PATTERN=-4124 DIAG_DOWN=1 DIAG_COLOR=0000FF FMT=0.000
WPS:   PATTERN=-4124 DIAG_DOWN=1 DIAG_COLOR=0000FF FMT=0.000
```

## 3. 复杂边框 / 填充 / 字体 / 对齐 / 自定义格式（C18.2）

新造夹具（openpyxl 写入，含 double/thick/dashed/mediumDashDot/**对角线 dotted**、
darkTrellis 前景+背景、宋体 15 粗斜体双下划线、居中/顶端/自动换行/旋转 30°、
`#,##0.00_);[Red](#,##0.00)`），我们往返一次后用独立读取器逐属性比对：

```
A1 same   B1 same   C1 same   D1 same   E1 same   （0 处差异）
```

双端打开：

```
Excel.Application open OK rows=1 A1_diag=-4118 E1_fmt=[#,##0.00;[红色]-#,##0.00]
KET.Application   open OK rows=1 A1_diag=-4118 E1_fmt=[#,##0.00_);[Red](#,##0.00)]
```

## 4. 未建模样式（C18.3）

- **quotePrefix**：改 B1 数字格式后保存，A1 仍是 `PrefixCharacter='`；
  Excel 自己再存一次后 `<xf ... quotePrefix="1">` 仍在且 A1 指向它（`s=2`）。
  之前探针打印 `QuotePrefix=` 空值是 PowerShell late-binding 假象（同 `.Item(1)` 那类坑）。
- **pivotButton / extLst / dxf**：`qa01/ext/pivot.xlsx` 往返前后计数一致
  （`pivotButton` 1→1、`<extLst` 1→1、`dxf` 1→1）。

## 5. 连续两次保存

`c18_pattern` 与 `c18_theme` 用例本身就写两次（`_rt`、`_rt2`/`_edit`），均通过：
`cargo test -p sheetcore --test c18_theme --test c18_pattern --test c18_unknown` → **2 + 3 + 3 passed**。

## 结论

C18 原合同逐条有据，切片 C18.1 / C18.2 / C18.3 与父项 C18 可判为已验收。
不在本合同内的部分保持不变：屏幕/打印不绘制图案与对角线（合同限定「样式模型/序列化」），
字体字形与换行由 UI-14 负责。
