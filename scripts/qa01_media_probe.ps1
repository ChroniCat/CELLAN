# QA-01.3 图片/图形/外链探针
$ErrorActionPreference='Stop'
$file = (Resolve-Path $args[0]).Path
$prog = if ($args.Count -ge 2) { $args[1] } else { 'Excel.Application' }
$xl = New-Object -ComObject $prog
$xl.Visible=$false
try { $xl.DisplayAlerts=$false } catch {}
$wb = $xl.Workbooks.Open($file, 0, $true)
try {
    "product=" + $xl.Name + " " + $xl.Version
    $ws = $wb.Worksheets.Item(1)
    "sheet=" + $ws.Name
    "pictures=" + $ws.Pictures.Count
    "shapes=" + $ws.Shapes.Count
    for ($i=1; $i -le $ws.Shapes.Count; $i++) {
        $s = $ws.Shapes.Item($i)
        "shape" + $i + "=" + $s.Type + " name=" + $s.Name + " w=" + [int]$s.Width + " h=" + [int]$s.Height
    }
    $links = $wb.LinkSources(1)
    if ($links -eq $null) { "links=0" } else { "links=" + (@($links).Count) + " all=" + (($links | ForEach-Object { Split-Path $_ -Leaf }) -join ";") }
    "a2.formula=" + $ws.Range('A2').Formula
    "a2.value=" + $ws.Range('A2').Value2
    "a3.value=" + $ws.Range('A3').Value2
} finally {
    $wb.Close($false)
    $xl.Quit()
}
