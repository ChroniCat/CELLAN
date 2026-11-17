# C12.4 显示值方式：基准字段（baseField/baseItem）核对

## 目的

台账里一直写着「baseField/baseItem 非默认基准未做」，但没确认过**默认基准**我们写得对不对。
这次用 Excel 自己造的参照样本核对。

## 参照样本（Excel COM 生成）

`scripts/c12_base_field_ref.ps1`：地区=行、产品=列、值=求和项:金额，数据字段 `Calculation=xlDifferenceFrom`。

未指定基准时 Excel 写：

```xml
<dataField name="求和项:金额" fld="2" showDataAs="difference" baseField="0" baseItem="0"/>
```

显式指定 `BaseField='产品'`、`BaseItem='A'` 时 Excel 写：

```xml
<dataField name="求和项:金额" fld="2" showDataAs="difference" baseField="1" baseItem="0"/>
```

（`baseField` 是 pivotFields 序号：0=地区、1=产品、2=金额；`baseItem` 是该字段 sharedItems 里的项序号。）

## 我们的输出

`pivot_parts2.rs` 写的是 `baseField="0" baseItem="0"` —— 与 Excel **默认基准**逐字节一致。
所以「显示值方式」这一组在默认基准下没有偏差；缺的只是**让用户挑自定义基准**这个功能。

## 结论

| 项 | 状态 |
|---|---|
| 默认基准（与上一项差、占行/列/总计%） | ✅ 与 Excel 一致（本次核对） |
| 自定义基准字段/项（如「以产品 A 为基准」） | ❌ 未做（属功能扩展，不是缺陷） |

要补自定义基准，需要在 spec 里加「基准字段 + 基准项」并让 UI 能选；
`baseField` 用 pivotFields 序号、`baseItem` 用 sharedItems 序号，上面两条样本就是格式依据。

## 附带修正一条记录

生成样本时再次踩到老坑：PowerShell 里 `$pt.DataFields.Item(1)` 会报
「PSParameterizedProperty does not contain a method named 'Item'」，必须写 `$pt.DataFields(1)`。
这与之前「`$ws.PivotTables.Item(1)` 导致 pivotCacheDefinition 缺版本属性」的探针假象是同一类问题。
