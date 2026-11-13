$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$src = "F:\finance-native-sheets-ready\work\goal-logs\gui\c17-roundtrip.xlsx"
$out = "F:\finance-native-sheets-ready\work\goal-logs\gui\c17-roundtrip-excel.png"
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($src)
    $ws = $wb.Worksheets.Item(1)
    foreach ($addr in "A1:A3", "B1:B3", "C1:C3", "D1:D3") {
        $fc = $ws.Range($addr).FormatConditions
        $n = $fc.Count
        $ps = @()
        for ($i = 1; $i -le $n; $i++) { $ps += "$($fc.Item($i).Priority)/stop=$($fc.Item($i).StopIfTrue)" }
        "$addr count=$n priorities=$($ps -join ',')"
    }
    $a = $ws.Range("A1:A3").FormatConditions.Item(1).ColorScaleCriteria
    "A criteria types= $($a.Item(1).Type),$($a.Item(2).Type),$($a.Item(3).Type) vals=$($a.Item(2).Value)"
    $b = $ws.Range("B1:B3").FormatConditions.Item(1).ColorScaleCriteria
    "B criteria types= $($b.Item(1).Type),$($b.Item(2).Type),$($b.Item(3).Type) vals=$($b.Item(2).Value)"
    $ws.Range("A1:D3").CopyPicture(1, 2) | Out-Null
    Start-Sleep -Milliseconds 800
    $img = [System.Windows.Forms.Clipboard]::GetImage()
    if ($img -ne $null) { $img.Save($out, [System.Drawing.Imaging.ImageFormat]::Png); "PICTURE=$out $($img.Width)x$($img.Height)" }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
