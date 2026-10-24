# Points this machine's DSH at the Git Bash binary explicitly.
#
# Why not rely on PATH: enabling the WSL feature puts a legacy `bash.exe` launcher in
# C:\Windows\System32, which is ahead of Git on PATH. An older DSH resolves the shell
# with a bare `which bash`, so it silently switches to a shell that has no distribution
# behind it and every command fails. Naming the path removes the race instead of
# depending on who wins it.
#
# The config key is `program` under [bash] (crates/server/src/config.rs).
param([switch]$WhatIf)

$ErrorActionPreference = 'Stop'

$candidates = @(
  'C:\Program Files\Git\bin\bash.exe',
  'C:\Program Files\Git\usr\bin\bash.exe',
  'C:\SoftWare\git\Git\bin\bash.exe',
  'C:\software\Git\Git\bin\bash.exe',
  'C:\software\Git\Git\cmd\bash.exe',
  (Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe')
)
$gitBash = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $gitBash) {
  throw "no Git Bash found; install Git for Windows, or edit the candidate list in this script"
}
if ($gitBash -match '\\Windows\\system32\\') {
  throw "refusing to pin the WSL launcher: $gitBash"
}

$configDir = Join-Path $HOME '.gongga'
$configPath = Join-Path $configDir 'config.toml'
if (-not (Test-Path $configDir)) { New-Item -ItemType Directory -Path $configDir | Out-Null }

$escaped = $gitBash -replace '\\', '\\'   # TOML basic strings escape backslashes
$block = "[bash]`nprogram = `"$escaped`""
$current = if (Test-Path $configPath) { Get-Content -LiteralPath $configPath -Raw } else { '' }

if ($current -match '(?m)^\[bash\]') {
  # Replace just the [bash] section, leaving other settings untouched.
  $pattern = '(?ms)^\[bash\].*?(?=^\[|\z)'
  $updated = [regex]::Replace($current, $pattern, "$block`n")
} else {
  $updated = $current.TrimEnd() + "`n`n" + $block + "`n"
}

if ($WhatIf) {
  "would pin: $gitBash"
  "config:    $configPath"
  $updated
  exit 0
}

Set-Content -LiteralPath $configPath -Value $updated -Encoding UTF8
"pinned bash = $gitBash"
"config       = $configPath"
