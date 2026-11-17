Add-Type @'
using System;
using System.Runtime.InteropServices;
public class MemPurge {
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr h, uint acc, out IntPtr tok);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool LookupPrivilegeValue(string host, string name, out long luid);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool AdjustTokenPrivileges(IntPtr tok, bool dis, ref TOKEN_PRIVILEGES np, int len, IntPtr prev, IntPtr rl);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
  [DllImport("ntdll.dll")] static extern int NtSetSystemInformation(int cls, ref int info, int len);
  [StructLayout(LayoutKind.Sequential, Pack=4)] struct TOKEN_PRIVILEGES { public int Count; public long Luid; public int Attr; }
  public static string Purge() {
    IntPtr tok;
    if (!OpenProcessToken(GetCurrentProcess(), 0x28, out tok)) return "token_err=" + Marshal.GetLastWin32Error();
    long luid;
    if (!LookupPrivilegeValue(null, "SeProfileSingleProcessPrivilege", out luid)) return "lookup_err=" + Marshal.GetLastWin32Error();
    TOKEN_PRIVILEGES tp = new TOKEN_PRIVILEGES(); tp.Count = 1; tp.Luid = luid; tp.Attr = 2; // SE_PRIVILEGE_ENABLED
    if (!AdjustTokenPrivileges(tok, false, ref tp, 0, IntPtr.Zero, IntPtr.Zero)) return "adjust_err=" + Marshal.GetLastWin32Error();
    int err = Marshal.GetLastWin32Error();
    int cmd = 4;
    int rc = NtSetSystemInformation(80, ref cmd, 4);
    return "adjust_last_err=" + err + " purge_rc=" + rc;
  }
}
'@
[MemPurge]::Purge()
