param(
  [Parameter(Mandatory=$true)][string]$Path,
  [Parameter(Mandatory=$true)][string]$Printer,
  [int]$Copies = 1,
  [int]$Dpi = 200,
  [switch]$Probe
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
Add-Type -AssemblyName System.Drawing

$asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
  $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
  $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
})[0]
$asTaskAction = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
  $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
  $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction'
})[0]

function Await($WinRtTask, $ResultType) {
  $asTask = $asTaskGeneric.MakeGenericMethod($ResultType)
  $netTask = $asTask.Invoke($null, @($WinRtTask))
  $netTask.Wait(-1) | Out-Null
  $netTask.Result
}

[Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType = WindowsRuntime] | Out-Null
[Windows.Storage.Streams.InMemoryRandomAccessStream, Windows.Storage.Streams, ContentType = WindowsRuntime] | Out-Null

if ([string]::IsNullOrWhiteSpace($Printer)) {
  $Printer = (Get-CimInstance Win32_Printer -Filter 'Default=True' | Select-Object -First 1).Name
}
if ([string]::IsNullOrWhiteSpace($Printer)) { throw '没有指定打印机，也没有系统默认打印机' }
$file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($Path)) ([Windows.Storage.StorageFile])
$pdf = Await ([Windows.Data.Pdf.PdfDocument]::LoadFromFileAsync($file)) ([Windows.Data.Pdf.PdfDocument])
if ($pdf.PageCount -lt 1) { throw 'PDF 没有页面' }

# 逐页渲染成位图；200dpi 对 A4 约 1654×2339，足够打印且不会一次吃几百 MB。
$bitmaps = New-Object System.Collections.ArrayList
for ($i = 0; $i -lt $pdf.PageCount; $i++) {
  $page = $pdf.GetPage($i)
  $w = [int][Math]::Round($page.Size.Width / 72.0 * $Dpi)
  if ($w -lt 1) { $w = 800 }
  $stream = New-Object Windows.Storage.Streams.InMemoryRandomAccessStream
  $opts = New-Object Windows.Data.Pdf.PdfPageRenderOptions
  $opts.DestinationWidth = $w
  $task = $page.RenderToStreamAsync($stream, $opts)
  $netTask = $asTaskAction.Invoke($null, @($task))
  try { $netTask.Wait(-1) | Out-Null } catch { throw "render page ${i}: $($_.Exception.InnerException)" }
  $ras = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($stream)
  [void]$bitmaps.Add([System.Drawing.Image]::FromStream($ras))
  $stream.Dispose()
}

$pd = New-Object System.Drawing.Printing.PrintDocument
$pd.PrinterSettings.PrinterName = $Printer
if (-not $pd.PrinterSettings.IsValid) { throw "打印机无效：$Printer" }
$pd.DocumentName = [System.IO.Path]::GetFileName($Path)
$pd.PrinterSettings.Copies = [int16][Math]::Max(1, [Math]::Min(99, $Copies))
$pd.DefaultPageSettings.Landscape = ($bitmaps[0].Width -gt $bitmaps[0].Height)
$pd.DefaultPageSettings.Margins = New-Object System.Drawing.Printing.Margins(0, 0, 0, 0)
$pd.OriginAtMargins = $false

# 一个 PrintDocument 任务交给驱动，Copies 由 PrintDocument 写入 DEVMODE；
# 不再由调用方循环提交 N 个任务（那会变成 N 个单份作业）。
"COPIES_REQUESTED=$($pd.PrinterSettings.Copies) PAGES=$($bitmaps.Count) PRINTER=$Printer"
if ($Probe) {
  $bitmaps | ForEach-Object { $_.Dispose() }
  exit 0
}

$script:pageIndex = 0
$pd.add_PrintPage({
  param($sender, $e)
  $bmp = $bitmaps[$script:pageIndex]
  $e.Graphics.DrawImage($bmp, 0, 0, $e.PageBounds.Width, $e.PageBounds.Height)
  $script:pageIndex++
  $e.HasMorePages = ($script:pageIndex -lt $bitmaps.Count)
})
$pd.Print()
$bitmaps | ForEach-Object { $_.Dispose() }
"PRINTED=1 COPIES_REQUESTED=$($pd.PrinterSettings.Copies) PAGES=$($script:pageIndex)"
