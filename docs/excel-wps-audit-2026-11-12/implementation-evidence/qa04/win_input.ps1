# QA-04.1 Windows 原生输入：直接向窗口消息队列投递 WM_KEYDOWN/WM_CHAR/WM_LBUTTONDOWN
#
# 为什么不用 SendInput：本机 Hermes 运行在无交互桌面的会话里（GetForegroundWindow()=0），
# SendInput 会静默失败。PostMessage 走的是 winit 的真实窗口消息处理路径，不是 SC_* 环境变量。
Add-Type -Namespace QI -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, System.UIntPtr w, System.IntPtr l);
[DllImport("user32.dll")] public static extern uint MapVirtualKey(uint code, uint mapType);
[DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
'@

$VK = @{ F2=0x71; ESC=0x1B; ENTER=0x0D; TAB=0x09; LEFT=0x25; UP=0x26; RIGHT=0x27; DOWN=0x28;
         HOME=0x24; END=0x23; DEL=0x2E; BS=0x08; PGUP=0x21; PGDN=0x22; SPACE=0x20; F4=0x73; F9=0x78 }

function Get-Vk([string]$Name) {
  if ($VK.ContainsKey($Name)) { return [int]$VK[$Name] }
  if ($Name.Length -eq 1) { return [int][char]$Name.ToUpper() }
  throw "未知按键: $Name"
}

function Find-AppWindow([string]$TitlePart) {
  Get-Process app -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -like "*$TitlePart*" } |
    Select-Object -First 1
}

function Post-Key($Proc, [string]$Key, [int]$Times = 1, [switch]$Ctrl, [switch]$Shift) {
  $h = $Proc.MainWindowHandle
  $vk = Get-Vk $Key
  $scan = [QI.Win]::MapVirtualKey([uint32]$vk, 0)
  if ($Ctrl)  { [QI.Win]::PostMessage($h, 0x0100, [UIntPtr]::new([uint64]0x11), [IntPtr]0) | Out-Null }
  if ($Shift) { [QI.Win]::PostMessage($h, 0x0100, [UIntPtr]::new([uint64]0x10), [IntPtr]0) | Out-Null }
  for ($i = 0; $i -lt $Times; $i++) {
    $lp = [IntPtr](1 -bor ([int]$scan -shl 16))
    [QI.Win]::PostMessage($h, 0x0100, [UIntPtr]::new([uint64]$vk), $lp) | Out-Null
    Start-Sleep -Milliseconds 30
    $lpUp = [IntPtr](1 -bor ([int]$scan -shl 16) -bor (1 -shl 30) -bor (1 -shl 31))
    [QI.Win]::PostMessage($h, 0x0101, [UIntPtr]::new([uint64]$vk), $lpUp) | Out-Null
    Start-Sleep -Milliseconds 50
  }
  if ($Shift) { [QI.Win]::PostMessage($h, 0x0101, [UIntPtr]::new([uint64]0x10), [IntPtr]0) | Out-Null }
  if ($Ctrl)  { [QI.Win]::PostMessage($h, 0x0101, [UIntPtr]::new([uint64]0x11), [IntPtr]0) | Out-Null }
  Start-Sleep -Milliseconds 120
}

# WM_CHAR：和输入法最终上屏后 winit 收到的是同一条消息（这里不涉及候选词选择）
function Post-Text($Proc, [string]$Text) {
  $h = $Proc.MainWindowHandle
  foreach ($ch in $Text.ToCharArray()) {
    [QI.Win]::PostMessage($h, 0x0102, [UIntPtr]::new([uint64][int]$ch), [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 30
  }
  Start-Sleep -Milliseconds 120
}

function ScreenToClientPt($Proc, [int]$X, [int]$Y) {
  $p = New-Object QI.Win+POINT
  $p.X = $X; $p.Y = $Y
  [QI.Win]::ScreenToClient($Proc.MainWindowHandle, [ref]$p) | Out-Null
  return $p
}

function Post-Click($Proc, [int]$ScreenX, [int]$ScreenY) {
  $p = ScreenToClientPt $Proc $ScreenX $ScreenY
  $lp = [IntPtr](($p.X -band 0xFFFF) -bor (($p.Y -band 0xFFFF) -shl 16))
  $h = $Proc.MainWindowHandle
  [QI.Win]::PostMessage($h, 0x0200, [UIntPtr]::new(0), $lp) | Out-Null   # WM_MOUSEMOVE
  Start-Sleep -Milliseconds 60
  [QI.Win]::PostMessage($h, 0x0201, [UIntPtr]::new(1), $lp) | Out-Null   # WM_LBUTTONDOWN
  Start-Sleep -Milliseconds 60
  [QI.Win]::PostMessage($h, 0x0202, [UIntPtr]::new(0), $lp) | Out-Null   # WM_LBUTTONUP
  Start-Sleep -Milliseconds 180
}

function Post-Drag($Proc, [int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [int]$Steps = 10) {
  $h = $Proc.MainWindowHandle
  $a = ScreenToClientPt $Proc $X1 $Y1
  $lp = [IntPtr](($a.X -band 0xFFFF) -bor (($a.Y -band 0xFFFF) -shl 16))
  [QI.Win]::PostMessage($h, 0x0200, [UIntPtr]::new(0), $lp) | Out-Null
  Start-Sleep -Milliseconds 60
  [QI.Win]::PostMessage($h, 0x0201, [UIntPtr]::new(1), $lp) | Out-Null
  Start-Sleep -Milliseconds 80
  for ($i = 1; $i -le $Steps; $i++) {
    $sx = [int]($X1 + ($X2 - $X1) * $i / $Steps)
    $sy = [int]($Y1 + ($Y2 - $Y1) * $i / $Steps)
    $c = ScreenToClientPt $Proc $sx $sy
    $lp2 = [IntPtr](($c.X -band 0xFFFF) -bor (($c.Y -band 0xFFFF) -shl 16))
    [QI.Win]::PostMessage($h, 0x0200, [UIntPtr]::new(1), $lp2) | Out-Null
    Start-Sleep -Milliseconds 35
  }
  $c2 = ScreenToClientPt $Proc $X2 $Y2
  $lp3 = [IntPtr](($c2.X -band 0xFFFF) -bor (($c2.Y -band 0xFFFF) -shl 16))
  [QI.Win]::PostMessage($h, 0x0202, [UIntPtr]::new(0), $lp3) | Out-Null
  Start-Sleep -Milliseconds 200
}

function Get-UiaAll($Proc) {
  Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Proc.MainWindowHandle)
  return $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
}

function Get-ActiveCellName($Proc) {
  $names = @()
  foreach ($e in (Get-UiaAll $Proc)) {
    if ($e.Current.Name -match '^[A-Z]+[0-9]+，') { $names += $e.Current.Name }
  }
  return ($names -join ' | ')
}

function Get-EditValue($Proc, [string]$Name = '编辑栏') {
  foreach ($e in (Get-UiaAll $Proc)) {
    if ($e.Current.Name -eq $Name) {
      try {
        $vp = $e.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        return $vp.Current.Value
      } catch { return '<no-value-pattern>' }
    }
  }
  return '<missing>'
}

function Get-GridSummary($Proc) {
  foreach ($e in (Get-UiaAll $Proc)) {
    if ($e.Current.ControlType.ProgrammaticName -eq 'ControlType.DataGrid') { return $e.Current.Name }
  }
  return '<no-grid>'
}
