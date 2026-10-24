- SPEC is the only source of truth (agents should not edit without user's permission).
- when designing new feature, search any existing wheels. 
- setting any limit/budget is forbidden.
- session turn end should kill all process it spawns so user won't use a process that tied to a session as a long running process.
- server home for tests should be local to each worktree so worktree's servers won't conflict (otherwise will be locked).
- track your WIP under (notes)[.agents/notes/].

## Verification
- tests layout: 
  ```
  cargo install --git https://github.com/windwhiterain/test-layout.git test-layout-check --locked
  test-layout-check --config test-layout.toml
  ```
- comments test:
  ```
  cargo install --git https://github.com/windwhiterain/comments-test comments-check --locked
  comments-check --config comments-check.toml
  ```
## 自我进化（改服务端之后怎么让自己用上）

整套流程在 `.agents/skills/evolve/SKILL.md`，脚本是 `.agents/skills/evolve/evolve.sh`：

```bash
S=.agents/skills/evolve/evolve.sh
$S stage        # 跑测试 + 构建 + 暂存 + 生成 launcher（不重启任何东西，先报磁盘）
$S stage --clean # 同上，并让继任者在旧服务端退出后跑 cargo clean（磁盘紧张时用）
# 然后用 bash --detached 启动它打印的 launcher
$S swap <stamp> # 确认继任者真在等，再发 cancel
$S status       # 当前 stamp、谁在等、端口与锁
$S clean        # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
```

三条必须记住的：

- **先起继任者，再 cancel**。反过来一旦新二进制起不来，home 就空着、没人 resume 本回合。
- **不要写交接笔记**。resume 会把完整 transcript 原样重放，笔记不会多提供任何信息；
  要验的是外部状态（pid / 端口 / pending），那部分只能重新测量。
- **换完看继任者自己的日志**有没有 `replaying a turn ... resumed=true`，那才是"接上了"。

## 技能
skill 是"任务相关才读"的散文 + 脚本，放在 `.agents/skills/<name>/SKILL.md`。**别全读**，
先列索引，命中才读正文：

```bash
grep -H -m3 -E '^(name|description):' .agents/skills/*/SKILL.md
```

- `self-control`：控制自己的回合 / 进程 / 上下文 / 服务端。
- `web-search`：查实时网页、读 URL 正文（DeepSeek REST，不花模型轮次，任何模型都能用）。
- `evolve`：换掉正在跑自己的服务端（改完 server 代码之后）。
- `image`：让模型真看到一张图。用 `.agents/skills/image/show` 打印 `@路径`，harness
  自动上传到 Files API 并作为 `input_image` 交给模型——不要自己拼 base64。
- `proxy`：按 host 路由并证明它真的走了。`.agents/skills/proxy/proxy` 是自实现的纯标准库
  代理（HTTP/SOCKS5/直连上游，用户可接任何 API 端点）。先 `doctor`，`upstream add` 加上游，
  `add host --via` 定规则，`check host` 看代理自己记下的去向。出口 IP 只在两条路由相同时
  才说明问题——`check` 会明说。改配置后要重启 `serve`。
- `computer_use`：用鼠标键盘操作真实桌面。`.agents/skills/computer_use/cu` 先跑 `doctor`，
  然后 `shot` 看屏 → `tree` 拿控件矩形 → `click`/`type`/`key`（都带 `--window`）。
  `cu.py` + 每平台一个后端（Windows ctypes/UIA 已实测，macOS/Linux 已写未实测），
  uv 解析内联依赖，不需要 pyautogui/pywinauto。这是用户的真实桌面，打字前先确认目标窗口，
  截图会把屏幕上的密钥一起带走。
- `compact`：折叠自己的上下文。收到 `Context notice` 时读它，用
  `.agents/skills/compact/transcript.py`（脚本，不是服务端工具）——服务端只测量和提醒，
  不再替你压缩。read 读的是消息（模型输出 + 注入），工具调用与 reasoning 被过滤掉。
