# Detached launcher for the live (flm-rendered) mihomo core.
#
# Why this exists: the core that carries this machine's traffic is the one flm renders
# into ~/.kualemon/mihomo/config.yaml. When it is started from an agent session's tool
# call, the session's turn end kills it -- 2026-11-23 11:35:23 the core's log stops
# mid-line with no fatal, no error and no shutdown line, and the router at 127.0.0.1:7899
# was left with a dead upstream (every connection answered 502). The scheduled task
# `Mihomo core` is the detached path, but it runs ~/.proxy/core/config.yaml, which is a
# Nov-8 render: 21 whitelist hosts, missing the 2026-11-20 googleapis.com / gstatic.com /
# googleusercontent.com additions.
#
# So this script is started *through WMI* (Invoke-CimMethod Win32_Process Create), which
# makes the core a child of Wmiprvse.exe rather than of any session. Nothing here writes
# the home or cancels anything.
$core = Join-Path $HOME '.kualemon\mihomo'
$started = Start-Process -FilePath (Join-Path $core 'mihomo.exe') `
    -ArgumentList @('-f', (Join-Path $core 'config.yaml'), '-d', $core) `
    -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $HOME '.kualemon\logs\mihomo.out.log') `
    -RedirectStandardError (Join-Path $HOME '.kualemon\logs\mihomo.err.log') `
    -PassThru
$started.Id
