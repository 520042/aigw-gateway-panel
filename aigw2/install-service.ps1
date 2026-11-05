# aigw2 网关自启：把 aigw.exe 注册成「登录时自动运行」的计划任务。
#
# 为什么不用原来的 Aigw2Service：那份是 2026-10-16 手搓的，实测有四个坑——
#   1. Enabled=false，从来没真正自启过（面板一直是手动拉起来的）；
#   2. 触发器是 BootTrigger + LogonType=InteractiveToken —— 开机时用户还没登录，
#      这种组合基本不会触发；
#   3. DisallowStartIfOnBatteries / StopIfGoingOnBatteries=true —— 笔记本一拔电源服务就被杀；
#   4. IdleSettings/StopOnIdleEnd=true —— 机器一离开空闲态任务被结束（用着用着网关没了）；
#   5. 没有 WorkingDirectory，进程起在 System32。
#
# 本脚本用 /XML 注册（schtasks 的开关无法表达「不因电池/空闲停止」这些设置），
# 并且默认只做「登录时启动 + 失败重试 + 无执行时长上限」这几件正常服务该有的事。
#
# ⚠ 另一个实测坑（本机专属，2026-10-23 验证）：动作里**直接写 aigw.exe 会被任务计划程序拒绝**，
#   LastTaskResult = 0x80070005 (E_ACCESSDENIED)，事件日志 TaskScheduler/Operational 101+203 报
#   「无法启动任务…中的操作 …\aigw.exe」。根因未定位，所以动作必须保留一层包装，不要改回直接 Exec。
#   2026-11-05 把包装从 cmd.exe 换成 powershell.exe -WindowStyle Hidden（见 Actions 段注释），
#   解决「每次登录弹命令窗」；实测包装仍然必要、服务照常 200。
#   exe 侧还有一层兜底：服务模式调 FreeConsole() 主动脱离控制台；--doctor / --install
#   这类需要看输出的 CLI 模式不受影响。
#
# ⚠ 本文件必须以 UTF-8 with BOM 保存：PowerShell 5.1 读无 BOM 的 UTF-8 会按 ANSI 解码，
#   中文注释变乱码进而引发「Unexpected token」语法错误（实测踩过）。
#
# 用法：
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-service.ps1            # 注册并启用
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-service.ps1 -Run       # 注册后立刻跑一次
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-service.ps1 -Status    # 只看现状
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\install-service.ps1 -Uninstall # 删除任务
#   ... 加 -TaskName Aigw2ServiceTest 可以装一份临时任务验证，不碰正式的

