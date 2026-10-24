---
name: remote
description: 在另一台机器上跑命令、搬文件，并证明真的做到了。目标是一行 `add` 记住的 host/port/user/key——不读 ~/.ssh/config、不依赖任何本地环境或第三方 CLI。任何"这台机器之外的东西怎么操作/远程机器上是不是真的成功了/换台机器跑一下"的任务用它。
---

# 远程执行

`.agents/skills/remote/remote` 是一个纯标准库的 SSH 客户端外壳。它只做一件事：把
"在另一台机器上执行这段脚本"变成一个**可判定**的动作——输出是字节、退出码是数字、
成败有证据。

```bash
.agents/skills/remote/remote add box --host 10.0.0.5 --user ops --identity ~/.ssh/id_ed25519
.agents/skills/remote/remote doctor box     # 先证明这条链每一环
```

## 为什么不直接用 ssh

因为 `ssh host 'cmd'` 的失败是**安静的**，而且错的方向通常不是远端。这里每一条都是实测：

| 现象 | 原因 | 这里的做法 |
|---|---|---|
| 中文变乱码 | 远端控制台是 GBK，字节用 UTF-8 解 | 输出按 base64 分帧，从不是"文本" |
| `exit 7` 变成 `1` | **登录 shell** 把子进程的失败洗成 1 | 命令行带 `exit $LASTEXITCODE` |
| 卡住不返回 | ssh 继承了调用方的 stdin，那是个永不关闭的管道 | 一次性路径加 `-n` |
| 文件传了一半 | `cat \| ssh` 在大文件处截断 | 用 sftp，并**读回大小核对** |
| 引号被吃掉 | 本地 shell → ssh → 远端 shell 三层 | 脚本 base64 落盘再执行，不过任何解析器 |

一句话：**不要在命令行里传脚本，也不要从文本里读结果。**

## 两条路径，一层传输

```bash
remote run box 'hostname'          # 会话：复用一条 ssh，~390ms
remote run box 'hostname' --direct # 一次性：每次重连，~800ms
```

会话是**优化**，不是依赖。没起、死了、或者这个目标不适合（见下），同一条命令自动走
一次性，并且会说明用的是哪条（`REMOTE_TRACE=1` 可以看到）。

`doctor` 会把两条都跑一遍并对比，这是判断"会话值不值得"的唯一可靠方式：

```
  ok   one-shot round trip                805 ms
  ok   session round trip                 median 388 ms over 5
  ok   exit code fidelity                 exit 7 came back as 7
  ok   UTF-8 round trip                   exact
  ok   binary round trip                  b'\x00\x01\x1f\x7f\x80\xff'
```

## 两个独立的轴，别混

这是这套东西里最容易搞错的概念���也是我花了最久才理清的地方：

| 轴 | 问题 | 取值 | 谁决定 |
|---|---|---|---|
| **shell** | 用哪种语言写脚本 | `posix` / `powershell` | 你（或探测） |
| **login** | sshd 把你的命令交给谁 | `posix` / `powershell` / `cmd` | 探测 |

**login 决定退出码和引号，shell 决定脚本语法。** Windows 机器上 sshd 配的是 PowerShell，
但你要跑的是 Git Bash 脚本——这时 shell=posix、login=powershell、再加一个 runner：

```bash
remote add box --host 10.0.0.5 --user ops \
  --shell posix --runner 'C:\Program Files\Git\bin\bash.exe'
```

`--runner` 是解释器前缀。它把内层命令包成 `bash.exe -c "…"`，所以内层**必须**用
login shell 的引号规则写（这里刻意不含 `$`、反引号和 `?`）。

`login` 是探测出来的，因为没法问：让远端 `exit 7`，哪个组合能原样报回 7 就是它。
探测结果写回 target 文件，之后一次失败就是**远端变了**，不是又猜一次。

## 三个会静默说谎的地方

