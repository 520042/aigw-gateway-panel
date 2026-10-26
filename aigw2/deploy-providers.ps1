# 把 aigw2/providers/*.toml 同步到安装目录并重启网关服务（改 toml 不需要重新编译）
# 用法：pwsh -NoProfile -ExecutionPolicy Bypass -File .\deploy-providers.ps1
param(
  [string]$Port = 8800,
  [switch]$NoRestart,
  [switch]$ShowSummary
)
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot 'providers'
$dst = Join-Path $env:LOCALAPPDATA 'aigw2\providers'
if (-not (Test-Path $src)) { throw "找不到 $src（请在 aigw2 目录下运行）" }
New-Item -ItemType Directory -Force $dst | Out-Null

$files = Get-ChildItem (Join-Path $src '*.toml')
Copy-Item (Join-Path $src '*.toml') $dst -Force
Write-Host ("已拷贝 {0} 份 provider 规格 → {1}" -f $files.Count, $dst)

if ($NoRestart) { Write-Host '按 -NoRestart 跳过重启；新清单要重启后才生效。'; exit 0 }

Write-Host '停止旧进程…'
taskkill /IM aigw.exe /F 2>$null | Out-Null
Start-Sleep -Milliseconds 800

$exe = Join-Path $env:LOCALAPPDATA 'aigw2\aigw.exe'
$task = 'Aigw2Service'
$started = $false
$out = schtasks /Query /TN $task 2>$null
if ($LASTEXITCODE -eq 0) {
  schtasks /Change /TN $task /ENABLE 2>$null | Out-Null
  schtasks /Run /TN $task 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { $started = $true; Write-Host "已通过计划任务 $task 启动" }
}
if (-not $started) {
  if (-not (Test-Path $exe)) { throw "找不到 $exe" }
  Start-Process $exe -WindowStyle Hidden
  Write-Host "已直接启动 $exe"
}

$ok = $false
for ($i = 0; $i -lt 40; $i++) {
  Start-Sleep -Milliseconds 500
  try {
    $h = Invoke-RestMethod ("http://127.0.0.1:{0}/api/health" -f $Port) -TimeoutSec 2
    if ($h.ok) { $ok = $true; break }
  } catch { }
}
Write-Host ("健康检查：{0}" -f ($(if ($ok) { 'OK' } else { '失败 —— 请查看 %LOCALAPPDATA%\aigw2\ 日志或手动运行 aigw.exe 看报错' })))
if (-not $ok) { exit 1 }

$p = Invoke-RestMethod ("http://127.0.0.1:{0}/api/platforms" -f $Port) -TimeoutSec 20
$rows = @($p.rows)
Write-Host ("平台总数 {0}，其中有模型清单的 {1} 个，模型条目合计 {2}" -f $rows.Count, @($rows | Where-Object { $_.models_n -gt 0 }).Count, ($rows | Measure-Object -Property models_n -Sum).Sum)
if ($ShowSummary) {
  $rows | Where-Object { $_.models_n -gt 0 } | Sort-Object -Property models_n -Descending |
    Select-Object -First 25 id, models_n, usable, enabled | Format-Table -AutoSize | Out-String | Write-Host
}
