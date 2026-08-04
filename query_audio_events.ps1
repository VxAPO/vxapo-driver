$ErrorActionPreference = 'SilentlyContinue'
chcp 936 > $null
$out = 'D:\APO_Project\VxAPO\vxapo-driver\events_out.txt'
Set-Content -Path $out -Value ''

function Log($msg) {
    Write-Host $msg
    Add-Content -Path $out -Value $msg
}

Log '=== APP LOG (vxapo/APO/audio related, last 3h) ==='
Get-WinEvent -FilterHashtable @{LogName='Application'; StartTime=(Get-Date).AddHours(-3)} -ErrorAction SilentlyContinue |
    Where-Object { $_.Message -match 'vxapo|41C34613|B4A97313|audio processing' } |
    Select-Object -First 15 |
    ForEach-Object {
        $m = $_.Message
        if ($m.Length -gt 400) { $m = $m.Substring(0, 400) }
        Log ("[{0}] Id={1} Provider={2}" -f $_.TimeCreated, $_.Id, $_.ProviderName)
        Log $m
        Log ''
    }

Log '=== AUDIO OPERATIONAL LOG (last 30) ==='
Get-WinEvent -LogName 'Microsoft-Windows-Audio/Operational' -MaxEvents 30 -ErrorAction SilentlyContinue |
    ForEach-Object {
        $m = $_.Message
        if ($m.Length -gt 300) { $m = $m.Substring(0, 300) }
        Log ("[{0}] Id={1} Level={2}" -f $_.TimeCreated, $_.Id, $_.LevelDisplayName)
        Log $m
        Log ''
    }

Log '=== DONE ==='