$ErrorActionPreference='Stop'
$files = @{ orig='F:\finance-native-sheets-ready\crates\core\tests\fixtures\c07\alerts.xlsx';
            ours='F:\finance-native-sheets-ready\work\probe\c07\alerts-ours.xlsx' }
foreach ($id in @('Excel.Application','KET.Application')) {
  $app = New-Object -ComObject $id
  try { $app.Visible=$false; $app.DisplayAlerts=$false } catch {}
  foreach ($k in @('orig','ours')) {
    $wb = $app.Workbooks.Open($files[$k]); $ws = $wb.Worksheets.Item(1)
    foreach ($a in @('B2','C2','D2','E2')) {
      $v = $ws.Range($a).Validation
      "{0} {1} {2} type={3} err={4} showErr={5} showIn={6} prompt=[{7}] errmsg=[{8}] f1=[{9}]" -f $id,$k,$a,$v.Type,$v.ErrorStyle,$v.ShowError,$v.ShowInput,$v.PromptTitle,$v.ErrorMessage,$v.Formula1
    }
    $wb.Close($false)
  }
  try { $app.Quit() } catch {}
}
