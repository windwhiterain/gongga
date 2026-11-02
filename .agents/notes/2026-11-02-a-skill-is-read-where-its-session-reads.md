# cmd_input 的 skill 检索目录：session 的 cwd，不是窗口的 cwd

User：「cmd_input 里的 skill 的检索的搜索目录 cwd/.agents/skills 是基于 server 的 cwd，
不是 session 的 cwd，修复这个 bug，以及检查一下其他 cwd 用错的地方」。

## 量到的失败

`.dev/skillprobe/project/.agents/skills/zzzprobe/SKILL.md` 只存在于该目录，
然后让 session `main` 的 cwd 指向它，窗口本身跑在别的目录：

- 未修：`@skill:` 弹窗给出 `evolve heir image monitor remote self-control subagent`
  ——`evolve` 是**窗口 cwd**（仓库）的 project skill，`zzzprobe` 一个都没有。
- 修后：`zzzprobe` 第一行。

同一个 base 的两处也一起量了（同一份脚本，改前红、改后绿）：

| 输入 | 未修（窗口 cwd） | 修后（session 的 cwd） |
| --- | --- | --- |
| `@skill:` | `evolve`… | `zzzprobe` |
| `@file:zzz` | 无（窗口目录里没有 zzz*） | `zzzfile.txt` |
| `--cwd su` 补全 | 无 | `sub/` |

服务端 `set_session --cwd` 的真实语义（一份临时 home、一个服务端，客户端在别处）：

| 输入 | 结果 |
| --- | --- |
| `--cwd <绝对路径>` | 原样落下 |
| `--cwd inner`（会话已在 `.../moved`） | `.../moved\inner` |
| `--cwd ~` | `C:\Users\17124` |

## 改了什么

数据，不是特例。`SessionInfo` 里本来就有 `cwd` 和 `target`，只是客户端把它们丢了：
catalog 只留名字，另一张表留 target。现在 catalog 按发过来的样子留整个 `SessionInfo`，
再加两个"没有会话可问"时的答案：`message_session`（sticky 的 session，一条消息唯一的地址）
和 `server_cwd`（greeting 里 `ServerInfo.cwd`，本地会话没给 `--cwd` 时就从这里开始）。

`Catalog::place` 每条输入算一次：机器先看 `--target`，再看这行会话的 target，最后才是本机；
目录则是这行会话自己的，没会话才用服务端那个。skill、`@file:`、`@image:`、`--cwd` 都读它。

skill 名字不再由 `sync_sticky` 推给 catalog，而是按 `SkillKey`（project 目录 + home）缓存、
用到时才读：旧写法只在"记得调它的那几个 call site"是对的，会话一移动就旧了。

服务端两处：

- `session_info` 的 `cwd` 改成**会话自己 shell 报的目录**（`session.cwd()`，就是
  `ToolContext.cwd` 那个 datum）。原来发的是 row 里的"放置点"：shell `cd` 走之后，
  工具读 A 而客户端按 B 补全。
- `session::stated_directory`：`--cwd` 先解析成绝对路径再比较、再存。相对路径读自
  会话自己的目录（移动本机会话时）或服务端目录（新会话），`~` 展开；目标是别的机器时
  原样交给对端。修之前 `--cwd sub` 存的是字面 `sub`，靠服务端进程的 cwd 去解释，
  而补全按客户端的 cwd 读——`--cwd ~/work` 更是直接被 shell 拒。

## 顺手发现、没改的

`scripts/tui_live.py` 的回合**会打到真 provider**：`set_session` 不带 `--model` 时用
`config::DEFAULT_SESSION_MODEL = deepseek-official/deepseek-flash`，而 harness 只把
`GONGGA_BASE_URL` 指给 fake。我量 `cd` 那一轮时确实花了几个真 token（deepseek-flash，
两次），`expect hi-from-fake` 之所以"通过"是因为模型的推理文本里提到了这个字符串。
真正的 fake 回合要 `--model fake/stream-model`，但那要求 roster 里有个 `fake` provider，
而 `_child_environment` 把 `GONGGA_PROVIDERS_CATALOG` 删掉了——所以现在无法只靠
tui_live 跑出假回合。这是 harness 的问题，不是这次改动引入的。

`list_paths` 的本地分支仍按服务端进程的 cwd 解释相对路径（`session.rs::list_paths` 的空 path
分支）；客户端现在很少走到它（本机补全直接读盘），但语义上它和 `stated_directory` 不一致。
`@file:` / `@image:` 在远端会话上补全走 peer 的 listing，而服务端 `meta::expand` 仍在本机
`std::fs::read`——远端会话的这两个引用注定读不到，这是既有的洞。
