# QA-01.3 SmartArt / 三维形状探针
$ErrorActionPreference='Stop'
$file=(Resolve-Path $args[0]).Path
$prog= if ($args.Count -ge 2) { $args[1] } else { 'Excel.Application' }
$xl=New-Object -ComObject $prog
$xl.Visible=$false
try { $xl.DisplayAlerts=$false } catch {}
$wb=$xl.Workbooks.Open($file,0,$true)
try {
  "product=" + $xl.Name + " " + $xl.Version
  $ws=$wb.Worksheets.Item(1)
  "shapes=" + $ws.Shapes.Count
  $art=0
  for($i=1;$i -le $ws.Shapes.Count;$i++){
    $s=$ws.Shapes.Item($i)
    if ($s.HasSmartArt) {
      $art++
      $texts=@()
      for($k=1;$k -le $s.SmartArt.Nodes.Count;$k++){ $texts += $s.SmartArt.Nodes.Item($k).TextFrame2.TextRange.Text }
      "smartart" + $art + "=name:" + $s.Name + " nodes:" + $s.SmartArt.Nodes.Count + " texts:" + ($texts -join '|') + " w=" + [int]$s.Width + " h=" + [int]$s.Height
    } else {
      $line="shape" + $i + "=type" + $s.Type + " name:" + $s.Name + " w=" + [int]$s.Width + " h=" + [int]$s.Height
      if ($s.ThreeD.Depth -gt 0) { $line += " depth:" + $s.ThreeD.Depth + " rotX:" + $s.ThreeD.RotationX + " rotY:" + $s.ThreeD.RotationY }
      $line
    }
  }
  "diagram_count=$art"
} finally { $wb.Close($false); $xl.Quit() }