### 1. 子进程在 ssh 下会以为 stdin 已经结束

这是会话最初版本坏掉的唯一原因，而且它的症状全都指向 ssh——所以我先怀疑错了对象。

实测：开一条 ssh，**等 2 秒**再发第二条请求，就什么都收不到；立刻发就正常。换成登录
shell 自己跑同一个循环，2 秒、4 秒、5 秒延迟全部正常。

原因是**由子进程启动的 PowerShell** 看到一个空的 stdin，判定 EOF 直接退出。所以
PowerShell 登录时，会话里的循环必须跑在登录 shell 自己里（`Invoke-Expression`）。

**这是 PowerShell 特有的，不要推广到 `sh`。** 我一开始以为两者一样，也照改了 POSIX 分支，
但实测 `sh script` 在延迟 2 秒写入时读得好好的。那个改动无害，但理由是错的——而错的理由
会误导下一个人。所以 POSIX 分支仍然是子进程 `exec sh $D`，注释里写清了这是量过的。

推论，也是设计约束：**带 runner 的目标没有会话**，因为会话要说登录 shell 的家庭语言，
而 runner 说的是另一种。这种目标走一次性——慢一点，但不是假的。

### 2. 退出码不是免费的

远端自己的失败一律变成 1，除非命令行自己说清楚：

```
powershell -EncodedCommand 'exit 7'                 -> 1
powershell -EncodedCommand 'exit 7'; exit $LASTEXITCODE -> 7
bash.exe -c "exit 3"                                -> 1
bash.exe -c "exit 3"; exit $LASTEXITCODE            -> 3
```

所以"非零就是失败"是可信的，"哪个非零"不可信。这也是 `doctor` 有一条
`exit code fidelity` 的原因。

### 3. sftp 会报告成功而文件没到

我遇到过 `sftp` 打印 `put ...`、退出 0、目标文件不存在。所以传输之后**读回大小核对**，
不一致就报错。`--no-verify` 可以关掉，但关掉之后你就只剩 sftp 的说法了。

sftp 有自己的解析器（不是 shell 的）：双引号成组，**里面的反斜杠要写两遍**。Windows
路径不这样处理，文件会落到别处，而且不报错。

## 传文件

```bash
remote put box ./local.tar.gz /tmp/remote.tar.gz     # 默认核对大小
remote put box ./run.sh /tmp/run.sh --mode 755
remote get box /var/log/app.log ./app.log
```

**不要用 `cat file | ssh host 'cat > file'`**：这条路在 108KB 附近会截断（早期实测），
而且截断后退出码仍然是 0。sftp + 大小核对是唯一不靠猜的方式。

脚本太大怎么办？一次性路径受命令行长度限制（Windows 是 32767 字符，base64 后约 ×4/3）。
超了会明确报错并给出出路：先 `put` 上去，再 `run box 'sh /tmp/x.sh'`。

## 后台任务

```bash
remote run box 'make -j8' --detach
# started           pid 4821
# log               /tmp/remote-job-20261024-161500-a1b2.log
remote run box 'tail -n 20 /tmp/remote-job-….log'
```

`--detach` 用 `nohup`/`Start-Process` 起进程，立刻返回 pid 和日志路径。不会等它结束
——这正是"后台"的意思。

## 目标和独立性

```bash
remote add box --host 10.0.0.5 --user ops --identity ~/.ssh/id_ed25519 --port 2222
remote add box --host 10.0.0.5 --user ops --jump bastion   # ProxyJump
remote add box --host 10.0.0.5 --user ops --no-config       # -F /dev/null，完全封闭
remote ls
```

存在 `~/.remote/targets.json`，`remote rm box` 删掉。

**刻意不读的东西**：`~/.ssh/config`（别名是本地约定，别处不存在）、任何 roster、
任何第三方 CLI。`--no-config` 能让连接完全不受本地配置影响——排查"是不是本地 ssh
配置在捣鬼"时这一刀最干脆。

