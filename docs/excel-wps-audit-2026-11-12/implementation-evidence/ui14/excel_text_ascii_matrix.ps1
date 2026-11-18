$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open('F:\finance-native-sheets-ready\work\probe\text_ascii_matrix.xlsx')
$ws=$wb.Worksheets.Item(1)
$c=0
foreach($n in 1..12){
 foreach($sz in 8..24){
  $c++
  $ws.Columns.Item($c).ColumnWidth=8.43
  $ws.Columns.Item($c).AutoFit() | Out-Null
  Write-Output ("{0}`t{1}`t{2}" -f $n,$sz,$ws.Columns.Item($c).ColumnWidth)
 }
}
$wb.Close($false); $xl.Quit()
