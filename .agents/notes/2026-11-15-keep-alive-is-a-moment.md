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

## 修法：已做（`0c7553de`）

`keep_alive` 现在是**行上的一个状态**，不是 `cancel` 一个瞬间的参数：

| 列 | 作用 |
| --- | --- |
| `detached` | `cancel(keep_alive)` 写 1；`stop_every` 跳过带它的行；继任者 adopt 后清 0 |
| `relay_pid` | relay 自己的 pid（不是 `pid`，那个是**程序**的） |
| `relay_started_at` | 那个 relay 的启动时间，adopt 前先核对，防 pid 被回收 |

后两列是必须的：行上的 `pid` 是程序的，只拿它无法找到 relay——程序没了能看出来，
relay 却无法 adopt。没有它们，`keep_alive` 就只是「让这一行留着」。

实测（`.dev/keepalive-check/check2.sh`，隔离 home）：

| 步骤 | program | relay | 结果 |
| --- | --- | --- | --- |
| `cancel{keep_alive}` × 3 轮，每轮后重启 | ALIVE | ALIVE | 每轮都答话 |
| adopt 之后的默认 `cancel` | gone | — | `1 channel(s) ended with this server` |

最后一行**不是** bug：adopt 之后 flag 已清，这个 channel 就是本 server 的了，
默认 `cancel` 收走它——这是 SPEC 的默认，也是 11-12 的判决（关窗口就关掉它拥有的）。
flag 只管**换手那一段**。

## 仍未查完的

- **孤儿 relay**：`stop_every` 删行之前不收 relay，于是留下一批「行没了、目录在、
  程序没了」的 relay（live home 现在有 4 个）。本次改动让带 flag 的行不再被误删，
  但**不带 flag**的行走到 `stop_every` 时仍会留下孤儿。这是独立的一处。
- **remote channel**：本 side 没有 relay 的句柄，所以 `adopt_kept` 对它是 no-op。
  你说的「本地 relay 持远端进程」是另一条路，单独谈。

## 验证脚本

`.dev/keepalive-check/`（跑完自删）：`check2.sh` 是三轮换手 + 一次默认 cancel，
隔离 home + 内核分配端口 + 显式 key，不碰 `~/.gongga`。
（早期探过 `.dev/keepalive-verify/`，已删。）