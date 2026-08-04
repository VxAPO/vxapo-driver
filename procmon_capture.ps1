$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\procmon_cap_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

$pml = 'C:\ProgramData\VxAPO\procmon_vxapo.pml'
$csv = 'C:\ProgramData\VxAPO\procmon_vxapo.csv'
foreach ($f in @($pml, $csv)) { if (Test-Path $f) { Remove-Item $f -Force } }

Log "=== start ==="
Log "PML path: $pml"

# ProcMon 采集 40 秒，结束后自动 SaveAs CSV
$procmonCmd = "cd /d D:\360极速浏览器X下载\ProcessMonitor && Procmon64.exe /AcceptEula /Quiet /Minimized /BackingFile `"$pml`" /SaveAs `"$csv`" /Runtime 40"
# 用 cmd /c 启动（提权 + 中文路径安全），不等待（ProcMon 自己 40 秒结束）
Start-Process -FilePath "cmd.exe" -ArgumentList "/c", $procmonCmd -WindowStyle Hidden
Log "ProcMon launched (40s runtime)"

Start-Sleep -Seconds 3
Log "restarting audio service..."
cmd /c "net stop audiosrv & timeout /t 2 /nobreak >nul & net start audiosrv"
Start-Sleep -Seconds 3

Log "playing tada.wav loop 30s..."
$wav = "$env:WINDIR\Media\tada.wav"
$player = New-Object System.Media.SoundPlayer($wav)
$player.PlayLooping()
Start-Sleep -Seconds 30
$player.Stop()
$player.Dispose()

Log "waiting for ProcMon to finish..."
Start-Sleep -Seconds 10

Start-Sleep -Seconds 2
# 检查 ProcMon 是否还在跑，等它结束
for ($i = 0; $i -lt 20; $i++) {
    $running = Get-Process Procmon64 -ErrorAction SilentlyContinue
    if (-not $running) { break }
    Start-Sleep -Seconds 2
}

Log ("pml exists: {0}" -f (Test-Path $pml))
Log ("csv exists: {0}" -f (Test-Path $csv))
if (Test-Path $csv) { Log ("csv size: {0}" -f (Get-Item $csv).Length) }
Log "=== end ==="