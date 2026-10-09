# Click inside the Dumb Engine window. Coordinates are in the 1600px-wide screenshot space.
param([double]$X, [double]$Y, [string]$Button = "left", [switch]$Double, [string]$Title = "Dumb Engine", [string]$Keys = "")
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class M {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
$p = Get-Process | Where-Object { $_.MainWindowTitle -like "*$Title*" } | Select-Object -First 1
$h = $p.MainWindowHandle
[M]::SetForegroundWindow($h) | Out-Null
$r = New-Object M+RECT
[M]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L
$scale = [Math]::Min(1.0, 1600.0 / $w)
$sx = [int]($r.L + $X / $scale); $sy = [int]($r.T + $Y / $scale)
[M]::SetCursorPos($sx, $sy) | Out-Null
Start-Sleep -Milliseconds 120
$down = 2; $up = 4
if ($Button -eq "right") { $down = 8; $up = 16 }
$n = 1; if ($Double) { $n = 2 }
for ($i = 0; $i -lt $n; $i++) {
  [M]::mouse_event($down, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40
  [M]::mouse_event($up, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60
}
if ($Keys) { Add-Type -AssemblyName System.Windows.Forms; Start-Sleep -Milliseconds 100; [System.Windows.Forms.SendKeys]::SendWait($Keys) }
Write-Output "clicked $sx,$sy"
