# aigw2 一键：测试 -> 构建 -> 部署 -> 重启 -> 健康检查 ->（可选）冒烟
#
# 用法（在本目录或任意目录执行均可）：
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build-deploy.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build-deploy.ps1 -SkipTest        # 只构建部署
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build-deploy.ps1 -Smoke           # 部署后跑一次 MiMo 冒烟
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\build-deploy.ps1 -Smoke -SmokeModel "doubao-fast@apk-doubao"
#
# 约定（来自 .workbuddy/memory/MEMORY.md）：
#   * CARGO_TARGET_DIR = C:/aigw-target，产物 <target>/<triple>/debug/aigw.exe
#   * 安装位 = %LOCALAPPDATA%\aigw2\（exe + providers\*.toml），数据目录 = %LOCALAPPDATA%\aigw2\data
#   * 自启任务名 Aigw2Service；服务端口 127.0.0.1:8800
#   * 默认工具链是 gnu（缺 dlltool 会失败），必须用 +stable-x86_64-pc-windows-msvc

[CmdletBinding()]
param(
  [switch]$SkipTest,
  [switch]$Release,
  [switch]$Smoke,
  [string]$SmokeModel = "MiMo-V2.5@mimo",
  [string]$Toolchain = "stable-x86_64-pc-windows-msvc",
  [string]$Triple = "x86_64-pc-windows-msvc",
  [int]$Port = 8800
)

$ErrorActionPreference = "Stop"
$repo = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }
Set-Location $repo

# ---------- 0. 环境 ----------
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = "C:/aigw-target" }
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path $cargoBin) { $env:PATH = "$cargoBin;$env:PATH" }
$cargo = Join-Path $cargoBin "cargo.exe"
if (-not (Test-Path $cargo)) {
  $c = Get-Command cargo.exe -ErrorAction SilentlyContinue
  if ($c) { $cargo = $c.Source } else { throw "找不到 cargo.exe（先装 Rust 工具链）" }
}

$profileName = if ($Release) { "release" } else { "debug" }
$exe = Join-Path $env:CARGO_TARGET_DIR (Join-Path $Triple (Join-Path $profileName "aigw.exe"))
$dest = Join-Path $env:LOCALAPPDATA "aigw2"
$base = "http://127.0.0.1:$Port"

function Step($n, $t) { Write-Host ""; Write-Host "===== $n  $t =====" -ForegroundColor Cyan }
function Invoke-Cargo([string[]]$cargoArgs) {
  & $cargo "+$Toolchain" @cargoArgs
  if ($LASTEXITCODE -ne 0) { throw "cargo $($cargoArgs -join ' ') 失败（exit $LASTEXITCODE）" }
}

Write-Host "repo       : $repo"
Write-Host "target dir : $env:CARGO_TARGET_DIR"
Write-Host "exe        : $exe"

# ---------- 1. 测试 ----------
if (-not $SkipTest) {
  Step "1/6" "cargo test（全 workspace）"
  Invoke-Cargo @("test", "--target", $Triple)
} else {
  Write-Host "（跳过测试）" -ForegroundColor DarkGray
}

# ---------- 2. 构建 ----------
Step "2/6" "cargo build -p aigw（$profileName）"
$buildArgs = @("build", "--target", $Triple, "-p", "aigw")
if ($Release) { $buildArgs += "--release" }
Invoke-Cargo $buildArgs
if (-not (Test-Path $exe)) { throw "构建完成但找不到 $exe" }
Write-Host ("exe 大小 : {0:N0} 字节" -f (Get-Item $exe).Length)

# ---------- 3. 停旧进程 ----------
Step "3/6" "停止旧服务"
& taskkill /IM aigw.exe /F 2>$null | Out-Null
Start-Sleep -Milliseconds 800

# ---------- 4. 拷贝 ----------
Step "4/6" "部署到 $dest"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item $exe (Join-Path $dest "aigw.exe") -Force
$srcProv = Join-Path $repo "providers"
$dstProv = Join-Path $dest "providers"
New-Item -ItemType Directory -Force -Path $dstProv | Out-Null
Copy-Item (Join-Path $srcProv "*.toml") $dstProv -Force
$n = (Get-ChildItem $dstProv -Filter *.toml).Count
Write-Host "providers: $n 份 toml"

