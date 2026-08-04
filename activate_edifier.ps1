$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\edifier_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

$t = @'
using System;
using System.Runtime.InteropServices;

public static class EdifierActivate
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
        void GetMixFormat(out IntPtr ppDeviceFormat);
        void GetDevicePeriod(out long phnsDefaultDevicePeriod, out long phnsMinimumDevicePeriod);
        void Start();
        void Stop();
        void Reset();
        void SetEventHandle(IntPtr eventHandle);
        void GetService(ref Guid riid, out IntPtr ppService);
    }

    private static IntPtr _client;

    // endpointId like "{3b1c3cb8-af9e-47b9-b776-3dac8c7ca333}"
    public static bool ActivateEndpoint(string endpointId)
    {
        var enumerator = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
        IntPtr device;
        enumerator.GetDevice(endpointId, out device);
        if (device == IntPtr.Zero) return false;
        var dev = (IMMDevice)Marshal.GetObjectForIUnknown(device);
        Guid iidClient = new Guid("{1CB9AD4C-DBFA-4C32-B178-C2F568A703B2}");
        IntPtr pClient;
        dev.Activate(ref iidClient, 1 /*CLSCTX_INPROC_SERVER*/, IntPtr.Zero, out pClient);
        if (pClient == IntPtr.Zero) return false;
        var client = (IAudioClient)Marshal.GetObjectForIUnknown(pClient);
        // shared mode, 2s buffer, format=null => use mix format
        client.Initialize(0, 0, 20000000, 0, IntPtr.Zero, IntPtr.Zero);
        client.Start();
        _client = pClient; // keep alive
        return true;
    }
}
'@

Add-Type -TypeDefinition $t -Language CSharp

Log '=== start ==='
try {
    $ok = [EdifierActivate]::ActivateEndpoint("{3b1c3cb8-af9e-47b9-b776-3dac8c7ca333}")
    Log ("activate edifier: {0}" -f $ok)
} catch {
    Log ("activate failed: {0}" -f $_.Exception.Message)
}

# sample audiodg modules every 2s x 6 while stream held
for ($n = 1; $n -le 6; $n++) {
    Start-Sleep -Seconds 2
    $procs = @(Get-Process audiodg -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) { Log 'AUDIODG_EXITED'; break }
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
Log '=== end ==='