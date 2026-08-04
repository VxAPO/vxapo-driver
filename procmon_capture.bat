@echo off
cd /d D:\ProcessMonitor
del /q C:\ProgramData\VxAPO\procmon_vxapo.pml 2>nul
del /q C:\ProgramData\VxAPO\procmon_vxapo.csv 2>nul

echo [1/4] start Procmon capture...
start "" Procmon64.exe /AcceptEula /Quiet /Minimized /BackingFile C:\ProgramData\VxAPO\procmon_vxapo.pml /Runtime 40

timeout /t 3 /nobreak >nul

echo [2/4] restart audio service...
net stop audiosrv >nul 2>&1
timeout /t 2 /nobreak >nul
net start audiosrv >nul 2>&1

echo [3/4] wait 40s for capture...
timeout /t 40 /nobreak >nul

echo [4/4] convert PML to CSV...
Procmon64.exe /OpenLog C:\ProgramData\VxAPO\procmon_vxapo.pml /SaveAs C:\ProgramData\VxAPO\procmon_vxapo.csv

timeout /t 10 /nobreak >nul
dir C:\ProgramData\VxAPO\procmon_vxapo.pml
dir C:\ProgramData\VxAPO\procmon_vxapo.csv
echo DONE