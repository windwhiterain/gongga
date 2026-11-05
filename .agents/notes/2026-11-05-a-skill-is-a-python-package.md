# 一个 skill 是一个 Python 包，公开 API 就是它的函数

2026-11-05。用户的判定：**Python 是一等公民**——`gongga_skill` 跑入口，`gongga` 是客户端，
`gongga-cli` 只是 wrapper。上一版把入口固定成「一个文件 + 扩展名/`#!` 决定谁来跑它」，于是
每个 skill 都得先写一个脚本、再在正文里教人怎么调它的命令行；要复用其中一步，就得再写一层
命令行解析。包形态把这一步删掉：函数就是 API，命令行只是它的一个调用者。

## 形状

- 目录里有 `__init__.py` 就是一个包；**包压过声明式入口**。
- 公开 API = 模块全局里**不以 `_` 开头**的可调用对象，按声明顺序。`__all__` 不读：`SKILL.md`
  才是「这些函数是干什么的」的那份答复，文件自己再藏一遍就是第二个答案。导入进来的函数
  也算公开——想留私有的用 `import json` 后写 `json.dumps`，或者加 `_`。
- 第一个词是函数名，其余的词按参数顺序绑定，`--name value`（或 `--name=value`）点名；
  `int`/`float`/`bool` 注解转换，别的按原样。填不满就报签名，不甩 traceback。
- 返回值：`None` 什么也不打印，`str` 原样打印，`int` 是退出码，其余按 JSON 打印。
- 一份文件两个读者：`python/gongga_skill.py` 是契约本身。Python 调用者 `import gongga_skill`
  后 `run(directory, function, words)`（进程内，没有命令行）；harness 用 `include_str!` 带着
  同一份字节，跑 `python -c <source> <dir> <function> <words…>`。谁都不解析名字——「这个名字
  打开哪个文件」只有一个答案，那不是在 Python 里。
- 声明式入口 `metadata.entry` 留着，是**脚本形态**，给包形态之前写的 skill 用。`entry.rs` 的
  扩展名/`#!` 规则因此没删。

## 实验：subagent 是第一个包

`ship/skills/subagent/spawn.py` → `__init__.py`，公开函数
`spawn(role, remote=None, cwd=None, model=None, context_limit=None)`，其余 helper 全部加 `_`。
SKILL.md 去掉 `metadata.entry`，例子改成 `gongga-cli skill subagent spawn impl …`。

L3 `scripts/subagent_selftest.sh` 抓到两个真 bug，都不是「看起来不对」能看出来的：

1. **`spawn` 在重试循环里把参数 `model` 改成了命令行 flag 列表**（`model = ["--model", stated]`）。
   第一次撞击名成功，第二次 `stated = model or env_model` 拿到的是一个 list，直接进了
   `subprocess.run([...])`，于是 `gongga_skill: expected str, bytes or os.PathLike object,
   not list`。改名成 `model_flags`/`named_model` 修掉。B 段的两个红灯同源。
2. **H 段的检查读错了地方**：`9e07fb3`（同日早些时候）把 index 从 `instructions` 挪成一条
   消息（`docs/reference/prefix-cache.md`），而 H 还在 `instructions` 里找 `EARLY file named`。
   改成读「prose + index + card」这段 head，card 是它的结尾。变异检查：把 `LATE skill body`
   塞进 index 那份请求，检查必须红（exit 1），未塞时绿。

## 顺带清掉的过时说法

- `ship/config/config.toml` 的系统提示、`ship/README.md`、`remote` / `self-control` 正文里
  还写着 `index_skill`——工具早就并入 `skill`（`3a4bb31`）。历史那一句（"发现改成
  `index_skill`"）保留。
- `docs/notes/cli-skill-command.md` 仍把 `which` 当成一个动词（`34adf06` 删了）。
- `crates/cli/src/skill_command.rs` 的模块注释同样还说 "say which file a name opens"。

## 发布

内容仓 `4201d80`，`scripts/ship.sh --check` 与远端一致（protocol 没动：形状是内容，不是
protocol）。跑着的服务端**下次启动时**才装这份内容，所以现有窗口里的 `skill subagent`
仍是旧的脚本形态——`evolve` 之后才是包。

## 与 2026-11-02 那份笔记的关系

`2026-11-02-an-entry-is-declared-once.md` 的结论（不要 `<home>/bin` 的 shim、一份三根的解析、
`metadata.entry`、不加 lockfile）都还成立。变的是：**入口不再是「声明一个文件」为第一形态**，
声明式入口降级为脚本形态，包形态是第一形态。

## 一等公民的后半截：程序拿得到"我是哪个会话"

包形态让函数成了 API，但一个包里的 Python 拿不到它该拿的东西：只有 **bash shell** 被服务端
告知自己属于谁（`GONGGA_SESSION` / `GONGGA_MODEL` / `GONGGA_REMOTE`）。`exec` 的程序、Python
解释器、skill 的入口进程，继承的都是**服务端自己的**环境——那是启动它的那个进程的环境，与调用者
无关。本轮在会话 `arg` 里实测：`python` 工具回答的 `GONGGA_SESSION` 是 `tui`。

这不是少一个便利，是一个错的答案，而且 `subagent` 的 `spawn` 正是从那个变量决定"父会话是谁"。

改法（`57ed2fe`）：`session_vars` 是一个会话加在任何 base 上的部分（名字、模型、机器，每次从
行里读，所以 `set_session --model` 之后下一个孩子看到的就是新的）；`SessionHandle::env` 是给
**不是 shell** 的程序的 base（同样的事实 + Windows 读的原生 `PATH`）。`ToolContext::env` 就是
它，`exec` / `python` / `skill` 把它交给子进程，`bash` 照旧用已有的 shells。

同一次还补了 `GONGGA_SKILL_LIB` 的第三个读者：`skill` 的入口进程。`gongga_skill.py` 在 import
时把这个目录放进 `sys.path`，于是包里的 `import gongga` 和驱动自己看到的是同一份。

观测（临时 home + 临时端口 + 一个会 `import gongga` 的包，跑完删掉）：shell 里 `GONGGA_SESSION=arg`
时，对会话 `other` 调 `skill` 工具，包回答 `session=other`、`client_pid` 等于那台服务端的 pid、
`sessions=['other']`；`python` 工具同样。

## 换手时撞到的两个过时测试

`evolve`（把跑着的服务端换成新二进制）之前跑全量自测，红的三条都不是本轮的改动：

- `subagent_selftest` 的 G/J/M 还在等**服务端替订阅说话**的那句 notice，而 `222847e` 已经把它
  删了（一次匹配只跑行里带的命令，subagent 的 news 是它自己的 `send_message`）。之前"绿过"
  是因为被测的二进制比 checkout 旧——脚本最后打印的 sha 是 checkout 的，不是二进制的。
- `subscription_selftest` 从 `spawn` 里取 relay，而模块已经是 `subagent`（包形态）。

两条都按现在的契约改：notice 是 `turn_end <会话>: {…事件…}`；`wait` 面对一次到达可能先说
"a message arrived"（回调的 `send_message` 同时是 pending 行和 inbox 信号，谁先被看见不是契约）。
