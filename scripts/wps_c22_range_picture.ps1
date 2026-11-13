$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$src = "F:\finance-native-sheets-ready\work\goal-logs\gui\c22.xlsx"
$out = "F:\finance-native-sheets-ready\work\goal-logs\gui\c22-wps.png"
if (Test-Path $out) { Remove-Item $out -Force }
$app = New-Object -ComObject KET.Application
try { $app.Visible = $false } catch {}
try { $app.DisplayAlerts = $false } catch {}
try {
    $wb = $app.Workbooks.Open($src)
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1:M8").CopyPicture(1, 2) | Out-Null
    Start-Sleep -Milliseconds 900
    $img = [System.Windows.Forms.Clipboard]::GetImage()
    if ($img -eq $null) { "CLIPBOARD_EMPTY" } else {
        $img.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
        "SAVED=$out " + $img.Width + "x" + $img.Height
    }
    $wb.Close($false)
} finally {
    try { $app.Quit() } catch {}
}
