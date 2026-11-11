# Urfin stand: File Explorer driver for perf.py (S9-11, S9 3.3). PowerShell 5.1, ASCII only.
# No synthetic input (SendInput, keys, mouse): listing is observed through UI Automation,
# operations go through the Shell engine IFileOperation (the same engine Explorer uses).
# Prints one JSON line; times are QPC ticks (Stopwatch.GetTimestamp), freq is ticks per second.
#
#   -Mode list   -Path <dir> [-Expect N]   open a NEW Explorer window on <dir>, t_first = first
#                                          ListItem, t_done = status bar count reaches N (or stays
#                                          unchanged 1 s); the window is closed via WindowPattern
#                                          by its own handle; other windows are never touched.
#   -Mode copy   -Src <dir> -Dst <parent> -Name <new>   IFileOperation.CopyItem
#   -Mode move   -Src <dir> -Dst <parent> -Name <new>   IFileOperation.MoveItem
#   -Mode delete -Src <dir>                IFileOperation.DeleteItem, permanent (no FOF_ALLOWUNDO)
#   -Silent                                no progress dialog (FOF_SILENT); default shows it
#   -LingerMs N                            wait N ms after PerformOperations before printing: the
#                                          detector check sees no I/O of our own output/exit
#   -Mode probe                            check COM and UIA load, print {"ok":true}
param(
  [ValidateSet("list", "copy", "move", "delete", "probe")][string]$Mode = "probe",
  [string]$Path = "",
  [long]$Expect = -1,
  [string]$Src = "",
  [string]$Dst = "",
  [string]$Name = "",
  [switch]$Silent,
  [int]$TimeoutSec = 600,
  [int]$LingerMs = 0
)
$ErrorActionPreference = "Stop"
$Freq = [Diagnostics.Stopwatch]::Frequency

function Out-Json($h) { ($h | ConvertTo-Json -Compress) }

$cs = @"
using System;
using System.Runtime.InteropServices;

[ComImport, Guid("947aab5f-0a5c-4c13-b4d6-4bf7836fc9f8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IUrfinFileOperation {
    void Advise(IntPtr sink, out uint cookie);
    void Unadvise(uint cookie);
    void SetOperationFlags(uint flags);
    void SetProgressMessage([MarshalAs(UnmanagedType.LPWStr)] string msg);
    void SetProgressDialog(IntPtr dlg);
    void SetProperties(IntPtr props);
    void SetOwnerWindow(IntPtr hwnd);
    void ApplyPropertiesToItem(IntPtr item);
    void ApplyPropertiesToItems(IntPtr items);
    void RenameItem(IntPtr item, [MarshalAs(UnmanagedType.LPWStr)] string name, IntPtr sink);
    void RenameItems(IntPtr items, [MarshalAs(UnmanagedType.LPWStr)] string name);
    void MoveItem(IntPtr item, IntPtr dstFolder, [MarshalAs(UnmanagedType.LPWStr)] string name, IntPtr sink);
    void MoveItems(IntPtr items, IntPtr dstFolder);
    void CopyItem(IntPtr item, IntPtr dstFolder, [MarshalAs(UnmanagedType.LPWStr)] string name, IntPtr sink);
    void CopyItems(IntPtr items, IntPtr dstFolder);
    void DeleteItem(IntPtr item, IntPtr sink);
    void DeleteItems(IntPtr items);
    void NewItem(IntPtr dstFolder, uint attrs, [MarshalAs(UnmanagedType.LPWStr)] string name,
                 [MarshalAs(UnmanagedType.LPWStr)] string template, IntPtr sink);
    void PerformOperations();
    [return: MarshalAs(UnmanagedType.Bool)] bool GetAnyOperationsAborted();
}

public static class UrfinShellOp {
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    static extern void SHCreateItemFromParsingName(string path, IntPtr bc, ref Guid riid, out IntPtr item);

    static readonly Guid IID_IShellItem = new Guid("43826d1e-e718-42ee-bc55-a1e261c37bfe");
    static readonly Guid CLSID_FileOperation = new Guid("3ad05575-8857-4850-9277-11b85bdb8e09");

    public const uint FOF_SILENT = 0x4, FOF_NOCONFIRMATION = 0x10, FOF_NOCONFIRMMKDIR = 0x200,
        FOF_NOERRORUI = 0x400;

    static IntPtr Item(string path) {
        Guid iid = IID_IShellItem;
        IntPtr p;
        SHCreateItemFromParsingName(path, IntPtr.Zero, ref iid, out p);
        return p;
    }

    // Returns [t_start, t_end] ticks around PerformOperations and the aborted flag in [2].
    public static long[] Run(string mode, string src, string dst, string name, bool silent) {
        var op = (IUrfinFileOperation)Activator.CreateInstance(Type.GetTypeFromCLSID(CLSID_FileOperation));
        uint flags = FOF_NOCONFIRMATION | FOF_NOCONFIRMMKDIR | FOF_NOERRORUI;
        if (silent) flags |= FOF_SILENT;
        op.SetOperationFlags(flags);
        IntPtr s = Item(src), d = IntPtr.Zero;
        try {
            if (mode == "copy" || mode == "move") {
                d = Item(dst);
                if (mode == "copy") op.CopyItem(s, d, name, IntPtr.Zero);
                else op.MoveItem(s, d, name, IntPtr.Zero);
            } else if (mode == "delete") {
                op.DeleteItem(s, IntPtr.Zero);
            } else {
                throw new ArgumentException(mode);
            }
            long t0 = System.Diagnostics.Stopwatch.GetTimestamp();
            op.PerformOperations();
            long t1 = System.Diagnostics.Stopwatch.GetTimestamp();
            return new long[] { t0, t1, op.GetAnyOperationsAborted() ? 1 : 0 };
        } finally {
            Marshal.Release(s);
            if (d != IntPtr.Zero) Marshal.Release(d);
            Marshal.ReleaseComObject(op);
        }
    }
}
"@

function Invoke-ShellOp {
  Add-Type -TypeDefinition $cs -Language CSharp
  $r = [UrfinShellOp]::Run($Mode, $Src, $Dst, $Name, [bool]$Silent)
  if ($LingerMs -gt 0) { Start-Sleep -Milliseconds $LingerMs }
  Out-Json ([ordered]@{ ok = ($r[2] -eq 0); mode = $Mode; t_start = $r[0]; t_done = $r[1]; freq = $Freq; aborted = ($r[2] -ne 0) })
}

function Get-ExplorerWindows {
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ClassNameProperty, "CabinetWClass")
  $root.FindAll([System.Windows.Automation.TreeScope]::Children, $cond)
}

