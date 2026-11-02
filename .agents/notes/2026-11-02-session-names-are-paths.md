# 会话名是路径，列表是树

日期：2026-11-02 · worktree `.worktrees/session-tree` · 分支 `feature/session-tree`

## 用户要的

> 让 session name 支持路径，例如 "a/b/c"；tui 要用 tree 的方式渲染（可展开折叠）。

## 批准的四点（用户「好」）

1. SPEC 新增 `## Session Name`，UI 那行补 tree 语义 —— 已写。
2. 折叠状态**只在内存**（像 `abstract` 的展开），不写 `cli.toml`、不加新命令。
3. group 行显示子孙**最强**状态（error > running > idle）。
4. 在 worktree `.worktrees/session-tree` 做，用户自己合并回 `main`。

## 改了什么

- `SPEC.md`：`## Session Name`；UI `[session{...}]` 那行加 `tree: ...`。
- `crates/server/src/session.rs`：`checked_name` —— `/` 分段，段不得为空。
- `crates/server/src/monitor.rs`：`reserved` 改成**逐段**检查 `@`（`work/@monitor` 也拒）。
- `crates/server/src/commands.rs`：`set_session` 先 `reserved` 再 `checked_name`。
- `crates/tui/src/tree.rs`（新）：`Row` / `Mark` / `rows()`，深度优先、显式栈。
- `crates/tui/src/panes.rs`：`list_names` → `list_rows`，加 `folded: BTreeSet`、
  `row_of`、`toggle_row`（点击时重建，不在 frame 里改形状）。
- `crates/tui/src/render.rs`：`draw_list` 画 `○   ▾ [api]`；`list_width` 按行算；
  `list_at` 返回 `ListHit { name, marker }`。
- `crates/tui/src/lib.rs`：`Part::Fold`；点 marker 折叠；点纯 group 的名字也折叠；
  纯 group 行的滚轮归列表。
- 文档：新 `docs/notes/tui-tree.md` + README 索引；同步 `tui-panes` / `tui-render` /
  `tui-lib` / `server-commands` / `server-session`。
- `scripts/tui_live_scenarios.py`：新场景 `session-tree`；顺带把四个场景开头
  `expect model fake` 改成 `expect session(s)`（见下）。

## 撤销的一个改动（记下来，别再来一遍）

曾经把 `tools/bash.rs` 的 `slug` 提到 `tools/mod.rs`，并让 `tools/detached.rs` 的
日志名 `logs/{session}-{job}.log` 走 `slug`，理由是我以为 `a/b/c` 会让 detached bash
起不来。**这是错的**：`DetachedJob::launch` 自己 `create_dir_all(log.parent())`，所以
`logs/a/b/c-1.log` 会被踏实创建出来。而且 `slug` 把 `a/b/c` 和 `a-b/c` 压成同一个名字，
比原来的嵌套目录更差（日志没有被任何目录扫描依赖，只从数据库行读回）。整段已回退。

## 验证

- `cargo build --workspace`、`cargo fmt --check`：干净。
- `comments-check`：与 `main` 的违规清单逐文件计数**完全一致**（33 条基线违规都是旧的），
  本次新增的注释零违规。
- `test-layout-check`：0 violations。
- 真帧（`python scripts/tui_live.py --scenario session-tree`，138 帧 / 46s，PASS）：
  四层断言全过 —— 树形（`a`/`b`/`[c]` 三层）、点 marker 折叠、点纯 group 的名字折叠、
  点名字仍然 show/hide 窗口、`a//b` 被拒并说明哪段为空。最后一帧：

```
○ ▾ a
○   ▾ b
○       [c]
○ ▾ lab
○     [x]
○ ▾ [work]
○     [web]
○     [api]
```

## 发现的旧问题（不是这次改动引入的，留给用户）

1. **四个 tui_live 场景的开头 `expect model fake` 早就不会出现了**：7615abe
   "the header drops the model" 把 model 从 header 拿掉，场景没跟着改，所以
   `single-stream` / `two-sessions` / `event-ui-toggles` 都在第 2 行卡住。已改成
   `expect session(s)`（唯一改动），但那三个场景里还有 `--mode line/none/full`，
   但现在 `set_render_session` 收的是 `--show <bool>`，跑到那一步仍会失败。
2. **这些场景会花真钱**：假端点只接管"服务端自己的模型"（`GONGGA_BASE_URL`/`GONGGA_MODEL`
   进的是 `cfg.model`），而 `set_session` 不写 `--model` 时用的是常量
   `DEFAULT_SESSION_MODEL = deepseek-official/deepseek-flash`，于是会话的回合直接打真 provider。
   本轮我踩了一次（`.dev/live/20261102-235156`：fake 端零请求，真模型在我的仓库里跑了几条
   bash 工具）。所以 `session-tree` 场景现在**不发消息**，只建会话、只看树。要修的话：
   tui_live 加一个只含假 provider 的 catalog（`GONGGA_PROVIDERS_CATALOG`），场景一律
   `set_session --model fake/...`。

## 没做的决定（留给用户）

- 兄弟节点排序沿用 announcement 顺序（新会话在前），没有按名字排序。
- 折叠状态不持久化：重启客户端全部展开。
