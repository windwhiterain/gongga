# roster 进程内缓存：读失败时退回到上一次解析成功的那份

User：「加上roster进程内缓存，解析失败用缓存」

## 起因

`~/.agents/gongga-agents/providers.toml` 卡在一次未完成的 rebase 里，留了
`<<<<<<< HEAD`。链路是：TOML 解析失败 → `providers=0` → 每个会话 `route()`
拿 db 里的 `provider/model` 字符串去 roster 换端点，换不到 →
`no provider "deepseek-official": the roster has no providers` → 服务端重启时又从空
roster 挑默认 provider，拿不到 key 直接退出（那条 `Error: no API key: set
MINIMAX_CN_API_KEY` 把人引向环境变量，真相是 roster 空）。

关键证据：`fix` 会话跑到 195 秒、前几步全部成功，某一步 roster 坏了，那一步立刻死。
因为 `agent.rs` 每步重读一次设置（注释写明了：`set_session` 要能对下一个请求生效）。
这个设计不能动——它换来的正是热改配置——所以缓存只能加在 roster 自己身上。

## 改动（`crates/server/src/providers.rs`，41 行）

复用已有的 `Live`（按文件 stamp 缓存那份 `Arc<Catalog>`），不新起一套：

- `Live` 加 `last_good: Option<Arc<Catalog>>`：最近一次**解析成功**的 roster。
- `roster()` 读到 `problem` 时，若 `last_good` 在，就返回 `last_good.blaming(problem)`
  ——一份带着真实病因的旧列表。`Catalog` 因此加 `Clone`。
- `blaming` **保留 generation**：serving 的还是 client 当初构建时那份，
  一次失败的读不该 retire 任何东西。
- 只有 `problem.is_none()` 的读才能写进 `last_good`：否则「带病因的副本」会顶掉
  它自己的来源，下一次失败就服务一份没有任何一次编辑产出过的列表。
- 坏文件的 stamp 照常记下，所以坏着的那段时间不会每次调用都重读、不刷屏。
- 日志加 `serving_cached`，下一个 agent 一眼看出这份列表来自文件还是缓存。

## 验证（L3，临时 home + `GONGGA_PROVIDERS_CATALOG`，不碰真 home）

真服务端起在 `127.0.0.1:19700`，catalog 是一份副本（13 个 provider）：

| 场景 | 结果 |
| --- | --- |
| 首次读 | `providers=13 generation=1 serving_cached=false` |
| 运行中插入 `<<<<<<< HEAD` | `providers=13 generation=1 problem="TOML parse error at line 1 ... invalid key" serving_cached=true`；同时 `gongga-cli set_session --model deepseek-official/deepseek-flash` 返回 `ok:true`，还解析出 `thinking_effort:"high"` |
| 修好文件 | `providers=13 generation=3 serving_cached=false`，立刻回到文件 |
| 冷启动时文件已坏 | `providers=0 serving_cached=false`，如实报错——缓存不掩盖「进程起来时就没有好列表」 |

## 边界（没做，也不该顺手做）

- **冷启动救不了**：进程内缓存没有跨进程的过去。要救那种场景得落盘，那是另一个决定。
- **启动报错仍然指向环境变量**：空 roster 明明带着 `problem`，`Error: no API key`
  却只说 `MINIMAX_CN_API_KEY`。这是另一处值得改的地方，本轮没动。
- `cargo test --workspace` 是 0 个测试（`5c8998b tests: the suite is gone`），
  所以正确性只有上表这一份 L3 数据。
- 工作树里 `crates/tui/src/lib.rs` 有一处 `mod render;` → `pub mod render;` 不是本轮的改动。
