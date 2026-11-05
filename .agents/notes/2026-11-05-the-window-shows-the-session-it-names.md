# 窗口只画它自己点名的会话

用户 2026-11-05 的裁定：**TUI 收到新建会话不要自动 set render session**；TUI 自己
在本地检测**发出的** server 命令里的 session type argument，有就设 set render session。

## 改了什么

- `Shown::is_shown`：没人设过的会话**默认不画**（原来默认画）。窗口画它被要求画的
  东西；服务端announce 出来的会话没人要求过，就不画。
- `Client::session_named`：从 catalog 的 **kind** 读哪个参数是会话，不是参数名 ——
  `set_session --name` 和 `get_messages --session` 名字不同，kind 相同。
  client 命令一律不认：`set_render_session` 自己也有 session 参数，跟着它会把
  它刚说的话反过来。
- `Client::show_session` / `show_named_session`：无声地显示那个会话，并让 sticky
  session 跟着它走（显示一个会话就是让窗口读它）。
- TUI（`crates/tui/src/run.rs`）：命令发出去**之前**自己判断，不等服务端的会话公告。

## 为什么默认改成「不画」

只有默认是不画，第二条规则才有意义：默认都画的话，「命令点了名的会话才画」几乎是
空话。代价是：新开的窗口在点一个会话之前，事件区是空的（会话列表还在）。

## 验证

`.dev/verify/ad2b120-tui-render-session.json`：真帧（`scripts/tui_live.py`）+ 第二个
client 在同一个 server 上建会话。

- TUI 自己 `/set_session main` → `cli.toml` 里 `render_session main = true`，事件区画出 main。
- 另一个 client `set_session --name other` → other 只在列表里，没有窗口，cli.toml 里没有它。
- 一个谁也没点名的窗口 → 两个会话都在列表里，事件区没有会话窗口。
- 点列表里的名字 → 那个会话的窗口画出来。

门禁：`cargo build`、`cargo fmt --check`、`cargo test --workspace`、
`test-layout-check`（0）、`comments-check`（干净）、`scripts/skill_contract.sh`、
`scripts/ship.sh --check`（0，一致）。

## 留下的话

- `SPEC.md` 没动：它没有写默认，也没有写「命令点名的会话要画」。这一条现在只活在
  代码与 `docs/notes/` 里；要写进 SPEC 得先问。
- 同一个 `cli.toml` 被 TUI 和一次性 CLI 共用（都在 server home 下），所以一次性 CLI 的
  `set_session` 也会写 sticky session —— 那是共享 home 的老事实，不是这次改的。
