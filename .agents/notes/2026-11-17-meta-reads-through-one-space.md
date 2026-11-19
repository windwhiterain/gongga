# Meta 引用统一到 Space：一条路径，一条保持的连接

用户指令（2026-11-17）：Meta reference 还没有支持 remote，要用统一路径支持 remote；
并且——**保持用保持连接**。也就是不要为统一付出「每个 `@file` dial 一次」的代价。

## 结果

`meta::Source` 删掉。meta 不再认识 `Reader`，也不再认识「Local / Remote / Unreachable」三态：

```rust
meta::rules(&Space, home)
meta::roots(&Space, home, texts)
meta::expand(store, &Space, home, text, llm)
meta::embed(&Space, home, text)
```

`Space` 现在就是那条唯一路径，它同时是「哪台机器」和「怎么在那儿读/跑」：

| 方法 | 本机 | 对端 |
| --- | --- | --- |
| `spell(cwd, name)` | `image::resolve` | `\`→`/`，驱动盘绝对，相对名挂在 cwd 后 |
| `read_bytes(path)` | `std::fs::read` | 保持连接上的 `Read`，游标前进，一次 redial |
| `list(path)` | `std::fs::read_dir` | 同一连接上的 `List` |
| `stat(path)` | `std::fs::metadata` | 同一连接上的 `Stat` |
| `free_space(path)` | `disk::free_space` | 同一连接上的 `Space` |
| `run(cwd, prog, args, in)` | `tokio::process` | 同一连接上的 `Open` |
| `machine()` | `Machine::Local` | `Machine::Remote` |

**保持连接**落在 `Space` 自己身上：`link: Arc<Mutex<Option<Runner>>>`，`Clone` 共享它。
所以 `SessionHandle` 持有一个 `Space`（`inner.space`），`ToolContext` 也克隆同一份，
`SessionManager.spaces` 按 remote 名缓存一份——一个 remote 一条 runner。

## 为什么这样统一才对

旧形状是两条路：`Space::run` 每次 `pick::runner` 重新 dial，`remote::Reader` 自己保持连接。
于是 meta 走 Reader、file 工具走 Space，一个有 peer 的 session 同时握着两个 runner，
而且每跑一个程序都要 dial 一次。`docs/notes/server-space.md` 里 "What is not yet here"
那段早就点名了这个分叉；现在那段改成了陈述。

`Unreachable` 不是丢了语义：它就是「远端 Space 但 dial 不上」，`read_bytes` 返回 `None`，
引用原样留在文本里——和以前 `Source::Unreachable` 的行为逐字一致。

## 难点：别在 `Space::of` 里新建连接

`Space::of(endpoint)` 每次给一条新 link。所以凡是「本该共用」的地方都改成传那份持有的
`Space`，而不是照着 endpoint 现造：`SessionHandle::space`、`ToolContext::space_on`、
`SessionManager::space_for`（按 remote 名缓存）。`ToolContext::space_on` 的另一台机器返回
`Space::local()`（对端不在这台机器上，没有连接可言）。

## 验证

`bash scripts/space_probe.sh` → **PASS · 18s**，真 server + 假模型 + 操作者自己的
`lichen-vm`：

- 对端 `exec` 造出 `C:\Users\Admin\space-probe-<pid>`；
- 一条消息写 `@file(a.md) @file(b.md) @file(./a.md)`，两个对端文件**都**进了模型请求
  （`alpha True`、`beta True`）——旧的「本机读」路径不可能读到这两个文件；
- 服务端日志里「placing this build's pair instead」整轮只出现 **1** 次：整条消息一次 dial。

`scripts/remote_selftest.sh` 仍是 4 红，但与本次改动无关：用改动前的 HEAD（`0592f0af`）
跑同一份脚本，红的是同样 4 条。成因在脚本自身——它给 `exec` 传 `name`/`task`（工具早就不
收这两个参数），于是对端 scratch 从来没建出来，后面四条级联失败。本分支顺手修掉了两处
让脚本**根本没跑**的毛病：`accept-new \n` 那个字面反斜杠 n（探针实际执行的是 `ssh ... n -i ...`，
永远失败，整轮直接 skip），以及两处过期的 `name`/`task`。剩下 4 条属于脚本自己的解析，
没有动。

## 没做的

- 没改 `bash`/`exec`/channel：它们是有 workspace 的长会话，不是一次性 run。
- `@skill(...)` 的项目层对 remote session 仍然故意不取（skill 在本机跑），保持原语义。
- 没有为 `Space` 加写文件：`write_bytes` 仍只有 Python 那份 `gongga_space.py`。