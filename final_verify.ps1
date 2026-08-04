$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\final_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

Log '=== restart audio service (new audiodg re-reads AudioEngine keys) ==='
net stop audiosrv 2>&1 | Out-Null
Start-Sleep -Seconds 2
net start audiosrv 2>&1 | Out-Null

# poll for new audiodg (old killed, fresh process reads AudioEngine keys at init)
$proc = $null
for ($i = 0; $i -lt 30; $i++) {
    $procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
    if ($procs.Count -gt 0) { $proc = $procs[0]; break }
    Start-Sleep -Milliseconds 500
}
if ($null -eq $proc) { Log 'NO_AUDIODG_AFTER_RESTART'; exit }
Log ("new audiodg PID: {0}" -f $proc.Id)

# sample modules every 2s x 5 - APO loads at endpoint graph init (no playback needed)
for ($n = 1; $n -le 5; $n++) {
    Start-Sleep -Seconds 2
    $procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) { Log 'AUDIODG_EXITED'; break }
    $p = $procs[0]
    $mods = $p.Modules
    Log ("--- sample {0} @ {1}s PID={2} total={3} ---" -f $n, ($n * 2), $p.Id, $mods.Count)
    $hits = $mods | Where-Object { $_.ModuleName -match 'vxapo|eapo|equalizer|41C34613|B4A97313' }
    if ($hits) {
        Log '=== VXAPO_OR_EAPO_HIT ==='
        $hits | ForEach-Object { Log ("{0}  {1}" -f $_.ModuleName, $_.FileName) }
    } else {
        Log 'NO_VXAPO_NO_EAPO_IN_MODULES'
    }
}
Log 'DONE'