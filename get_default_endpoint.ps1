$ErrorActionPreference = 'SilentlyContinue'
chcp 936 > $null
$out = 'D:\APO_Project\VxAPO\vxapo-driver\default_endpoint_out.txt'
Set-Content -Path $out -Value ''

function Log($msg) {
    Write-Host $msg
    Add-Content -Path $out -Value $msg
}

$t = @'
using System;
using System.Runtime.InteropServices;

public static class DefaultEndpoint
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
        void OpenPropertyStore(int stgmAccess, out IntPtr ppProperties);
    }

    [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        void GetCount(out uint cProps);
        void GetAt(uint iProp, out PropertyKey pkey);
        void GetValue(ref PropertyKey key, out PropVariant pv);
        void SetValue(ref PropertyKey key, ref PropVariant pv);
        void Commit();
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PropertyKey
    {
        public Guid fmtid;
        public uint pid;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct PropVariant
    {
        [FieldOffset(0)] public ushort vt;
        [FieldOffset(8)] public IntPtr pszVal;
    }

    public static string GetDefaultRenderId()
    {
        var enumerator = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
        IntPtr device;
        enumerator.GetDefaultAudioEndpoint(0, 0, out device); // eRender=0, eConsole=0
        if (device == IntPtr.Zero) return "NO_DEFAULT";
        // 通过 PKEY_Device_FriendlyName 获取名称
        var dev = (IMMDevice)Marshal.GetObjectForIUnknown(device);
        IntPtr pProps;
        dev.OpenPropertyStore(1 /*STGM_READ*/, out pProps);
        var store = (IPropertyStore)Marshal.GetObjectForIUnknown(pProps);
        // PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, 14
        var key = new PropertyKey { fmtid = new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"), pid = 14 };
        PropVariant pv;
        store.GetValue(ref key, out pv);
        return Marshal.PtrToStringUni(pv.pszVal) ?? "NO_NAME";
    }
}
'@
Add-Type -TypeDefinition $t -Language CSharp
try {
    $name = [DefaultEndpoint]::GetDefaultRenderId()
    Log ("DEFAULT_RENDER = {0}" -f $name)
} catch {
    Log ("FAILED: {0}" -f $_.Exception.Message)
}

# 对照注册表 role-4
Log '--- registry role-4 (粗对照) ---'
& reg query 'HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render' /s 2>&1 |
    Out-String |
    ForEach-Object { Log $_ }

Log '--- DONE ---'