function Get-StatusCount($win) {
  # Status bar text like "Items: 97 556" / "Elementov: 97 556" (group separators vary).
  $sb = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::StatusBar)
  $bar = $win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $sb)
  if (-not $bar) { return -1 }
  $texts = $bar.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.Condition]::TrueCondition)
  foreach ($e in $texts) {
    $n = $e.Current.Name
    if ($n -match '(\d[\d\s,.]*)') {
      $digits = ($Matches[1] -replace '[^\d]', '')
      if ($digits) { return [long]$digits }
    }
  }
  return -1
}

function Invoke-List {
  Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
  $leaf = Split-Path -Leaf $Path
  $before = @{}
  foreach ($w in Get-ExplorerWindows) { $before[$w.Current.NativeWindowHandle] = $true }
  $t0 = [Diagnostics.Stopwatch]::GetTimestamp()
  Start-Process -FilePath "$env:WINDIR\explorer.exe" -ArgumentList "`"$Path`""
  $deadline = $t0 + $TimeoutSec * $Freq
  $win = $null; $tFirst = $null; $tDone = $null; $count = -1
  $lastCount = -1; $lastChange = $null
  $li = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::ListItem)
  try {
    while ([Diagnostics.Stopwatch]::GetTimestamp() -lt $deadline) {
      if (-not $win) {
        foreach ($w in Get-ExplorerWindows) {
          $h = $w.Current.NativeWindowHandle
          if (-not $before.ContainsKey($h) -and $w.Current.Name -like "*$leaf*") { $win = $w; break }
        }
      }
      if ($win) {
        if (-not $tFirst) {
          if ($win.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $li)) {
            $tFirst = [Diagnostics.Stopwatch]::GetTimestamp()
          }
        }
        $now = [Diagnostics.Stopwatch]::GetTimestamp()
        $c = Get-StatusCount $win
        if ($c -ne $lastCount) { $lastCount = $c; $lastChange = $now }
        if ($tFirst -and $c -ge 0) {
          if ($Expect -ge 0 -and $c -eq $Expect) { $tDone = $now; $count = $c; break }
          if ($Expect -lt 0 -and ($now - $lastChange) -ge $Freq) { $tDone = $lastChange; $count = $c; break }
        }
      }
      Start-Sleep -Milliseconds 20
    }
  } finally {
    if ($win) {
      try {
        $wp = $win.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern)
        $wp.Close()
      } catch { }
    }
  }
  Out-Json ([ordered]@{ ok = [bool]$tDone; mode = "list"; t_start = $t0; t_first = $tFirst; t_done = $tDone;
      count = $count; last_count = $lastCount; freq = $Freq; window = [bool]$win })
}

try {
  switch ($Mode) {
    "list" { Invoke-List }
    "probe" {
      Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
      Add-Type -TypeDefinition $cs -Language CSharp
      Out-Json ([ordered]@{ ok = $true; mode = "probe"; freq = $Freq })
    }
    default { Invoke-ShellOp }
  }
} catch {
  Out-Json ([ordered]@{ ok = $false; mode = $Mode; error = $_.Exception.Message })
  exit 1
}