# ---------- 5. 启动 ----------
Step "5/6" "启动服务（计划任务 Aigw2Service，失败则直接拉起 exe）"
$task = (schtasks /Query /TN Aigw2Service 2>$null | Out-String)
if ($LASTEXITCODE -eq 0 -and $task) {
  schtasks /Run /TN Aigw2Service | Out-Null
  Write-Host "已触发计划任务 Aigw2Service"
} else {
  Start-Process -FilePath (Join-Path $dest "aigw.exe") -WorkingDirectory $dest -WindowStyle Hidden
  Write-Host "未找到计划任务，已直接启动 aigw.exe"
}

# ---------- 6. 健康检查 ----------
Step "6/6" "健康检查 $base/api/health"
$ok = $false
for ($i = 0; $i -lt 40; $i++) {
  Start-Sleep -Milliseconds 500
  try {
    $h = Invoke-RestMethod -Uri "$base/api/health" -TimeoutSec 3
    $ok = $true
    Write-Host ("OK  health = " + ($h | ConvertTo-Json -Compress -Depth 4))
    break
  } catch { }
}
if (-not $ok) { throw "服务 20 秒内未就绪：$base/api/health 不可达（看 $dest\data 下日志）" }

# 平台清单（确认新 toml 被加载；非法 spec 会被 load_dir_lenient 静默跳过）
try {
  $plats = Invoke-RestMethod -Uri "$base/api/platforms" -TimeoutSec 10
  $arr = @()
  if ($plats -is [array]) { $arr = $plats }
  else { foreach ($p in $plats.PSObject.Properties) { if ($p.Value -is [array]) { $arr = @($p.Value); break } } }
  Write-Host ("/api/platforms 平台数 = " + $arr.Count)
  $mimo = $arr | Where-Object { $_.id -eq "mimo" }
  if ($mimo) {
    Write-Host ("mimo       : 已加载 auth=" + $mimo.auth + " 模型数=" + (@($mimo.models)).Count)
  } else {
    Write-Host "mimo       : 未出现！检查 providers\mimo.toml 是否被 validate 拒绝（启动日志有 warn）" -ForegroundColor Yellow
  }
} catch {
  Write-Host ("/api/platforms 读取失败：" + $_.Exception.Message) -ForegroundColor Yellow
}

# ---------- 冒烟 ----------
if ($Smoke) {
  Write-Host ""
  Write-Host "===== 冒烟 /v1/chat/completions model=$SmokeModel =====" -ForegroundColor Cyan
  $obj = @{
    model    = $SmokeModel
    stream   = $false
    messages = @(@{ role = "user"; content = "只回复两个字：你好" })
  }
  $body = $obj | ConvertTo-Json -Depth 6 -Compress
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
  $t0 = Get-Date
  try {
    $r = Invoke-RestMethod -Uri "$base/v1/chat/completions" -Method Post -Body $bytes -ContentType "application/json; charset=utf-8" -TimeoutSec 180
    $ms = [int]((Get-Date) - $t0).TotalMilliseconds
    Write-Host ("HTTP 200  耗时 " + $ms + "ms")
    $txt = $r.choices[0].message.content
    Write-Host ("content : " + $txt)
    if ($r.usage) { Write-Host ("usage   : in=" + $r.usage.prompt_tokens + " out=" + $r.usage.completion_tokens) }
  } catch {
    $ms = [int]((Get-Date) - $t0).TotalMilliseconds
    Write-Host ("失败  耗时 " + $ms + "ms  " + $_.Exception.Message) -ForegroundColor Red
    try {
      $resp = $_.Exception.Response
      if ($resp) {
        $sr = New-Object System.IO.StreamReader($resp.GetResponseStream())
        Write-Host ("响应体 : " + $sr.ReadToEnd()) -ForegroundColor Red
      }
    } catch { }
    throw "冒烟失败"
  }
}

Write-Host ""
Write-Host "全部完成。面板：$base" -ForegroundColor Green
