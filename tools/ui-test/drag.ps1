param([double]$X1, [double]$Y1, [double]$X2, [double]$Y2, [string]$Title = "Dumb Engine")
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class D {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
$p = Get-Process | Where-Object { $_.MainWindowTitle -like "*$Title*" } | Select-Object -First 1
$h = $p.MainWindowHandle
[D]::SetForegroundWindow($h) | Out-Null
$r = New-Object D+RECT
[D]::GetWindowRect($h, [ref]$r) | Out-Null
$s = [Math]::Min(1.0, 1600.0 / ($r.R - $r.L))
function P($x, $y) { [D]::SetCursorPos([int]($r.L + $x / $s), [int]($r.T + $y / $s)) | Out-Null }
P $X1 $Y1; Start-Sleep -Milliseconds 300
[D]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 100
for ($i = 1; $i -le 20; $i++) { P ($X1 + ($X2 - $X1) * $i / 20) ($Y1 + ($Y2 - $Y1) * $i / 20); Start-Sleep -Milliseconds 25 }
Start-Sleep -Milliseconds 100
[D]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
Write-Output "dragged"
