$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\audioengine_keys_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

$base = 'HKCR:\AudioEngine\AudioProcessingObjects'
$entries = @(
    @{ id = '{41C34613-D391-459D-A039-72B2B15A1A1D}'; name = 'VxAPO Pre-Mix' },
    @{ id = '{B4A97313-ABC0-45ED-9C33-428B20D39428}'; name = 'VxAPO Post-Mix' }
)

foreach ($e in $entries) {
    $key = Join-Path $base $e.id
    Log "creating: $key"
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name FriendlyName -Value $e.name -PropertyType String -Force | Out-Null
    New-ItemProperty -Path $key -Name Copyright -Value 'VxAPO Project' -PropertyType String -Force | Out-Null
    New-ItemProperty -Path $key -Name Flags -Value 0xd -PropertyType DWord -Force | Out-Null
    New-ItemProperty -Path $key -Name NumAPOInterfaces -Value 1 -PropertyType DWord -Force | Out-Null
    New-ItemProperty -Path $key -Name APOInterface0 -Value '{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}' -PropertyType String -Force | Out-Null
    Log "  ok: $key"
}

Log '=== verify ==='
foreach ($e in $entries) {
    $key = Join-Path $base $e.id
    Log ('--- ' + $key + ' ---')
    Get-ItemProperty -Path $key | Select-Object FriendlyName, Flags, NumAPOInterfaces, APOInterface0 | Format-List | Out-String | ForEach-Object { Log $_ }
}
Log 'DONE'