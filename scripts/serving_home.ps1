param([string]$Posix, [string]$Forward, [string]$Windows)
# Every program this home is served by, its watcher first: a watcher restarts the
# server under it, so it goes before the server does. The caller passes all three
# spellings of one directory -- bash's `/c/...`, `C:/...`, and `C:\...` -- because the
# watcher is started with one and the server logged with another.
function Names-Home($item) {
  if (-not $item.CommandLine) { return $false }
  foreach ($form in @($Posix, $Forward, $Windows)) {
    if ($form -and $item.CommandLine.Contains($form)) { return $true }
  }
  return $false
}
$all = Get-CimInstance Win32_Process
$envs = $all | Where-Object { $_.Name -eq 'env.exe' -and (Names-Home $_) }
$servers = $all | Where-Object { $_.Name -eq 'gongga-server.exe' -and (Names-Home $_) }
$envs | ForEach-Object { $_.ProcessId }
$servers | ForEach-Object { $_.ProcessId }
