# 密钥为什么「配了却不在」：环境块是继承的，roster 是重读的

日期：2026-11-01

## 症状

用户说本机的 key「好像都没配上」。服务端给的是：

```
no key for provider minimax-cn: set MINIMAX_CN_API_KEY (the roster has no literal key either)
```

而注册表里 `MINIMAX_CN_API_KEY` 明明有值，`~/桌面/文章/安全/minimax-api.txt` 与它逐字节相同。

## 根因

Windows 上进程的环境块**只在创建时从父进程拷一份**。`setx` 写注册表，并广播
`WM_SETTINGCHANGE`——只有 Explorer 这类专门监听者会刷新自己的块；**已经存在**的进程和它们的
子孙永远看不到新值。这台机器上：

- 键是 2026-10-30 用 `setx` 写进 `HKCU\Environment` 的（见
  `2026-10-30-roster-splits-into-community-and-private.md`）；
- gongga 是从 2026-10-26 就开着的 VS Code（`Code.exe` 8:18 → `pwsh` → bash → server）里起的。
  它的继承链比键老，所以服务端进程里只有 `DEEPSEEK_API_KEY` / `DASHSCOPE_API_KEY`（这两个在
  VS Code 启动前就有）。

于是「键是持久的，服务端看不见它」——两者并不矛盾，也不该靠重跑一遍 `setx` 解决。

## 三个名字本来就没有，不是丢了

- `TENCENT_API_KEY`：`tencent-tokenhub` 的社区条目自己写着本机没有明文，`key_file` 那个占位
  服务端也不读。
- `BONSAI_API_KEY`：`local-qwen3` 是本机 llama.cpp，`bonsai` 这台机器上也不解析。
- `QWEN_CONSOLE_COOKIE`：只给额度脚本用，不参与推理。

社区 12 个 provider 里 10 个有 key，与 `~/桌面/文章/安全/*.txt` 一一对得上。

## 修法

写 `~/.agents/providers.toml`（私有层）10 条 `id` + `key_env` + `key`。理由是这一层**与谁启动
服务端无关**：`Live::is_fresh` 每次解析前按 size+mtime 比较，文件一变就重读，所以新建这个文件
当场生效，不用重启，也不用 `evolve`。

`credential()` 的顺序仍然是环境变量 > 文件里的 `key`：将来从新终端起来的服务端照样用环境变量，
两边都写不会冲突。私有文件只在本机，不进任何仓库。

## 验证

- `set_session --llm minimax-cn/MiniMax-M3`：改之前 `no key for provider minimax-cn`，改之后
  `ok: true`；`longcat/LongCat-2.0` 同样。
- 6 把 key 打各自 `/models`：minimax / longcat / qwen-token-plan / kimi / stepfun / xiaomi-mimo
  全部 200（deepseek 本来就在跑）。
- 探针会话用完 `del_session` 删掉。

## 没做的

- 没让服务端去读 `HKCU\Environment`：那会把「环境变量」偷偷变成「注册表」，是一次跨平台的语义
  扩张，得先进 SPEC。
- 没动 `tencent-tokenhub` / `local-qwen3`：一个没有 key，一个不需要。
