# 引用带 remote，技能看到对端的 checkout

用户指令（2026-11-17）：skill 要能加载远端 cwd 下的 `.agents/skills`；meta 引用要能带 remote，默认 sessions' remote，
且只能一层括号（`remote://<name>/<path>`），不做第二个括号组；`@skill(read image)` 要能产生 remote 的引用。
上一轮已统一到 `Space`，这轮把这三件事都接到那条路径上。

## 1. 引用的机器：`Machines::in_space(remote)`，默认 session 的

`Mention` 多了 `remote: Option<String>`。解析是**读**不是搜：引用后紧跟的
`remote://<name>/` 前缀才算，且必须完整（名字后面要有 `/`）。

`meta` 的入口从 `&Space` 变成 `&Machines`：`in_space(None)` 是 session 自己的 space，
`in_space(Some(p))` 是 `Space::peer(p)`。所以一条消息可以同时引用本机文件和对端图片，
每个引用在自己说的机器上读——这正是「统一路径」：解析机器只有一处，读只有一处
（`spell` / `read_bytes` / `list`）。

`remote://` 是 **spelling 的一部分**：`@file(x)` 和 `@file(remote://p/x)` 是两条
索引项，否则同一路径在两台机器上会撞成一条。

## 2. `Space::peer(name)`：进程内一个 remote 一条连接

名字走本机 `~/.agents/remotes.toml`（`crate::remotes::load`）。未知名字**退回 session
自己的 space**——引用写错不该变成这个机器凭空造出来的路径。`PEERS` 是进程级表，
所以引用读和工具读共享同一条 runner。

## 3. 技能的项目层在对端

`tools::skill::installed_in(&Space, cwd)` 取代 `installed_for(cwd, machine)`：

| 层 | 在哪读 |
| --- | --- |
| `project` | session 所在机器（对端就经它的 `Space`） |
| `harness` | 本机（`<home>/skills`） |
| `general` | 本机（`~/.agents/skills`） |

后两层是**这个 server 自己的**，所以固定在 `Space::local()` 读——第一版我错写成用
session 的 space 读，结果对端 session 的 `ls_skill` 只剩下对端项目层的 `peer-only`，
本机的 `image` 直接找不到。`ls_skill` / `run_skill` / `@skill(...)` 共用这一条 walk。

`skill` crate 新增 `declared_of(text, dir)`：从字节解析 frontmatter，因为对端
`SKILL.md` 只拿得到 bytes，不能 `read_to_string` 一个本机路径。

## 4. `@skill(read image)` 产生 remote 引用

技能**在本机跑**（`run_skill` 的既有 invariant，没动）。所以 `image.show` 拿到的是本机
cwd，看不到对端的图。接法是 environment：`session_vars` 在 session 有 remote 时给
`GONGGA_REMOTE` 与 **`GONGGA_REMOTE_CWD`**（session 在对端站在哪）。

`image` 技能据此分两支：

```
本机：  @image(C:/x/shot.png)                       # 读字节、校验格式
对端：  @image(remote://lichen-vm/C:/Users/Admin/proj/shot.png)    # 不读，交给 harness
```

相对路径按 `GONGGA_REMOTE_CWD` 拼、`\`→`/`（`space.rs::spell` 读的是 `/`）。
`check=true` 在对端只报「在哪、拼成什么」，因为字节不在这台机器上。

`attach_references` 也按每个引用的 `remote` 选 space 与目录：带 `remote` 的按
session 目录拼，不带的按「哪个工具打印的」那一台拼——旧行为一字不改。

## 发布

`image` 是内容仓的，所以走 `scripts/ship.sh -m "image: show names the peer a reference is on"`：
protocol 13/13 未动（形状没变，只是 `show` 多认两个环境变量），推成 `38c5b25`。
`--check` 与 `--dry-run` 都 exit 0。

## 验证

`bash scripts/remote_meta_selftest.sh` → **PASS · 344s**，真 server + 假模型 + 操作者
自己的 `lichen-vm`，七项：

- session 建在对端，`exec` 造出对端 scratch（含一张 png、一个 md、一个**对���自己的**
  `.agents/skills/peer-only/SKILL.md`）；
- session 移到那里后，消息里的 `@file(notes.md)` 内容进了请求（`peer rules` True）；
- `ls_skill peer-only` 找得到**对端 checkout 里**的技能；
- `@file(remote://lichen-vm/notes.md)` 内容进了请求；
- `run_skill image show shot.png` 打印 `@image(remote://lichen-vm/...)`；
- 这条引用写进消息后，对端的图被读入、复制进本 home、并作为 `input_image` 进了模型请求。

数据在 `.dev/verify/ba20d889-remote-meta.json`。门禁：`cargo fmt --check`、`cargo build`
（0 error）、`comments-check`（我这个仓 10 条，与 main 基线同为 10，我碰过的文件 0 条）、
`test-layout-check`、`scripts/skill_contract.sh` 全绿。

## 没做的

- 技能仍然只在本机跑；让它去对端跑是另一件事（要装解释器、要搬 package）。
- `Space::peer` 每次按名字查 `remotes.toml`；配置改一次要重启才生效，与既有 remote
  目录的规则一致。
- 客户端补全里 `@skill(...)` 仍然只列本机的层（它只有本机账本）；对端项目层的名字
  要客户端知道当前 session 在哪个 peer 才有意义，那是 catalog 的事。