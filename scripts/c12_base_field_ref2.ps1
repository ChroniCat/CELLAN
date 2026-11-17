$ErrorActionPreference='Stop'
$out = 'F:\finance-native-sheets-ready\work\probe\c12\base-field-ref2.xlsx'
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
try {
  $wb = $xl.Workbooks.Add()
  $ws = $wb.Worksheets.Item(1)
  $ws.Name = 'Data'
  $rows = @(
    @('地区','产品','金额'),
    @('华东','A',10), @('华东','B',20), @('华东','A',30),
    @('华北','A',40), @('华北','B',50), @('华北','A',60)
  )
  $data = New-Object 'object[,]' $rows.Count,3
  for ($r=0; $r -lt $rows.Count; $r++) {
    for ($c=0; $c -lt 3; $c++) { $data[$r,$c] = $rows[$r][$c] }
  }
  $ws.Range('A1:C7').Value2 = $data
  $pch = $wb.PivotCaches().Create(1, $ws.Range('A1:C7'))
  $pt = $pch.CreatePivotTable($ws.Range('E3'), 'PT1')
  @($pt.PivotFields('地区'))[0].Orientation = 1   # xlRowField
  @($pt.PivotFields('产品'))[0].Orientation = 2   # xlColumnField
  $pt.AddDataField(@($pt.PivotFields('金额'))[0], '求和项:金额', -4157) | Out-Null
  "datafields count=$($pt.DataFields.Count)"
  $df = $pt.DataFields(1)
  "df name=$($df.Name)"
  $df.Calculation = 2
  $wb.SaveAs($out, 51)
  "saved $out"
  $wb.Close($false)
} finally { $xl.Quit() }
