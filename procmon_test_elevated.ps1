$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\procmon_test_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) {
    Write-Host $msg
    Add-Content -Path $out -Value $msg -Encoding UTF8
}

$procmon = 'D:\360极速浏览器X下载\ProcessMonitor\Procmon64.exe'
$pml = 'D:\APO_Project\VxAPO\vxapo-driver\procmon_test2.pml'
if (Test-Path $pml) { Remove-Item $pml -Force }

Log "starting Procmon from elevated shell..."

# 直接 & 调用（非 Start-Process），参数拼接为单字符串避免数组解析
$argsLine = '/AcceptEula /Quiet /BackingFile ' + $pml + ' /Runtime 5'
Log "args: $argsLine"
$p = Start-Process -FilePath $procmon -ArgumentList $argsLine -PassThru
if ($null -eq $p) {
    Log 'PROCMON_START_FAILED_NULL'
} else {
    Log ("pid={0} hasExited={1}" -f $p.Id, $p.HasExited)
}

Start-Sleep -Seconds 12

$pmlExists = Test-Path $pml
Log ("after 12s: pml exists={0}" -f $pmlExists)
if ($pmlExists) {
    Log ("pml size={0}" -f (Get-Item $pml).Length)
}
Log 'DONE'