$ErrorActionPreference='Stop'
$pairs = @(
  @('F:\finance-native-sheets-ready\crates\core\tests\fixtures\c16\print.xlsx','F:\finance-native-sheets-ready\work\probe\c16\print-excel.pdf'),
  @('F:\finance-native-sheets-ready\crates\core\tests\fixtures\c16\objects.xlsx','F:\finance-native-sheets-ready\work\probe\c16\objects-excel.pdf')
)
New-Item -ItemType Directory -Force -Path 'F:\finance-native-sheets-ready\work\probe\c16' | Out-Null
$xl = New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
foreach ($p in $pairs) {
  if (Test-Path $p[1]) { Remove-Item $p[1] -Force }
  $wb = $xl.Workbooks.Open($p[0])
  $wb.ExportAsFixedFormat(0, $p[1])
  "{0} -> {1} pages_ok={2}" -f (Split-Path $p[0] -Leaf), (Split-Path $p[1] -Leaf), (Test-Path $p[1])
  $wb.Close($false)
}
$xl.Quit()
