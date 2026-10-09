param([string]$Out, [string]$Title = "Dumb Engine", [int]$ProcId = 0)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
if ($ProcId -gt 0) { $p = Get-Process -Id $ProcId } else { $p = Get-Process | Where-Object { $_.MainWindowTitle -like "*$Title*" } | Select-Object -First 1 }
if (-not $p) { Write-Output "window not found"; exit 1 }
$h = $p.MainWindowHandle
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
[W]::PrintWindow($h, $hdc, 2) | Out-Null
$g.ReleaseHdc($hdc)
$scale = [Math]::Min(1.0, 1600.0 / $w)
$nw = [int]($w * $scale); $nh = [int]($hh * $scale)
$small = New-Object System.Drawing.Bitmap $bmp, $nw, $nh
$small.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $Out ($w x $hh)"
