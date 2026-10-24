# Verifies this machine can host both a Windows and a Linux toolchain at once.
#
# The failure this exists for: enabling the WSL feature drops a legacy `bash.exe`
# launcher into C:\Windows\System32, which is ahead of Git on PATH. Every bare `bash`
# then lands in WSL, and a Windows-only toolchain (this harness included) stops
# working with no error that names the cause. `-Fix` renames that launcher back out of
# the way, which is safe because `wsl` is the documented entry point and remains.
param([switch]$Fix, [switch]$Quiet)

$ErrorActionPreference = 'Stop'
$problems = 0
$shim = 'C:\Windows\System32\bash.exe'

function Report([bool]$ok, [string]$what, [string]$detail) {
  if (-not $ok) { $script:problems++ }
  if (-not $Quiet -or -not $ok) {
    '{0} {1,-30} {2}' -f $(if ($ok) { 'ok  ' } else { 'FAIL' }), $what, $detail
  }
}

# 1. The Windows side owns the name `bash`.
$bash = @(Get-Command bash -All -ErrorAction SilentlyContinue)
$first = $bash | Select-Object -First 1
$gitBash = $first -and $first.Source -notmatch '\\Windows\\system32\\'
Report ([bool]$gitBash) 'bash is not the WSL launcher' $(if ($first) { $first.Source } else { 'no bash on PATH' })

if (-not $gitBash -and (Test-Path $shim)) {
  if ($Fix) {
    try {
      Rename-Item -LiteralPath $shim -NewName 'bash.exe.wsl-shim-disabled' -ErrorAction Stop
      $again = @(Get-Command bash -All -ErrorAction SilentlyContinue) | Select-Object -First 1
      Report ([bool]$again -and $again.Source -notmatch '\\Windows\\system32\\') 'shim disabled' $again.Source
    } catch {
      Report $false 'shim disabled' "could not rename $shim : $_"
    }
  } else {
    Report $false 'shim disableable' 'run with -Fix to rename the WSL launcher'
  }
}

# 2. The Linux side is reachable under its own name, never by shadowing `bash`.
$wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
Report ([bool]$wsl) 'wsl.exe present' $(if ($wsl) { $wsl.Source } else { 'not installed' })

if ($wsl) {
  # Do not parse wsl.exe's output as text: it is UTF-16, and when no distribution is
  # installed it prints usage instead of an empty list, so splitting it yields "distro
  # names" that are really help lines. A registered distribution is a single bare token;
  # that shape is what separates the two.
  $raw = (& wsl.exe --list --quiet 2>&1 | Out-String) -replace "`0", ''
  $names = @($raw -split "`r?`n" | ForEach-Object { $_.Trim() } |
             Where-Object { $_ -match '^[A-Za-z][A-Za-z0-9._-]*$' })
  Report ($names.Count -gt 0) 'a distro is installed' $(if ($names.Count) { $names -join ', ' } else { 'none; run: wsl --install -d <name>' })
}

# 3. The two must not share a name anywhere else.
foreach ($name in 'sh', 'env') {
  $system = "C:\Windows\System32\$name.exe"
  Report (-not (Test-Path $system)) "$name.exe has no Windows shim" $(if (Test-Path $system) { $system } else { 'only Git provides it' })
}

# 4. A Windows toolchain must still work when bash is asked for by name.
if ($gitBash) {
  $probe = & bash -c 'echo platform-ok' 2>&1
  Report ($probe -match 'platform-ok') 'bash runs a Windows toolchain' ($probe -join ' ')
}

if (-not $Quiet) {
  ''
  if ($problems -eq 0) {
    'verdict: both platforms available'
    '         windows  bash  -> ' + $first.Source
    if ($wsl) { '         linux    wsl   -> ' + $wsl.Source }
  } else {
    "verdict: $problems problem(s); run with -Fix for the ones that can be repaired"
  }
}
exit $(if ($problems -eq 0) { 0 } else { 1 })
