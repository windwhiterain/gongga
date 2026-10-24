# Computer use for Windows: screenshot, UI tree, mouse and keyboard.
#
# Two things shape this file:
#   * Every command re-declares DPI awareness, because a non-aware process sees a
#     scaled desktop while UIA reports physical pixels -- a click would then land
#     somewhere other than the pixel seen in the screenshot.
#   * Input is built in C#, not PowerShell: assigning into a nested struct field
#     ($input.u.ki.wVk = ...) writes to a temporary copy and is discarded, which
#     makes SendInput accept the events and deliver nothing.
$ErrorActionPreference = 'Stop'

# Arguments arrive through CU_ARGV, one per unit-separator byte, because
# PowerShell's own parser would claim '--window' as a parameter of this script.
# Trimming with a slice would loop forever when one argument remains: in
# PowerShell $a[0..-1] is the whole array, not empty. Remove trailing empties by
# index instead.
$all = New-Object System.Collections.ArrayList
foreach ($piece in ($env:CU_ARGV -split [char]0x1f)) { [void]$all.Add($piece) }
while ($all.Count -gt 0 -and $all[$all.Count - 1] -eq '') { $all.RemoveAt($all.Count - 1) }
if ($all.Count -eq 0) { [void]$all.Add('help') }
$Cmd = $all[0]
$rest = @(); if ($all.Count -gt 1) { $rest = @($all.GetRange(1, $all.Count - 1)) }

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class Cu {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, int d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] i, int size);

  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr extra; }
  // On x64 the union starts at offset 8: 4 bytes of type, then alignment padding.
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
  }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }

  const uint KEYBOARD = 1, KEYUP = 0x0002, UNICODE = 0x0004;

  // Sends text as UTF-16 code units, so Chinese and punctuation arrive intact
  // without depending on the keyboard layout.
  public static uint TypeText(string text) {
    List<INPUT> list = new List<INPUT>();
    foreach (char ch in text) {
      INPUT down = new INPUT(); down.type = KEYBOARD; down.u.ki.wScan = ch; down.u.ki.dwFlags = UNICODE; list.Add(down);
      INPUT up = new INPUT(); up.type = KEYBOARD; up.u.ki.wScan = ch; up.u.ki.dwFlags = UNICODE | KEYUP; list.Add(up);
    }
    INPUT[] arr = list.ToArray();
    return SendInput((uint)arr.Length, arr, Marshal.SizeOf(typeof(INPUT)));
  }

  // Modifiers go down in order and up in reverse, which is what a human does.
  public static uint SendVks(ushort[] vks) {
    List<INPUT> list = new List<INPUT>();
    foreach (ushort vk in vks) { INPUT d = new INPUT(); d.type = KEYBOARD; d.u.ki.wVk = vk; list.Add(d); }
    for (int i = vks.Length - 1; i >= 0; i--) {
      INPUT u = new INPUT(); u.type = KEYBOARD; u.u.ki.wVk = vks[i]; u.u.ki.dwFlags = KEYUP; list.Add(u);
    }
    INPUT[] arr = list.ToArray();
    return SendInput((uint)arr.Length, arr, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
[void][Cu]::SetProcessDPIAware()

# Windows refuses SetForegroundWindow from a process that is not already the
# foreground one, which would send later keystrokes to whatever window the user
# is looking at. Sharing the foreground thread's input queue lifts that
# restriction, and the caller still verifies the result instead of trusting it.
function Force-Foreground([IntPtr]$h) {
  if ([Cu]::GetForegroundWindow() -eq $h) { return $true }
  if ([Cu]::IsIconic($h)) { [void][Cu]::ShowWindowAsync($h, 9) }
  $fg = [Cu]::GetForegroundWindow()
  $fgThread = 0
  if ($fg -ne [IntPtr]::Zero) { $fgThread = [Cu]::GetWindowThreadProcessId($fg, [IntPtr]::Zero) }
  $myThread = [Cu]::GetCurrentThreadId()
  $attached = $false
  if ($fgThread -ne 0 -and $fgThread -ne $myThread) {
    $attached = [Cu]::AttachThreadInput($myThread, $fgThread, $true)
  }
  [void][Cu]::BringWindowToTop($h)
  [void][Cu]::ShowWindowAsync($h, 5)
  [void][Cu]::SetForegroundWindow($h)
  if ($attached) { [void][Cu]::AttachThreadInput($myThread, $fgThread, $false) }
  Start-Sleep -Milliseconds 150
  return ([Cu]::GetForegroundWindow() -eq $h)
}

function Get-Windows {
  Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::Window)
  $found = @()
  foreach ($w in $root.FindAll([System.Windows.Automation.TreeScope]::Children, $cond)) {
    $hwnd = $w.Current.NativeWindowHandle
    if ($hwnd -eq 0) { continue }
    $r = New-Object Cu+RECT
    if (-not [Cu]::GetWindowRect([IntPtr]$hwnd, [ref]$r)) { continue }
    if (($r.Right - $r.Left) -le 0) { continue }
    $found += [pscustomobject]@{
      Hwnd = $hwnd; Pid = $w.Current.ProcessId; Title = $w.Current.Name
      X = $r.Left; Y = $r.Top
      W = $r.Right - $r.Left; H = $r.Bottom - $r.Top
    }
  }
  return $found
}

function Resolve-Window([string]$spec) {
  $wins = Get-Windows
  if (-not $spec) { return $null }
  if ($spec -match '^\d+$') {
    foreach ($w in $wins) { if ("$($w.Hwnd)" -eq $spec) { return $w } }
  }
  $hits = @($wins | Where-Object { $_.Title -like "*$spec*" })
  if ($hits.Count -eq 1) { return $hits[0] }
  if ($hits.Count -gt 1) {
    $listing = ($hits | ForEach-Object { "  $($_.Hwnd)  $($_.Title)" }) -join "`n"
    throw "window '$spec' is ambiguous, use a hwnd:`n$listing"
  }
  throw "no window matches '$spec' (run: cu windows)"
}

function Get-Arg([string]$name, [string]$default) {
  for ($i = 0; $i -lt $rest.Count - 1; $i++) { if ($rest[$i] -eq $name) { return $rest[$i + 1] } }
  return $default
}

function Get-Flag([string]$name) { return ($rest -contains $name) }

# Positional words, with flags and their values removed.
function Get-Words {
  $vals = @()
  for ($i = 0; $i -lt $rest.Count; $i++) {
    $t = $rest[$i]
    if ($t -eq '--window') { $i++; continue }
    if ($t -match '^--') { continue }
    $vals += $t
  }
  return $vals
}

# Every input action takes --window so it can claim the foreground first.
# Without it, keystrokes land in whatever the user happens to have in front.
function Claim-Target {
  $spec = Get-Arg '--window' $null
  if (-not $spec) {
    $fg = [Cu]::GetForegroundWindow()
    $title = ''
    if ($fg -ne [IntPtr]::Zero) {
      $el = [System.Windows.Automation.AutomationElement]::FromHandle($fg)
      $title = $el.Current.Name
    }
    Write-Output "target: foreground hwnd=$fg `"$title`""
    return
  }
  $w = Resolve-Window $spec
  if (-not (Force-Foreground ([IntPtr]$w.Hwnd))) {
    throw "could not focus $($w.Hwnd) `"$($w.Title)`"; refusing to send input blind"
  }
  Write-Output "target: $($w.Hwnd) `"$($w.Title)`""
}

function Invoke-Shot {
  Add-Type -AssemblyName System.Windows.Forms,System.Drawing
  $scale = [double](Get-Arg '--scale' '1')
  $out = Get-Arg '--out' (Join-Path $env:TEMP ("cu-" + [guid]::NewGuid().ToString('N').Substring(0, 8) + ".png"))
  $winSpec = Get-Arg '--window' $null
  if ($winSpec) {
    $w = Resolve-Window $winSpec
    if ([Cu]::IsIconic([IntPtr]$w.Hwnd)) { throw "window $($w.Hwnd) is minimized; run: cu focus $($w.Hwnd)" }
    $src = New-Object System.Drawing.Rectangle $w.X, $w.Y, $w.W, $w.H
  } else {
    $vs = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $src = New-Object System.Drawing.Rectangle $vs.Left, $vs.Top, $vs.Width, $vs.Height
  }
  $bmp = New-Object System.Drawing.Bitmap $src.Width, $src.Height
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($src.Left, $src.Top, 0, 0, $bmp.Size)
  $g.Dispose()
  if ($scale -ne 1) {
    $nw = [int]($src.Width * $scale); $nh = [int]($src.Height * $scale)
    $small = New-Object System.Drawing.Bitmap $nw, $nh
    $g2 = [System.Drawing.Graphics]::FromImage($small)
    $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g2.DrawImage($bmp, 0, 0, $nw, $nh)
    $g2.Dispose(); $bmp.Dispose(); $bmp = $small
  }
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $h = $bmp.Height; $w2 = $bmp.Width; $bmp.Dispose()
  # Screen coordinates stay unscaled in the note, so a click computed from a
  # shrunk image can be divided back by the same factor.
  [Console]::Error.WriteLine("captured ${w2}x${h} scale=$scale screen=$($src.Width)x$($src.Height) origin=$($src.Left),$($src.Top)")
  # The leading @ is the harness convention for "attach this image"; printing it
  # here turns a capture into one step instead of capture-then-show.
  Write-Output "@$out"
}

function Invoke-Tree {
  Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
  $depth = [int](Get-Arg '--depth' '5')
  $max = [int](Get-Arg '--max' '150')
  $winSpec = Get-Arg '--window' $null
  if ($winSpec) { $el = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr](Resolve-Window $winSpec).Hwnd) }
  else {
    $h = [Cu]::GetForegroundWindow()
    if ($h -eq [IntPtr]::Zero) { throw "no foreground window" }
    $el = [System.Windows.Automation.AutomationElement]::FromHandle($h)
  }
  $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
  $script:emitted = 0
  $script:lines = New-Object System.Collections.ArrayList
  function Visit($node, [int]$d) {
    if ($d -gt $depth -or $script:emitted -ge $max) { return }
    $ch = $walker.GetFirstChild($node)
    while ($null -ne $ch) {
      if ($script:emitted -ge $max) { return }
      $c = $ch.Current
      $r = $c.BoundingRectangle
      # Some containers expose neither a name nor a control type under the
      # control view; they still matter as structure, so fall back rather than
      # letting a null dereference kill the whole walk.
      $name = [string]$c.Name
      if ($name.Length -gt 60) { $name = $name.Substring(0, 57) + '...' }
      $type = 'Unknown'
      if ($null -ne $c.ControlType) { $type = $c.ControlType.ProgrammaticName.Replace('ControlType.', '') }
      # An element with no on-screen box reports an infinite rectangle, which
      # cannot become an integer; it is shown as ? rather than crashing the walk.
      $rx = '?'; $ry = '?'; $rw = '?'; $rh = '?'
      if (-not [double]::IsInfinity($r.X) -and -not [double]::IsNaN($r.X)) {
        $rx = [string][int]$r.X; $ry = [string][int]$r.Y
        $rw = [string][int]$r.Width; $rh = [string][int]$r.Height
      }
      if ($name -or $type -match 'Button|Edit|MenuItem|TabItem|ListItem|CheckBox|RadioButton|ComboBox|Document|Hyperlink') {
        [void]$script:lines.Add(("  " * $d) + "$type `"$name`" @$rx,$ry ${rw}x$rh")
        $script:emitted++
      }
      Visit $ch ($d + 1)
      $ch = $walker.GetNextSibling($ch)
    }
  }
  Write-Output "window `"$($el.Current.Name)`" hwnd=$($el.Current.NativeWindowHandle) pid=$($el.Current.ProcessId)"
  Visit $el 1
  $script:lines
  if ($script:emitted -ge $max) { Write-Output "!! truncated at $max elements; narrow with --depth" }
}

function Invoke-Click {
  $words = Get-Words
  $pos = @($words | Where-Object { $_ -match '^-?\d+$' })
  if ($pos.Count -lt 2) { throw "usage: cu click X Y [--double] [--right] [--window W]" }
  Claim-Target | Out-Host
  $x = [int]$pos[0]; $y = [int]$pos[1]
  [void][Cu]::SetCursorPos($x, $y)
  Start-Sleep -Milliseconds 40
  if (Get-Flag '--right') { $d = 0x0010; $u = 0x0020 } else { $d = 0x0002; $u = 0x0004 }
  $times = 1; if (Get-Flag '--double') { $times = 2 }
  for ($i = 0; $i -lt $times; $i++) {
    [Cu]::mouse_event($d, 0, 0, 0, [UIntPtr]::Zero)
    [Cu]::mouse_event($u, 0, 0, 0, [UIntPtr]::Zero)
    if ($times -gt 1) { Start-Sleep -Milliseconds 60 }
  }
  Write-Output "clicked $x,$y"
}

function Invoke-Type {
  $text = (Get-Words) -join ' '
  if (-not $text) { throw "usage: cu type [--window W] <text>" }
  Claim-Target | Out-Host
  $want = $text.Length * 2
  $sent = [Cu]::TypeText($text)
  Write-Output "typed $($text.Length) chars (delivered $sent/$want events)"
  if ($sent -ne $want) { throw "SendInput dropped events (delivered $sent of $want)" }
}

function Invoke-Key {
  $spec = ((Get-Words) -join '').ToLower()
  if (-not $spec) { throw "usage: cu key [--window W] ctrl+s" }
  Claim-Target | Out-Host
  $map = @{ 'ctrl' = 0x11; 'shift' = 0x10; 'alt' = 0x12; 'win' = 0x5B
            'enter' = 0x0D; 'tab' = 0x09; 'esc' = 0x1B; 'space' = 0x20
            'backspace' = 0x08; 'delete' = 0x2E; 'up' = 0x26; 'down' = 0x28
            'left' = 0x25; 'right' = 0x27; 'home' = 0x24; 'end' = 0x23
            'pageup' = 0x21; 'pagedown' = 0x22; 'insert' = 0x2D
            'printscreen' = 0x2C; 'capslock' = 0x14 }
  # F1..F24 are contiguous virtual keys, so they are cheaper to compute than list.
  for ($n = 1; $n -le 24; $n++) { $map["f$n"] = 0x6F + $n }
  $vks = @()
  foreach ($p in ($spec -split '\+')) {
    if ($map.ContainsKey($p)) { $vks += [System.UInt16]$map[$p] }
    elseif ($p.Length -eq 1) { $vks += [System.UInt16][int][char]$p.ToUpper() }
    else { throw "unknown key '$p' (known: ctrl shift alt win f1-f24 enter tab esc space backspace delete up down left right home end pageup pagedown insert printscreen, or a single letter)" }
  }
  $want = $vks.Count * 2
  $sent = [Cu]::SendVks([System.UInt16[]]$vks)
  Write-Output "sent $spec (delivered $sent/$want events)"
  if ($sent -ne $want) { throw "SendInput dropped events (delivered $sent of $want)" }
}

function Invoke-Focus {
  $w = Resolve-Window ((Get-Words) -join ' ')
  if (-not (Force-Foreground ([IntPtr]$w.Hwnd))) {
    throw "could not bring $($w.Hwnd) `"$($w.Title)`" to the foreground; Windows kept another window in front"
  }
  Write-Output "focused $($w.Hwnd) `"$($w.Title)`""
}

function Invoke-Doctor {
  Add-Type -AssemblyName System.Windows.Forms,System.Drawing
  $ok = $true
  $vs = [System.Windows.Forms.SystemInformation]::VirtualScreen
  Write-Output "desktop: $($vs.Width)x$($vs.Height) (DPI aware, so these are physical pixels)"
  if ($vs.Width -le 0 -or $vs.Height -le 0) { Write-Output "  !! no visible desktop; input would go nowhere"; $ok = $false }

  try {
    Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
    $n = (Get-Windows).Count
    Write-Output "uiautomation: ok, $n top-level windows visible"
    if ($n -eq 0) { Write-Output "  ?? no windows found; the session may be locked" }
  } catch { Write-Output "uiautomation: FAILED ($_)"; $ok = $false }

  $bmp = New-Object System.Drawing.Bitmap 200, 200
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($vs.Left, $vs.Top, 0, 0, $bmp.Size)
  $g.Dispose()
  $colors = $bmp.GetPixel(10, 10), $bmp.GetPixel(50, 120), $bmp.GetPixel(150, 180)
  $bmp.Dispose()
  $distinct = ($colors | ForEach-Object { $_.ToString() } | Select-Object -Unique).Count
  Write-Output "screen capture: ok (sampled $distinct distinct colors)"
  if ($distinct -le 1) { Write-Output "  ?? uniform pixels; a locked or headless session looks like this" }

  $size = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Cu+INPUT])
  # 4 bytes of type, padding to pointer alignment, then a 32-byte union on x64.
  $expect = 28
  if ([IntPtr]::Size -eq 8) { $expect = 40 }
  Write-Output "input struct: $size bytes (expected $expect for this pointer size)"
  if ($size -ne $expect) { Write-Output "  !! layout mismatch, SendInput will deliver nothing"; $ok = $false }

  if ($ok) { Write-Output "verdict: usable" } else { Write-Output "verdict: NOT usable, see above" }
}

switch ($Cmd) {
  'windows' {
    $wins = Get-Windows
    $fg = [Cu]::GetForegroundWindow()
    foreach ($w in $wins) {
      $mark = ' '; if ([IntPtr]$w.Hwnd -eq $fg) { $mark = '*' }
      $t = $w.Title; if ($t.Length -gt 55) { $t = $t.Substring(0, 52) + '...' }
      "{0}{1,9} {2,7} @{3},{4} {5}x{6}  {7}" -f $mark, $w.Hwnd, $w.Pid, $w.X, $w.Y, $w.W, $w.H, $t
    }
  }
  'shot'  { Invoke-Shot }
  'tree'  { Invoke-Tree }
  'click' { Invoke-Click }
  'type'  { Invoke-Type }
  'key'   { Invoke-Key }
  'focus' { Invoke-Focus }
  'doctor' { Invoke-Doctor }
  default {
    @'
cu -- drive the Windows desktop

  cu doctor                   check that this skill can actually work here
  cu windows                  list top-level windows (* = foreground)
  cu shot [--window W] [--scale 0.5] [--out P]
                              capture screen or one window, print a path to view
  cu tree [--window W] [--depth 5] [--max 150]
                              UIA control tree with names and screen rectangles
  cu click X Y [--double] [--right] [--window W]
  cu type [--window W] <text> send text (Unicode-safe, Chinese ok)
  cu key [--window W] ctrl+s  send a key combo
  cu focus W                  bring a window forward (W = hwnd or title part)

Coordinates are physical screen pixels, the same space as the UIA rectangles.
Pass --window to every input command: it claims the foreground first and
refuses to send input if Windows will not hand it over.
'@
  }
}
