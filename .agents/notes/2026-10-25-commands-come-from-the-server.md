# 命令表由 server 独占：cli/tui 不再有清单，python 脚本可注册命令

## 病灶

改之前命令有**两份清单**：

- `crates/server/src/commands.rs` 的 `catalog()`：只有 6 条服务端命令；
- `crates/cli/src/catalog.rs` 的 `client_specs()`：5 条客户端命令硬编码在 CLI 里。

于是"这条命令存在吗"有两个答案。客户端那半条要加命令就必须重编 CLI；服务端发来的
catalog 里如果出现客户端不认识的客户端命令，CLI 只能报 "unknown"。

## 改法

1. **`CommandSpec` 多一个 `target`**（`protocol`）：`server` / `server_script` / `client`。
   缺省 `server`，所以旧形状解码后语义不变。前端一律按 target 分派，不再按名字查表。
2. **catalog 只有一个出口**：`Dispatch::catalog()` = 服务端命令 + 客户端命令 + 脚本命令，
   在每次 greeting 时构建。net.rs 直接把它的结果放进 `Hello`。
3. **客户端命令由 server 发布**（`protocol::client_commands()`）。CLI 只保留"怎么做"，
   不保留"有哪些"。补全里那两处依赖客户端状态的（sticky 名、filter 名）新增
   `CompletionSource::StickyArguments` / `EventFilters`：server 说"来自客户端状态"，
   客户端自己填。
4. **`server_script`**：新模块 `crates/server/src/script.rs`。扫描 `<home>/scripts/*.py`，
   每个脚本用 stdin 一条 JSON（`{"op":"manifest"}`）自报命令，命中调用时再起一次子进程
   （`{"op":"run", ...}`），回包 `ok/message/data`。**加命令不改 Rust、不重启**：目录指纹
   变了就在下一次连接时重读。
5. **`[scripts]` 配置**：`dir`（默认 `<home>/scripts`）与 `program`；不给 program 时探测
   一个真的会答 `-V` 的解释器（Windows 的 `python3` 商店桩会失败，不能只看 `which`）。
6. **收尾**：`cancel` 之后 `stop_all()` 杀掉还在跑的脚本，调用方收到的说法是
   "the server stopped while this was running"，而不是被杀进程的退出码。

## 顺带修好的两个真 bug（都是脚本把它顶出来的）

- **命令 id 跨客户端撞车**：`cmd_result` 是广播的，而当每个 CLI 都从 1 开始编号时，
  一个脚本里再调 CLI，内层的 `id:1` 会直接结束外层的等待。现在 id 用
  时间+pid 播种，跨客户端唯一（`seed_command_id`，有回归测试）。
- **相对 `--home` 泄漏给子进程**：`--home .dev/x` 时子进程（脚本、shell）在别的目录解析同一个
  相对路径。现在 config 载入时把 home 变成绝对路径（用 `std::path::absolute`，不是
  `canonicalize`——后者在 Windows 会带 `\\?\` 前缀，塞进 `GONGGA_HOME` 会毒到 shell）。

## 证据（都是实测，不是设想）

- `cargo test --workspace`：20+70+15+5+160+63 全过。新增：脚本注册/调用/坏脚本/热加载/未注册
  4 组 server 测试，`target` 与两个新补全源的 protocol 测试，两个 id 回归测试。
- `comments-check` 0 violation、`test-layout-check` 0 violation、`fmt` 干净；
  clippy 剩的 warning 全在没碰过的 `llm.rs`/`session.rs`/`agent.rs`。
- 真服务端 + 真 python（临时 home、端口 19701，已 kill 并清理）：
  - 加脚本 → **不重启**，下一次连接就能 `gongga script_echo --message hi`，回包带 data；
  - `script_echo` 少参数 / 多参数 → 由脚本声明的 spec 判，报错与内建命令同款；
  - 坏脚本只产生一条 `problems()`，不影响其它命令；
  - 一个 sleep 300s 的脚本命令，`cancel` 后进程消失、调用方收到"server stopped"。

## 我没做、要你裁的一件事

SPEC 把 `get_messages` 写在 `- server_script:` 组下。**它现在仍是 Rust 内建**
（target `server`）。理由：它要一次 SQL 就把 rows、`last_seq`、`status` 取成同一快照，
而脚本实现是每次读起一个进程（bridge 每回合结束都读），而且 `status` 是会话活状态。
机制已经支持脚本提供命令（示例见 `scripts/server_script_example.py`）；若你要按字面把
`get_messages` 变成 python 脚本，我可以照做，但上面两条（进程开销、单快照）会退掉。
