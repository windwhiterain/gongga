# keep_alive 是一个瞬间，不是一个状态

2026-11-15。症状：`evolve.sh swap` 之后，session `complete` 的 channel「全死了」，
而 SPEC 说 `cancel(keep_alive)` 会让它们活过重启并被 adopt。

## 先说结论：flag 本身没有坏

隔离 home（`.dev/keepalive-verify`，内核分配端口，key 显式传）实测：

| 步骤 | program | relay | dir | row | 下一个 start |
| --- | --- | --- | --- | --- | --- |
| `cancel {keep_alive:true}` | ALIVE | ALIVE | there | 在 | `adopted_channels=2`，`input_channel` 答 `MARKAFTER` |

连做三轮（第一轮由本 server 起的 relay，后两轮是 **adopt** 来的 relay）都活：
`release_all` 走的是 `tools::job::ARMED` 注册表，而 `arm_adopted` 调的也是
`job::arm`（`detached.rs:1133`），所以两类都进得了注册表、都会被放走。11-14 那份
note 里「adopted 的 relay 不在注册表里」的猜测是错的。

## 真正的洞：`keep_alive` 只活到下一次 stop

`stop_every`（`channel.rs:878`）对**每一行**都写 `kill`，然后 `notice_end(end_row=true)`
把行删掉。它不看这个 channel 是谁要求留下的：

```rust
pub async fn stop_every(&self) -> usize {
    let rows = ...every_channel()...;
    for row in rows {
        place.append("in", "kill\n")  // 无条件
        self.notice_end(..., end_row = true)   // 行被 claim 掉
    }
}
```

实测（`moment.sh`，同一个 channel）：

| 步骤 | 结果 |
| --- | --- |
| `cancel {keep_alive:true}` | program ALIVE / relay ALIVE / dir there / row 在 |
| 下一个 start | **adopted，而且答话** |
| `cancel {}`（窗口离开发的那一个） | `1 channel(s) ended with this server` |
| 再下一个 start | row 没了，`input_channel` �� `no channel` |

而窗口离开时发的正是**不带** `keep_alive` 的 `cancel`
（`client/src/lib.rs` 的 `Client::cancel`，`docs/notes/client-lib.md` §"Client::cancel --
SPEC's default"：11-12 用户判过「关 TUI 就该关掉它拥有的东西」）。

所以 `evolve` 的换手路径上就有这个洞：`swap` 用 `keep_alive` 让 channel 活下来，
`swap` 之后只要还有**任何一个** start 的 stop 走了默认路径（窗口关、第二次 evolve、
`stop` 信号），上一轮 `keep_alive` 保住的那批就整批没了。

## 顺带量到的第二件事：relay 成了孤儿

live home（`~/.gongga`）现在挂着 4 个 `gongga-relay.exe`，**行没了、目录还在、程序没了**：

```
56524|... --relay C:\Users\17124\.gongga\channels\ch-4acfa43f7b1042d8bef5b21d42315f8b --protocol 1 --build e0d717cd7769
```

`ch-4acfa43f` 的 `status` 是 `{"pid":56108,...,"exit":null}`，而 56108 已经不在了。
`stop_every` 删了行，但删不掉这个「行已经没人认领」的 relay —— 它按 50ms 轮询
那个没人写的 `in`，永远等着。`ls` 不会列它，`stop_every` 也够不着它（没有行了），
`sweep` 删目录但不动进程。**这类进程只能靠 pid 收**，而 pid 只在 `status` 里、
`status` 又只在这个目录里。

## 修法（未做，等指令）

`keep_alive` 应该是**行上的一个状态**，不是 `cancel` 一个瞬间的参数：

- `channels` 表加一列（如 `kept_at`），`cancel{keep_alive}` 写它；
- `stop_every` 只对**没有** `kept_at` 的行写 `kill`；`cancel{keep_alive:true}` 之后的
  默认 stop 不再收走它们；
- `stop_every` 删行之前先收 relay（`disarm_detached` / `kill_tree`），否则就留下孤儿。

这是 SPEC 层的语义决定（`cancel` 的 `keep_alive` 现在是一次性的），按规矩要先过
justice/advisor，不该我直接改。

## 验证脚本

`.dev/keepalive-verify/`（跑完自删）：`moment.sh` 是两个 cancel 的对比，
`adopt.sh` 是「adopted 的 relay 也活」，`kill.sh` 是不走 cancel 的 kill。
三条都是隔离 home + 内核分配端口 + 显式 key，不碰 `~/.gongga`。