[CmdletBinding()]
param(
  [string]$TaskName = "Aigw2Service",
  [int]$Port = 8800,
  [switch]$Run,
  [switch]$Status,
  [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$dest = Join-Path $env:LOCALAPPDATA "aigw2"
$exe = Join-Path $dest "aigw.exe"

function Show-Status {
  # 用 ScheduledTasks 模块读结构化对象：schtasks 的文本输出在本机是 UTF-16，经管道会被
  # 按 ANSI 解码成一串 NUL 字符，正则全部失配（实测：Enabled 明明是 true 却显示"已禁用"）。
  $t = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
  if (-not $t) {
    Write-Host "任务 $TaskName 不存在" -ForegroundColor Yellow
    return $false
  }
  $act = @($t.Actions)[0]
  $cmd = $act.Execute
  $argStr = $act.Arguments
  # 包装层现在可以是 powershell.exe（隐藏窗口，当前方案）或 cmd.exe（旧方案）；
  # 两者都算「包了一层」，只有 Execute 直接指向 aigw.exe 才是危险的直写形式。
  $wrappedBy = if ($cmd -match 'powershell\.exe$') { 'PowerShell Hidden 包装（无命令窗，正常）' }
    elseif ($cmd -match 'cmd\.exe$') { 'cmd.exe 包装（旧方案，会弹命令窗，建议重跑本脚本）' }
    else { '' }
  $wd = $act.WorkingDirectory
  $enabled = if ($t.State -eq "Disabled") { "已禁用" } else { "已启用" }
  Write-Host "任务 $TaskName：$enabled"
  Write-Host "  命令      : $cmd $argStr"
  Write-Host "  启动方式  : $(if ($wrappedBy) { $wrappedBy } else { '直接 Exec（本机可能 0x80070005，建议重跑本脚本）' })"
  Write-Host "  工作目录  : $(if ($wd) { $wd } else { '(未设置)' })"
  Write-Host "  电池不打断: $(-not $t.Settings.DisallowStartIfOnBatteries)"
  Write-Host "  空闲不打断: $(-not $t.Settings.IdleSettings.StopOnIdleEnd)"
  Write-Host "  当前状态  : $($t.State)"
  return $true
}

if ($Status) { Show-Status | Out-Null; exit 0 }

if ($Uninstall) {
  schtasks /Delete /TN $TaskName /F 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { Write-Host "已删除任务 $TaskName" -ForegroundColor Green }
  else { Write-Host "删除失败（任务可能不存在）" -ForegroundColor Yellow }
  exit 0
}

if (-not (Test-Path $exe)) { throw "找不到 $exe，先跑 build-deploy.ps1 部署" }

$user = "$env:USERDOMAIN\$env:USERNAME"
# ★ 不要用 `whoami /user`：在 Git Bash / MSYS 下 PATH 里的 /usr/bin/whoami 会抢在
#   Windows 的 whoami.exe 前面，而它不认 /user 参数 → 报 "extra operand '/user'"，
#   $sid 变成空 → XML 的 UserId 非法 → schtasks /Create 直接失败（实测踩过）。
#   改用 .NET 直接拿当前 Windows 身份的 SID，纯进程内调用，不依赖 PATH 上的外部命令。
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
if (-not $sid) { throw "取不到当前用户的 SID" }
$xmlPath = Join-Path $env:TEMP "aigw-service-$TaskName.xml"

# 说明几点：
# - 触发器用 LogonTrigger 而不是 BootTrigger：进程要读当前用户的 DPAPI 密文凭据，
#   必须是该用户的交互式会话，开机时还没登录，BootTrigger 起不来（原来的坑）；
# - ExecutionTimeLimit=PT0S 表示不限时长（默认 72 小时会被强杀）；
# - RestartOnFailure 让进程崩了自动再起，配合应用内 data/logs 日志能留下现场。
$xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>aigw2 聚合网关（127.0.0.1:$Port）：登录时启动，日志见 data\logs\aigw.log</Description>
    <URI>\$TaskName</URI>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <UserId>$sid</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <!-- ★ Hidden=true：计划任务本身不再显示命令窗（配合 exe 内的 FreeConsole 双保险） -->
    <Hidden>true</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$user</UserId>
    </LogonTrigger>
    <!-- 会话状态变化（解锁 / 远程连接）时再拉起：计划任务在「注销时停止」或系统挂起恢复后
         偶尔不会自己回来，加这个触发器相当于一记兜底。MultipleInstancesPolicy=IgnoreNew
         保证网关已经在跑时不会起第二份（第二份会抢 8800 端口直接退出）。 -->
    <SessionStateChangeTrigger>
      <Enabled>true</Enabled>
      <UserId>$user</UserId>
      <StateChange>SessionUnlock</StateChange>
    </SessionStateChangeTrigger>
    <SessionStateChangeTrigger>
      <Enabled>true</Enabled>
      <UserId>$user</UserId>
      <StateChange>RemoteConnect</StateChange>
    </SessionStateChangeTrigger>
  </Triggers>
  <Actions Context="Author">
    <Exec>
      <!-- ★ 2026-11-05 改：原来用 `cmd.exe /c aigw.exe`，每次登录都会弹一个黑色命令窗
           且一直挂在任务栏（用户明确要求去掉）。
           现在改用 powershell.exe -WindowStyle Hidden 包一层：
             · PowerShell 自身以隐藏窗口启动，派生的 aigw.exe 继承隐藏属性；
             · exe 内服务模式还会调 FreeConsole() 双保险（CLI 模式不受影响）；
             · 本机实测：Register/Start 后 /api/health=200、LastTaskResult=0x41301(运行中)，
               且进程树里 aigw.exe 直接挂在 powershell 下，不再有 cmd.exe 控制台。
           注意仍保留「包一层」而不用直接 Exec aigw.exe：文件头记录的 E_ACCESSDENIED
           (0x80070005) 是 schtasks /XML 直写 exe 时的本机现象，包装层不能省。 -->
      <Command>C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe</Command>
      <Arguments>-NoProfile -WindowStyle Hidden -Command "&amp; '$exe' --port $Port"</Arguments>
      <WorkingDirectory>$dest</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"@

# schtasks /XML 需要 UTF-16 编码的 XML 文件
[System.IO.File]::WriteAllText($xmlPath, $xml, [System.Text.UnicodeEncoding]::new($false, $true))
schtasks /Create /TN $TaskName /XML $xmlPath /F | Out-Null
if ($LASTEXITCODE -ne 0) { throw "schtasks /Create 失败（exit $LASTEXITCODE）" }
Remove-Item $xmlPath -ErrorAction SilentlyContinue
Write-Host "已注册任务 $TaskName（登录时启动，隐藏窗口：powershell -WindowStyle Hidden → aigw.exe --port $Port）" -ForegroundColor Green
Show-Status | Out-Null

if ($Run) {
  schtasks /Run /TN $TaskName | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "schtasks /Run 失败（exit $LASTEXITCODE）" }
  Write-Host "已触发一次运行，等待服务就绪..." -ForegroundColor Cyan
  $ok = $false
  for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Milliseconds 500
    try {
      $h = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/health" -TimeoutSec 3
      Write-Host ("OK  health = " + ($h | ConvertTo-Json -Compress -Depth 4))
      $ok = $true
      break
    } catch { }
  }
  if (-not $ok) { throw "15 秒内 /api/health 不可达（看 $dest\data\logs\aigw.log）" }
}
