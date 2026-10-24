---
name: evolve
description: 替换掉正在跑自己的那个服务端——改完 server 代码之后，让新二进制接管 home 并继续当前回合。用 .agents/skills/evolve/evolve.sh（stage / launch / swap / status / clean）。只在改过服务端代码后用它；改 skill 脚本、文档、TUI 不需要。换手会 cancel 本回合，靠 resume 接回来。
---

# 换掉自己

改完服务端代码，跑着的进程还是旧的二进制。这个 skill 把它换成新的，而且让**当前回合不死**。

```bash
S=.agents/skills/evolve/evolve.sh

$S stage              # 跑测试 + 构建到 .dev/target + 暂存 + 生成 launcher
# 用 bash --detached 启动它打印的 launcher
$S swap <stamp>       # 确认继任者真在等，再发 cancel
$S status             # 当前 stamp、谁在等、端口与锁
$S clean              # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
```

## 为什么顺序不能反

`swap` 会 cancel 本回合。只有**继任者已经在等**，那个回合才有东西接：

```
stage → 起继任者 → swap(cancel) → 旧进程退出 → 继任者绑端口 → resume 本回合
```

反过来（先 cancel 再起），一旦新二进制起不来，home 就空着，没人 resume——回合真的丢了。
`swap` 的守卫就是为此存在（stamp 必须是 current + launcher pid 活着 + 心跳 10s 内 +
老服务端还在）。**不要绕过它手动 `cancel`。**

## 换完怎么证明成功

不要相信自己写的笔记，也不要相信 `server.lock` 变了就算数。看**继任者自己的日志**：

```bash
sed 's/\x1b\[[0-9;]*m//g' ~/.gongga/detached/<最新>.log | grep -E "replaying a turn|resumed=true"
```

要看到这两行（时间戳应几乎相同）：

```
... commands: replaying a turn left pending session=<名> <你上一句原话>
... agent: turn start session=<名> resumed=true
```

**没有 `replaying a turn` = 没接上**，哪怕 `server.lock` 已经是新 pid、端口也绑着。

## 不要写交接笔记

resume 是把**完整 transcript** 原样重放（消息、reasoning、每个工具调用和结果），外加
`summary`（如果折叠过）。"我打算干什么"已经在里面了，写一份笔记不会多提供任何信息。

而且它在两种真故障下毫无用处：

| 故障 | 笔记有用吗 |
|---|---|
| pending 行没留下 | **没用**——根本没人被唤醒，没人会读 |
| 上下文折叠过 | 没用——该用 `compact` skill 的 `read --after 0` 读回来 |

resume 之后要做的是**重新测量外部状态**，因为那部分 transcript 里没有、而且已经变了：

```bash
cat ~/.gongga/server.lock                    # 现在是哪个 pid
netstat -ano | grep :9700                    # 端口绑着吗
python -c "import sqlite3,os;c=sqlite3.connect(f'file:{os.path.expanduser(\"~/.gongga/gongga.db\")}?mode=ro',uri=True);print(c.execute('select id,kind from pending_tasks').fetchall())"
```

## 实测踩到的坑

**1. Windows 上 `exec` 不保留 pid。** launcher 是 bash，`exec` 一个原生 `.exe` 只是
spawn 一个子进程并留下 bash 当父进程。所以 `server.lock` 的 pid ≠ `launch.pid` 里记的
pid，这是**正常**的，不是换手失败。`status` 用父进程关系识别这种情况。

副作用：`del_bash` 打到一个"是服务器父进程"的 detached 行时，会拒绝执行（`adopt()` 的
祖先守卫）——否则删一个 detached bash 就能把自己杀掉。

**2. `.dev/target`，不是 `target/`。** Windows 锁住运行中的 `.exe`，而被替换的往往就是
`target\debug\gongga-server.exe`。私有 target 目录让构建不受 target/ 在干什么影响。

**3. `stage` 默认只构建 server。** 协议没变时旧客户端照样能连；`--all` 会连客户端一起换，
而用户正在用的 TUI 锁着 `gongga.exe`（会 `拒绝访问 (os error 5)`）。

**4. 只有最新的 stage 能接管。** `stage` 写 `.dev/evolve-current`；被取代的 launcher 自己
退出等待循环，`swap` 拒绝非当前 stamp。重复 `stage` 安全，但**别指望用旧 stamp 回滚**
（它的继任者已经走了，要回滚就重新 `stage`）。

**5. 继任者会留在进程表里。** 每次 swap 多一个 `bash.exe`（服务器的父进程），随服务器
退出而死。不是泄漏。

## 什么时候不需要它

- 只改了 skill 脚本 / 文档 / TUI → 不用换服务端（TUI 要重开客户端而已）。
- 想换的是**客户端** → `stage --all`，然后手动重开 TUI。

## 代价

`swap` 之后本回合被 cancel 并保留，继任者重放它。残余风险见 `DESIGN.md` §10。
