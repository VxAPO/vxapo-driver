$ErrorActionPreference = 'SilentlyContinue'

$out = 'D:\APO_Project\VxAPO\vxapo-driver\audiodg_check_out.txt'
$err = 'D:\APO_Project\VxAPO\vxapo-driver\audiodg_check_err.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
Set-Content -Path $err -Value '' -Encoding UTF8

function Log($msg) {
    Write-Host $msg
    Add-Content -Path $out -Value $msg -Encoding UTF8
}

Log ('=== start: {0} ===' -f (Get-Date -Format 'HH:mm:ss'))

# 预取 audiodg（若已存活则保留原进程）
$procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
if ($procs.Count -gt 0) {
    Log ("existing audiodg PID: {0}" -f $procs[0].Id)
}

# C# COM：激活默认渲染端点 IAudioClient -> Initialize(shared) -> Start()
# 静态字段持有流引用，避免 GC 释放导致流断
$t2 = @'
using System;
using System.Runtime.InteropServices;

public static class AudioStreamKeep
{
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    private class MMDeviceEnumeratorComObject { }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        void EnumAudioEndpoints(int dataFlow, int dwStateMask, out IntPtr devices);
        void GetDefaultAudioEndpoint(int dataFlow, int role, out IntPtr device);
        void GetDevice(string id, out IntPtr device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        void Activate(ref Guid iid, int dwClsCtx, IntPtr pActivationParams, out IntPtr pInterface);
    }

    [ComImport, Guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioClient
    {
        void Initialize(int shareMode, int streamFlags, long hnsBufferDuration, long hnsPeriodicity, IntPtr pFormat, IntPtr audioSessionGuid);
        void GetBufferSize(out uint numFrames);
        void GetStreamLatency(out long latency);
        void GetCurrentPadding(out uint padding);
        void IsFormatSupported(int shareMode, IntPtr pFormat, IntPtr ppClosestMatch);
        void GetMixFormat(IntPtr ppDeviceFormat);
        void GetDevicePeriod(IntPtr phnsDefaultDevicePeriod, IntPtr phnsMinimumDevicePeriod);
        void Start();
        void Stop();
        void Reset();
        void SetEventHandle(IntPtr eventHandle);
        void GetService(ref Guid riid, out IntPtr ppService);
    }

    private static IntPtr _client;

    public static bool StartRenderStream()
    {
        var enumerator = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
        IntPtr device;
        enumerator.GetDefaultAudioEndpoint(0, 0, out device); // eRender=0, eConsole=0
        if (device == IntPtr.Zero) return false;
        var dev = (IMMDevice)Marshal.GetObjectForIUnknown(device);
        Guid iidClient = new Guid("{1CB9AD4C-DBFA-4C32-B178-C2F568A703B2}");
        IntPtr pClient;
        dev.Activate(ref iidClient, 1 /*CLSCTX_INPROC_SERVER*/, IntPtr.Zero, out pClient);
        if (pClient == IntPtr.Zero) return false;
        var client = (IAudioClient)Marshal.GetObjectForIUnknown(pClient);
        // shared mode, no flags, 1s buffer, format=null (use mix format)
        client.Initialize(0, 0, 10000000, 0, IntPtr.Zero, IntPtr.Zero);
        client.Start();
        _client = pClient; // keep alive
        return true;
    }
}
'@
Add-Type -TypeDefinition $t2 -Language CSharp

try {
    $ok = [AudioStreamKeep]::StartRenderStream()
    Log ("audio stream started: {0}" -f $ok)
} catch {
    Log ("audio start failed: {0}" -f $_.Exception.Message)
}

# 播放系统提示音触发真实音频流（不 Initialize 的 Activate 不足以维持 audiodg/实例化 APO）
Log '=== play system sound ==='
try {
    [System.Media.SystemSounds]::Asterisk.Play()
    Log 'asterisk played'
} catch {
    Log ("play failed: {0}" -f $_.Exception.Message)
}

# 每 2 秒采样一次，共 4 次（总 8 秒流保持期）
for ($n = 1; $n -le 4; $n++) {
    Start-Sleep -Seconds 2
    $procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) {
        Log 'AUDIODG_EXITED'
        break
    }
    $proc = $procs[0]
    $mods = $proc.Modules
    Log ("--- sample {0} @ {1} PID={2} total={3} ---" -f $n, (Get-Date -Format 'HH:mm:ss'), $proc.Id, $mods.Count)
    $hits = $mods | Where-Object { $_.ModuleName -match 'vxapo|eapo|equalizer|41C34613|B4A97313' }
    if ($hits) {
        Log '=== VXAPO_OR_EAPO_HIT ==='
        $hits | ForEach-Object { Log ("{0}  {1}" -f $_.ModuleName, $_.FileName) }
    } else {
        Log 'NO_VXAPO_NO_EAPO_IN_MODULES'
    }
    $eng = $mods | Where-Object { $_.ModuleName -match 'audioeng' }
    if ($eng) {
        Log ("audioeng.dll present: {0}" -f $eng[0].FileName)
    } else {
        Log 'audioeng.dll NOT loaded'
    }
    # 完整模块名列表（判断引擎/APO 是否初始化）
    $names = ($mods | ForEach-Object { $_.ModuleName }) -join ', '
    Log ("modules: {0}" -f $names)
}

Log ('=== end: {0} ===' -f (Get-Date -Format 'HH:mm:ss'))