# SPEC 把 `get_messages` 改名成 `--range`，并归到 `server_script` 一类

## 法改了哪两处（工作区的 SPEC.md，未提交）

1. `## Cmd` 里 `get_messages` 从 `- server:` 挪到新的 `- server_script:` 分组，
   参数从 `--seq <a:b>` 改成 `--range <begin:end>`，语义写明**闭区间 + 负数从末尾数**。
   旧法里"the rows, `last_seq`, and the session's `status`"这句随这次移动一起没了。
2. `## Polity` 由长条款版缩成 `## Skill: Polity`（三个角色 + `judgement` 要记录）。

## 程序这边做了什么

- 全链路改名：服务端 catalog + 处理函数（`crates/server/src/commands.rs`）、
  桥的探测与读回（`crates/bridge/src/main.rs`，常量 `READ_SEQ` → `READ_RANGE`）、
  以及超限提示（`net.rs`）里让用户"命令行该怎么写"的那句。
- 保留 `last_seq` / `status` 两个回包字段。新法只说"return the rows"，没有禁止多余字段；
  桥靠 `last_seq` 推游标，等的逻辑靠 `status` 判断回合是否收尾。删了就是把能用的东西弄坏，
  而法没要求这么做。
- `server_script:"that registered by external python scripts"` **没有实现成插件宿主**。
  现在没有任何"外部脚本注册命令"的机制，`get_messages` 是 Rust 内建命令，脚本通过
  CLI/UDP 调它。按"脚本调用的命令"读，现状已满足；按"脚本注册命令"读，那是一个新功能
  （协议、生命周期、home、失败语义都得设计），不该在"照法修订"里顺手发明。**这条待裁。**
- 顺手把 `commands.rs` / `store.rs` / `cli/commands.rs` 里 14 处超预算注释改短
  （`comments-check` 在 HEAD 上就是红的，不是这次改出来的）。

## 证据

- `cargo test --workspace`：20+68+13+5+152+63 全过。
- `comments-check --config comments-check.toml`：0 violation（HEAD 时 14）。
- `test-layout-check --config test-layout.toml`：0 violation。
- `cargo fmt --all -- --check` 干净；`cargo clippy` 的 5 条 warning 都在我没碰的
  `llm.rs` / `session.rs` / `agent.rs`（HEAD 就有）。
- 真服务端跑一遍（临时 home、端口 19700，跑完已 kill 并删除）：
  - `get_messages --session nope --range 1:2` → cmd 回显 `{"range":"1:2",...}`，报 no session；
  - `--seq` 被 catalog 拒：`get_messages has no argument --seq; it takes --session, --range`；
  - 新建会话上 `--range -5:` → `no rows ... (transcript ends at seq 0)` + `status: idle`。

## 没动、但看着不对的

`AGENTS.md` 还写着 `polity init/up/dispatch/wait`，而 `.agents/skills/polity/` 在工作区里
已被删掉（不是我删的）。SPEC 也正好把 Polity 缩成一段。要不要连 `AGENTS.md` 的技能索引一起改，
等 user 说。
