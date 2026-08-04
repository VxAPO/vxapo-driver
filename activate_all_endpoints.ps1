$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$out = 'D:\APO_Project\VxAPO\vxapo-driver\activate_all_out.txt'
Set-Content -Path $out -Value '' -Encoding UTF8
function Log($msg) { Add-Content -Path $out -Value $msg -Encoding UTF8 }

$t = @'
using System;
using System.Runtime.InteropServices;

public static class EndpointActivator
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

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-CC5392387B5E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceCollection
    {
        void GetCount(out uint count);
        void Item(uint index, out IntPtr device);
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

    public static IntPtr _firstClient;

    public static int ActivateAllRender()
    {
        var enumerator = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
        IntPtr devices;
        // eRender=0, state mask=1 (ACTIVE)
        enumerator.EnumAudioEndpoints(0, 1, out devices);

        var coll = (IMMDeviceCollection)Marshal.GetObjectForIUnknown(devices);
        uint count;
        coll.GetCount(out count);

        int activated = 0;
        for (uint i = 0; i < count; i++) {
            IntPtr device;
            coll.Item(i, out device);
            if (device == IntPtr.Zero) continue;
            var dev = (IMMDevice)Marshal.GetObjectForIUnknown(device);
            Guid iidClient = new Guid("{1CB9AD4C-DBFA-4C32-B178-C2F568A703B2}");
            IntPtr pClient;
            try {
                dev.Activate(ref iidClient, 1, IntPtr.Zero, out pClient);
                if (pClient == IntPtr.Zero) continue;
                var client = (IAudioClient)Marshal.GetObjectForIUnknown(pClient);
                client.Initialize(0, 0, 20000000, 0, IntPtr.Zero, IntPtr.Zero);
                client.Start();
                if (activated == 0) _firstClient = pClient; // keep at least one alive
                activated++;
            } catch {
                // endpoint may not support audio client
            }
        }
        return activated;
    }
}
'@

Add-Type -TypeDefinition $t -Language CSharp

Log '=== activate all render endpoints ==='
try {
    $activated = [EndpointActivator]::ActivateAllRender()
    Log ("activated endpoints: {0}" -f $activated)
} catch {
    Log ("activation failed: {0}" -f $_.Exception.Message)
}

# wait for graph build, sample audiodg modules
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