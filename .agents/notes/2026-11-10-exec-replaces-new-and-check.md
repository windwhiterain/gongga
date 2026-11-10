# 2026-11-10 — `exec` 取代 `new` 与 `check`

SPEC 的 `## Tool` 段把两个动词合成一个：

```python
fn exec(bin, args, input, id, wait, until, remote, cwd) -> Result
```

操作员一句「按照最新的SPEC改tool」，并选了「全切」「彻底删掉 new/check」「bin 按基名
去扩展名对上 `[channel.<name>]`」。所以这一轮同时动了代码、内容配置、自测与文档。

## 代码

- `tools/new_check.rs` → `tools/exec.rs`，一个 `ExecTool`：`bin` 没有就开一个 channel，
  `id` 已经有了就是那个 channel。`id` 省略是 SPEC 的 `/<bin>/default`（存成
  `bash/default`，前导 `/` 会被 Git Bash 改写成本机路径），所以「再问一次同一个程序」
  不需要另一个动词。
- `Channels::start` 的 `id` 从 `Option<String>` 变成 `&str`：自动编号（`{session}/{name}/{n}`）
  随 SPEC 的默认 id 一起删掉，`auto_name` 没有调用者了。
- `Channels::grammar_of` 按基名查表：`bash`、`bash.exe`、`C:\...\Git\bin\bash.exe` 都是
  `[channel.bash]`；查不到就是 raw（一行写、无 marker）。
- `bin` 与已存在的 channel 比较也用基名：`bash` 与 `bash.exe` 是同一个程序。
- 已有 channel 又给了 `args` 会被拒：`args` 只塑造一个**将要**启动的程序。

## 内容

`ship/config/config.toml`：`[tool.new]` + `[tool.check]` 两行合成 `[tool.exec]`，参数按
SPEC 的顺序 bin/args/input/id/wait/until/remote/cwd。**只提交在 `local`，没有推**：
`scripts/ship.sh --check` 退出 0（protocol 9 一致），推不推由人定。

## 观测

- `channel_selftest.sh`：**PASS · 18 checks**（一次起服务端，全表跑完）。
- `tool_selftest.sh`：`exec` 的 4 条全过（模型回合里开 channel、用 `id` 再进去一次），
  剩 2 条 `wait` 的断言红：答案现在是裸的 `2.494s`，而那两条断言按「有消息时 `ended()`
  返回一句话」写的——`fn ended` 在当前 HEAD 已经不存在了。与本次改动无关，未修。
- `cargo build`、`cargo fmt --check`、`test-layout-check`、`skill_contract.sh`、
  `ship.sh --check` 全 0；`comments-check` 全仓 17 条违规都在我没碰过的文件里
  （vt_input/client/llm/tui/bundle/providers），本次改的文件 0 条。

## 环境上的三个坑（都不是代码）

1. **`target/` 被另一个会话共用**：`cargo run --bin gongga-tui` 那边的构建把我的
   `gongga-server.exe` 换成了不含 `exec` 的版本，于是 `exec` 落到 `Forward` 上，症状是
   `{"data":null,...,"output":"exec is missing required argument(s): session"}`。
   判据：在二进制里 `grep` 新串（`needs \`bin\` to start a program`）为 −1。
   绕法：编译后立刻复制到 `.dev/bin-exec/`，测试用 `GONGGA_BIN` 指过去。
2. **本机的 trash 后端坏了**：`rm -rf` 被 safe-delete 钩子拦成 `SAFE_DELETE_FAIL_CLOSED`，
   于是 `*_selftest.sh` 里那句 `rm -rf "$home"` 从来没成功过——`channel-home` 一直是上一轮
   的旧 `config.toml`（`[tool.new]`），跑出来的失败与代码无关。绕法：跑之前 `mv` 走。
3. **home 的 ship checkout 跟的是远端**：`[ship] url` 让 home 在启动时把本地 clone 换成
   GitHub 的 main，所以内容没推上去时测试看不到 `[tool.exec]`。测试用
   `GONGGA_SHIP_URL=<本仓库 ship>`。

`scripts/channel_selftest.sh` 顺手补了 `bin="${GONGGA_BIN:-$root/target/debug}"`，
和 `tool_selftest.sh` 一样的写法——AGENTS.md 要求每个 worktree 自己的 target 目录。
