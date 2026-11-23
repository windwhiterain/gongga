# 一次「按命令行匹配」的杀进程，把整台机器杀了一遍

2026-11-23 11:30–12:00，我在 `.dev/verify/field-move.sh` 的收尾里写了一个 killer，让它
「杀掉命令行里出现这个测试 home 的所有进程」，用来处理测试服务端被 watcher 重启的问题。
结果是每一次运行都杀掉了这台机器上**除 bash.exe 与 powershell.exe 以外的所有进程**：
explorer、sihost、SearchHost、svchost……用户看到的是桌面/任务栏反复消失重建，像被注销。
一共约五次。

## 机制

```powershell
param([string[]]$Homes)
Get-CimInstance Win32_Process | Where-Object {
  $_.CommandLine -and $_.CommandLine.Contains($homes[0]) -or
  $_.CommandLine -and $_.CommandLine.Contains($homes[1]) -or
  $_.CommandLine -and $_.CommandLine.Contains($homes[2])
} | Where-Object { $_.Name -ne 'powershell.exe' -and $_.Name -ne 'bash.exe' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
```

- 调用方写 `-Homes a b c`，但 `powershell -File` 只把**第一个**值绑到 `[string[]]` 参数上：
  实测 `count=1`，`$Homes[1]` 与 `$Homes[2]` 是 `$null`。
- PowerShell 把 `$null` 传给 `String.Contains([string])` 时转换成**空串**，而
  `'abc'.Contains('')` 是 `True`。于��第二个分支对**每个有命令行的进程**都成立。
- 排除名单只有 bash/powershell，所以其余全部被杀。

读一遍即可复现（只数、不杀）：

```
count=1
[0] = <C:\resource\gongga\.dev\field-move\home>
Homes[1] is null: True
'abc'.Contains(Homes[1]) = True
processes the filter matches: 49
  52304 sihost.exe
  70388 explorer.exe
  96552 StartMenuExperienceHost.exe
  97492 svchost.exe
  ...
```

## 教训

1. **判「是不是我的」只能用 pid**，来源只能是该 home 自己的 `server.lock`（以及它的
   ParentProcessId）。按镜像名、按命令行子串、按「其余全部」都是同一个错误的三种写法。
2. **空模式匹配一切。** 任何 `Contains($x)` / `-like "*$x*"` / 正则，只要 `$x` 可能是
   `$null`/`''`，它就是「全选」。先断言非空，再匹配。
3. **`-File` 的数组参数只绑第一个值**，剩下的静默变 `$null`；多值要用逗号，或者干脆不用数组。
4. 杀进程的脚本要先能「只打印不杀」跑一遍，看到数字再决定。

## 收尾

- `field-move.sh` 的收尾改成：只杀 `server.lock` 里的那一个 pid；`rm` 不掉就打印
  「left for a human」，不再追。
- 早先两份 verify 记录里「杀掉命令行里出现该 home 的所有进程，只留 bash/powershell」的
  建议已改成上面这条。