反过来，**默认也不会覆盖本地配置**：不加 `-F` 就仍然会读 `~/.ssh/config`，所以已有的
别名照样能用。这是有意的取舍，`--no-config` 是那个例外。

## 命令

| 命令 | 作用 |
|---|---|
| `remote add NAME --host H --user U` | 记住一个目标（`--port --identity --jump --shell --login --runner --no-config`） |
| `remote ls` / `rm NAME` | 列出 / 删除 |
| `remote doctor NAME` | 逐环证明：ssh、探测、退出码、UTF-8、二进制、传输、会话 |
| `remote run NAME 'script'` | 跑脚本；`--file` 读文件，`-` 或省略读 stdin |
| `remote run NAME 'script' --detach` | 后台跑，返回 pid 和日志路径 |
| `remote put` / `get` | 传文件，默认核对大小 |
| `remote session start\|stop\|status` | 管会话 |

`run` 还接受 `--cwd`、`--env K=V`（可重复）、`--timeout`、`--max-output`、`--direct`。

## 脚本怎么写给远端

脚本是在**远端**执行的，写法要按远端的 shell：

```bash
# POSIX
remote run box 'df -h / | tail -1'
# PowerShell
remote run box 'Get-ChildItem C:\ | Measure-Object | Select-Object -Expand Count'
# 多行用 --file 或 stdin，不要试图在引号里塞换行
remote put box ./probe.sh /tmp/probe.sh && remote run box 'sh /tmp/probe.sh'
```

`--file` 和 stdin 是**推荐**方式：脚本逐字节送达，不经过任何一层引号。

## 限制

- 只支持 SSH。没有 WinRM、没有云厂商 API、没有串口。
- 会话串行：一条会话一个请求。并发要用多进程，或者多个 session（`--direct`）。
- 一次性受命令行长度限制；会话没有（有一个 4MB 的输出上限，可调）。
- `--timeout` 超时后远端**可能还在跑**。这种情况会明说 `outcome unknown`，并且**绝不重放**
  ——重放可能让命令执行两次。
- 传文件走 sftp 的系统实现，所以 `sftp` 必须在 PATH 上（`doctor` 会检查）。
- 会话的东西全在 `~/.remote/`（targets、sessions、logs），删掉就是全新状态。

## 自己验证它

```bash
python3 test_remote.py      # 58 项
```

两层：纯函数（默认值、ssh 参数、引号、包装），以及**对着一个假登录 shell 跑真会话**
（`fake_login_shell.py`，把命令在本地 bash 里跑）。后者能测到真机测不到的东西——比如
POSIX 登录路径，以及"请求延迟到达"这个让第一版彻底失效的场景。

测试是有齿的，七项变异都验证过会红：去掉按 id 分帧、sftp 不转义反斜杠、一次性路径继承
stdin、去掉登录 shell 的退出码尾缀、输出截断超过上限、`execute` 不遵守会话闸门、
以及把 PowerShell 循环从登录 shell 交回子进程（对应那个真 bug）。

两项是**先发现测试没齿才补的**，值得记下来：去掉按 id 分帧时什么都没红，因为我的超时测试
会杀掉会话，反而绕开了「迟到的回复」这个场景——补了一个直接把外部 id 的帧塞进队列的用例才
抓到。`execute` 的闸门也一样，之前只测了 `session_possible()` 的返回值，没测它真的被遵守。

另外一项（把 `. $D` 改回 `sh $D`）**永远红不了**，因为它是等价变异——原因见上面「子进程
的 stdin」那一节。测试没红不一定是测试不行，也可能是那个改动本来就没作用。

## 一句话总结

远端执行的难点不在连上去，而在于**判断它到底成功了没有**。所以这里的一切都围绕证据：
字节而不是文本，数字而不是"没有报错"，读回大小而不是"传完了"。
