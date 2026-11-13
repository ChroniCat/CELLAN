$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$src = "F:\finance-native-sheets-ready\work\goal-logs\gui\c22.xlsx"
$out = "F:\finance-native-sheets-ready\work\goal-logs\gui\c22-excel.png"
if (Test-Path $out) { Remove-Item $out -Force }
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Open($src)
    $ws = $wb.Worksheets.Item(1)
    $ws.Range("A1:M8").CopyPicture(1, 2) | Out-Null
    Start-Sleep -Milliseconds 800
    $img = [System.Windows.Forms.Clipboard]::GetImage()
    if ($img -eq $null) { "CLIPBOARD_EMPTY" } else {
        $img.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
        "SAVED=$out " + $img.Width + "x" + $img.Height
    }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
}
