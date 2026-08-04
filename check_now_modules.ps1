$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\now_modules_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

$procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
if ($procs.Count -eq 0) { Log 'NO_AUDIODG'; exit }
$proc = $procs[0]
Log ("audiodg PID: {0}" -f $proc.Id)
$mods = $proc.Modules
Log ("total modules: {0}" -f $mods.Count)
$hits = $mods | Where-Object { $_.ModuleName -match 'vxapo|eapo|equalizer|41C34613|B4A97313' }
if ($hits) {
    Log '=== VXAPO_OR_EAPO_HIT ==='
    $hits | ForEach-Object { Log ("{0}  {1}" -f $_.ModuleName, $_.FileName) }
} else {
    Log 'NO_VXAPO_NO_EAPO_IN_MODULES'
}
$names = ($mods | ForEach-Object { $_.ModuleName }) -join ', '
Log ("modules: {0}" -f $names)
Log 'DONE'