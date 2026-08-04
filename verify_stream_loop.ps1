$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\audiodg_check_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8

function Log($msg) {
    Add-Content -Path $out -Value $msg -Encoding UTF8
}

Log ('=== start: {0} ===' -f (Get-Date -Format 'HH:mm:ss'))

$procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
if ($procs.Count -gt 0) {
    Log ("existing audiodg PID: {0} total={1}" -f $procs[0].Id, $procs[0].Modules.Count)
} else {
    Log 'NO_AUDIODG_AT_START'
}

# Find a system wav to loop - keeps a live audio stream alive
$wav = $null
$candidates = @(
    "$env:WINDIR\Media\tada.wav",
    "$env:WINDIR\Media\Windows Notify.wav",
    "$env:WINDIR\Media\chimes.wav",
    "$env:WINDIR\Media\Alarm01.wav"
)
foreach ($c in $candidates) {
    if (Test-Path $c) {
        $wav = $c
        break
    }
}

if ($null -eq $wav) {
    Log 'NO_WAV_FOUND'
} else {
    Log ("loop playing: {0}" -f $wav)
    $player = New-Object System.Media.SoundPlayer($wav)
    $player.PlayLooping()

    # sample 6 times, every 2 seconds = 12s of sustained stream
    for ($n = 1; $n -le 6; $n++) {
        Start-Sleep -Seconds 2
        $procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
        if ($procs.Count -eq 0) {
            Log 'AUDIODG_EXITED'
            break
        }
        $proc = $procs[0]
        $mods = $proc.Modules
        Log ("--- sample {0} @ {1}s PID={2} total={3} ---" -f $n, ($n * 2), $proc.Id, $mods.Count)
        $hits = $mods | Where-Object { $_.ModuleName -match 'vxapo|eapo|equalizer|41C34613|B4A97313' }
        if ($hits) {
            Log '=== VXAPO_OR_EAPO_HIT ==='
            $hits | ForEach-Object { Log ("{0}  {1}" -f $_.ModuleName, $_.FileName) }
        } else {
            Log 'NO_VXAPO_NO_EAPO_IN_MODULES'
        }
    }

    $player.Stop()
    $player.Dispose()
}

Log ('=== end: {0} ===' -f (Get-Date -Format 'HH:mm:ss'))