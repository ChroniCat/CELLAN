$ErrorActionPreference='Stop'
$xl=New-Object -ComObject Excel.Application
$xl.Visible=$false; $xl.DisplayAlerts=$false
$wb=$xl.Workbooks.Open('F:\finance-native-sheets-ready\work\probe\style_file_matrix.xlsx')
$ws=$wb.Worksheets.Item(1)
$c=0
foreach($s in @('1','中文文字中','111111111111')){
 foreach($sz in 8..24){
  foreach($st in @('regular','bold','indent1','indent2','bold_indent1')){
   $c++
   $ws.Columns.Item($c).ColumnWidth=8.43
   $ws.Columns.Item($c).AutoFit() | Out-Null
   $cell=$ws.Cells.Item(1,$c)
   Write-Output ("{0}`t{1}`t{2}`t{3}`t{4}" -f $st,$s,$sz,$ws.Columns.Item($c).ColumnWidth,$cell.IndentLevel)
  }
 }
}
$wb.Close($false); $xl.Quit()
