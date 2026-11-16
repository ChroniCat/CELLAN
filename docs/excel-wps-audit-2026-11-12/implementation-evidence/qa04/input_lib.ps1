# QA-04.1 原生输入注入库：真实 SendInput（键盘/Unicode），不是 SC_* 环境变量
# 用法： . "$PSScriptRoot/input_lib.ps1"
Add-Type -Namespace QA04 -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, System.UIntPtr e);
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint f, System.UIntPtr e);
'@

$VK = @{
  F2='0x71'; ESC='0x1B'; ENTER='0x0D'; TAB='0x09'; LEFT='0x25'; UP='0x26'; RIGHT='0x27'; DOWN='0x28';
  HOME='0x24'; END='0x23'; DEL='0x2E'; BS='0x08'; PGUP='0x21'; PGDN='0x22'; SPACE='0x20'; CTRL='0x11'; SHIFT='0x10'; ALT='0x12'
}

function Find-AppWindow([string]$TitlePart) {
  Get-Process app -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -like "*$TitlePart*" } |
    Select-Object -First 1
}

function Focus-App([string]$TitlePart) {
  $p = Find-AppWindow $TitlePart
  if (-not $p) { throw "找不到窗口: $TitlePart" }
  [QA04.Native]::ShowWindow($p.MainWindowHandle, 9) | Out-Null
  [QA04.Native]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
  Start-Sleep -Milliseconds 250
  return $p
}

function Get-Vk([string]$Name) {
  if ($VK.ContainsKey($Name)) { return [Convert]::ToByte($VK[$Name], 16) }
  if ($Name.Length -eq 1) { return [byte][char]$Name.ToUpper() }
  throw "未知按键: $Name"
}

function VkPress([string]$Name, [int]$Times = 1) {
  $vk = Get-Vk $Name
  for ($i = 0; $i -lt $Times; $i++) {
    [QA04.Native]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 25
    [QA04.Native]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
  }
}

function CtrlKey([string]$Name) {
  $vk = Get-Vk $Name
  $ctrl = [Convert]::ToByte($VK['CTRL'], 16)
  [QA04.Native]::keybd_event($ctrl, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 20
  [QA04.Native]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [QA04.Native]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 20
  [QA04.Native]::keybd_event($ctrl, 0, 2, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 80
}

# Unicode 文本注入（真实键盘事件，可含中文；不依赖输入法选中）
function Send-UnicodeText([string]$Text) {
  Add-Type -Namespace QA04 -Name Uni -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public System.UIntPtr dwExtraInfo; }
[StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public KEYBDINPUT ki; public int pad; }
[DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint n, INPUT[] p, int cb);
public static void TypeUnicode(string s) {
  foreach (char c in s) {
    INPUT[] a = new INPUT[2];
    a[0].type = 1; a[0].ki.wScan = c; a[0].ki.dwFlags = 0x0004; // KEYEVENTF_UNICODE
    a[1].type = 1; a[1].ki.wScan = c; a[1].ki.dwFlags = 0x0004 | 0x0002;
    SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
    System.Threading.Thread.Sleep(25);
  }
}
'@ -ReferencedAssemblies System.Runtime.InteropServices
  [QA04.Uni]::TypeUnicode($Text)
  Start-Sleep -Milliseconds 120
}

function Click-At([int]$X, [int]$Y) {
  [QA04.Native]::SetCursorPos($X, $Y) | Out-Null
  Start-Sleep -Milliseconds 60
  [QA04.Native]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
  Start-Sleep -Milliseconds 40
  [QA04.Native]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
  Start-Sleep -Milliseconds 120
}

function Drag-From([int]$X, [int]$Y, [int]$ToX, [int]$ToY) {
  [QA04.Native]::SetCursorPos($X, $Y) | Out-Null
  Start-Sleep -Milliseconds 60
  [QA04.Native]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 80
  $steps = 12
  for ($i = 1; $i -le $steps; $i++) {
    $nx = [int]($X + ($ToX - $X) * $i / $steps)
    $ny = [int]($Y + ($ToY - $Y) * $i / $steps)
    [QA04.Native]::SetCursorPos($nx, $ny) | Out-Null
    Start-Sleep -Milliseconds 40
  }
  Start-Sleep -Milliseconds 150
  [QA04.Native]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 150
}

# 窗口客户区原点（屏幕绝对坐标）
function Get-ClientOrigin($Proc) {
  Add-Type -Namespace QA04 -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
'@
  $pt = New-Object QA04.Win+POINT
  $pt.X = 0; $pt.Y = 0
  [QA04.Win]::ClientToScreen($Proc.MainWindowHandle, [ref]$pt) | Out-Null
  return @{ X = $pt.X; Y = $pt.Y }
}

# 读当前「活动单元格」的可访问名字（我们把它发布成 "A2，值，已选中"）
function Get-ActiveCellName($Proc) {
  Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Proc.MainWindowHandle)
  $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  $names = @()
  foreach ($e in $all) {
    $n = $e.Current.Name
    if ($n -match '^[A-Z]+[0-9]+，') { $names += $n }
  }
  return ($names -join ' | ')
}

function Get-CellRect($Proc, [string]$Cell) {
  Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($Proc.MainWindowHandle)
  $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  foreach ($e in $all) {
    if ($e.Current.Name -like "$Cell，*") { return $e.Current.BoundingRectangle }
  }
  return $null
}
