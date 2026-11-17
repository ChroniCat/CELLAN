$p='F:\finance-native-sheets-ready\work\probe\c18\complex-out.xlsx'
foreach ($id in @('Excel.Application','KET.Application')) {
  $app = New-Object -ComObject $id
  try { $app.Visible=$false; $app.DisplayAlerts=$false } catch {}
  try {
    $wb=$app.Workbooks.Open($p)
    $ws=$wb.Worksheets.Item(1)
    $a=$ws.Range('A1'); $e=$ws.Range('E1')
    "{0} open OK rows={1} A1_diag={2} E1_fmt=[{3}]" -f $id, $ws.UsedRange.Rows.Count, $a.Borders.Item(5).LineStyle, $e.NumberFormat
    $wb.Close($false)
  } catch { "{0} ERR {1}" -f $id, $_.Exception.Message } finally { try { $app.Quit() } catch {} }
}
