# C12.4：让 Excel 打开我们的多字段透视并另存（相当于用户打开后直接存回），
# 后续再用 probe2 检查存回的文件里透视表还在不在。
$ErrorActionPreference = 'Stop'
$src = $args[0]
$dst = $args[1]
$src = (Resolve-Path $src).Path.Replace('/', '\')
$dst = [IO.Path]::GetFullPath($dst).Replace('/', '\')

$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($src, 0, $false)
Start-Sleep -Milliseconds 800
$wb.SaveAs($dst, 51)
$wb.Close($false)
$xl.Quit()
"saved " + $dst
