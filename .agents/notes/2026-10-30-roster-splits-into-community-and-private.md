# LLM 目录拆成社区层 + 私有层，并把 llm 改名 providers

日期：2026-10-30

## 要解决的问题

`~/.agents/llm.toml` 一直是一台机器一份、65KB、里面既有「所有机器都一样的事实」（模型目录、
上下文、模态、价格、端点）又有「只有这台机器知道的事」（密钥、内网 base_url、自建 provider）。
结果是每台机器各漂各的，改一处价格要找到并改 N 份。

## 现在的形状

两个根，同构（`providers.toml` + `providers/`）：

| 根 | 谁的 | 谁维护 |
| --- | --- | --- |
| `~/.agents/gongga-agents/` | 社区，`windwhiterain/gongga-agents` | 服务端：缺就 clone，启动 + 每小时 `pull --ff-only` |
| `~/.agents/` | 用户私有 | 人 |

- 合并：按 provider `id` 字段级；`[[provider.endpoint]]` / `[[provider.model]]` 同样按 `id`。
  顺序以社区为准（`default_entry` 的偏好顺序），私有新增追加在后。
- 社区文件零密钥：只有 `key_env`；原来的 13 处 `key = "sk-..."` 在进仓库前就删掉了（已确认
  `grep -E '"(sk|ak|flm_ak|user)_' providers.toml` 无输出）。
- 脚本不用额外查找表：`quota_query.script` 本来就相对 provider.toml 所在目录，所以社区条目落
  在 clone 里、私有条目落在 `~/.agents/providers/`。
- `GONGGA_PROVIDERS_CATALOG=<path>`（原 `GONGGA_LLM_CATALOG`）仍然是「就这一个文件」。

## 改了哪些地方

- 新模块 `crates/server/src/agents.rs`：`Roots` / `sync` / `start`，clone 先 https 再 ssh，
  `.git` 不存在但目录非空时视为 `Foreign`，一个字节都不写。
- `providers.rs`：`load_layers` + `merge_into`（TOML 值级合并）+ 每文件 `Stamp`；`path()` 换成
  `paths()`。
- `lib.rs`：`agents::start()` 在 instance lock 之后、任何 roster 读之前；日志 `provider roster imported`。
- `ship/` 内容仓库（已发布，pin `61da64e`）：`web-search.sh` 的默认 roster 路径、evolve/self-control
  的说明、config.toml 注释。
- 机器上：`~/.agents/llm.toml` → `providers.toml`，`~/.agents/llm/` → `providers/`；社区 clone 已
  跟到改名后的 commit。

## 私有那份最后被删掉了

对比结果是：私有文件与社区文件**逐字段一致**，唯一差别是 13 个 `key`——没有私有独有的
provider，也没有改过的 `base_url`。所以"合并"是空操作，社区那份本来就是全量。

于是把密钥搬到环境变量（与各自 `key_env` 同名，`setx` 写进用户环境，逐条按注册表读回比对
`match=True`），然后删掉 `~/.agents/providers.toml` 和重复的 `~/.agents/providers/`（与社区
那份 `diff -r` 完全相同）。现在这台机器只有社区一层 + 环境变量，roster 仍是 15 个 provider。

两个没有 key 的 provider 一直就没有（`tencent-tokenhub` 要 SecretId/SecretKey 签名，
`local-qwen3` 是本机 llama.cpp），和这次改动无关。

**换了环境变量就得让服务端重新起**：`setx` 只对新进程生效，正在跑的那个进程里没有这些变量，
而私有文件已经没了——它下一次请求会解不出 key。

## 试过又撤掉的

- `clone --depth 1` + `pull --depth 1`：浅取把新 commit 的父节点 graft 成根，`--ff-only` 从此
  认为分叉，每次刷新都失败。两个文件不值得。
- 私有 `[[provider]]` 整块替换：私有文件最常见的全部内容是一个 `key`，整块替换等于把社区那条
  40 行抄一遍，然后开始漂。

## 没做的

- 服务端仍然不跑 `providers/*.py`：那是 quota 查询，读的人现在只有脚本自己。要跑它是功能，得先进
  `SPEC.md`。

## 已知环境问题

这台机器上 https 到 GitHub 被本地代理打断（CONNECT 502），所以 `sync` 的 ssh 回退是常态；
`~/.gongga/ship.git` 缓存已手工灌入新 pin，否则下一次启动会报「取不到 pin 又没缓存」。
