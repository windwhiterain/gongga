# 双平台共存：Windows 与 Linux on one machine

目标：同一台机器上，Windows 工具链和 Linux 工具链**各自可用且互不干扰**，并且
"哪个平台"是明确的，不依赖 PATH 的偶然顺序。

## 唯一的硬冲突：`bash` 这个名字

Git for Windows 和 WSL 都想提供 `bash`。冲突的具体形态（实测于 2026-10-24）：

```
Get-Command bash -All
  C:\Windows\system32\bash.exe        ← 启用 WSL 功能后出现的启动器
  C:\software\Git\Git\cmd\bash.exe    ← 真正的 Git Bash
```

System32 在 PATH 里排在 Git 之前，所以 `bash` 落到 WSL 启动器上。如果没装发行版，
它什么也做不了；而**任何**通过 `bash` 跑命令的工具（包括 DSH 自己的 shell 工具）都会
静默失效——没有报错指向真正的原因。

## 规则：两个平台各有专属名字

| 平台 | 入口 | 谁的 |
|---|---|---|
| Windows | `bash` | Git Bash，独占这个名字 |
| Linux | `wsl` / `lx` | WSL，绝不占用 `bash` |

`lx`（`lx.cmd`）是 Linux 的常用入口：把当前 Windows 目录翻译成 `/mnt/<drive>/...`，
然后在那里进入 Linux shell。

```bash
lx                 # 在当前目录进入 Linux bash
lx make test       # 在 Linux 里跑一条命令
wsl -d Ubuntu      # 直接用 wsl，也在 PATH 里
```

Windows 侧什么都不用改：Git Bash 仍然是 `bash`。

## 三层保障

只靠一层不够，因为每层都会在某次系统更新后失效。

### 1. 不装 shim（最强）

`dualenv-doctor.ps1 -Fix` 会把 `C:\Windows\System32\bash.exe` 改名成
`bash.exe.wsl-shim-disabled`。

**代价**：`wsl` 命令本身不受影响（`wsl.exe` 是另一个文件），只是少了那个遗留启动器。
`wsl bash` 仍然可用。

### 2. 钉死 DSH 的 shell 路径（对旧版本必需）

DSH 的 `[bash] program` 可以显式指定 shell。旧版 DSH 用 `which::which("bash")` 解析，
会随 PATH 变化；写进配置就与 PATH 无关了。

```powershell
powershell -File scripts/dualenv/dsh-pin-shell.ps1 -WhatIf   # 先看会写什么
powershell -File scripts/dualenv/dsh-pin-shell.ps1
```

写入 `~/.gongga/config.toml`：

```toml
[bash]
program = "C:\\SoftWare\\git\\Git\\bin\\bash.exe"
```

新版 DSH 的兜底链本来就先试 Git 的固定安装位置，最后才 `which bash`，所以即使不钉也
不会错——但显式配置不依赖版本。

### 3. 自检（每次系统更新后跑）

```powershell
powershell -File scripts/dualenv/dualenv-doctor.ps1
```

检查：`bash` 没落到 System32、`wsl.exe` 在、有发行版、`sh`/`env` 没有 Windows shim、
并且真的能通过 `bash` 跑一段 Windows 命令。

## 已知的其他代价

启用 WSL 会让 Windows 启动时加载 hypervisor（`HypervisorPresent=True`，
`hypervisorlaunchtype=Auto`）。这是 VirtualMachinePlatform 的固有副作用，会影响嵌套
虚拟化、部分反作弊和模拟器。要撤掉必须再重启一次。

## 回退

```powershell
# 只关掉 shim 之外的残留
Disable-WindowsOptionalFeature -Online -FeatureName VirtualMachinePlatform -NoRestart
Disable-WindowsOptionalFeature -Online -FeatureName Microsoft-Windows-Subsystem-Linux -NoRestart
# 重启后 hypervisor 也不再加载
```

已装的发行版要另外卸载：`wsl --unregister Ubuntu-24.04`。
