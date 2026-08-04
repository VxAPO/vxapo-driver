$ErrorActionPreference = 'SilentlyContinue'
chcp 936 > $null

$dll = 'D:\APO_Project\VxAPO\vxapo-driver\target\x86_64-pc-windows-msvc\debug\vxapo_driver.dll'
$eapo = 'D:\Program Files\EqualizerAPO\EqualizerAPO.dll'

Write-Host '=== VxAPO debug DLL dependencies ==='
& dumpbin /dependents $dll | Where-Object { $_ -match '\.dll' } | ForEach-Object { $_.Trim() }

if (Test-Path $eapo) {
    Write-Host ''
    Write-Host '=== EAPO DLL dependencies ==='
    & dumpbin /dependents $eapo | Where-Object { $_ -match '\.dll' } | ForEach-Object { $_.Trim() }
} else {
    Write-Host ''
    Write-Host 'EAPO dll not found at ' $eapo
}

Write-Host ''
Write-Host '=== check EAPO install dir =='
Get-ChildItem 'D:\Program Files\EqualizerAPO' -ErrorAction SilentlyContinue | Select-Object Name