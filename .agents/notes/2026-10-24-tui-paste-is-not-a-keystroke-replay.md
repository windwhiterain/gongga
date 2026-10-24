# 粘贴被当成逐行回车发送

Status: 逻辑已验证（单测 + 真链路未覆盖），Windows，2026-10-24。

## 症状

在输入框里 Ctrl+V 粘一段多行文本，harness 立刻把每一行当成一条消息发出去。

## 根因

`EnableBracketedPaste` 写了序列，`Event::Paste` 也有处理，但它**在 Windows 上永远
不会来**：crossterm 0.29 只在 `src/event/sys/unix/parse.rs` 里解析
`ESC[200~ … ESC[201~`；Windows 的事件源走 `ReadConsoleInput` 的 `KeyEventRecord`，
根本没有 paste 变体（`grep -c Paste src/event/sys/windows/parse.rs` → 0）。
于是粘贴到手的是**一串按键**，里面的每个 `\r` 都是 Enter，而 Enter 就是发送。

## 改了什么

按键不再立即生效：第一个键先进 `Tui::pending`，循环最多等 `PASTE_GAP`（10ms）
看下一个来不来。

- 又一个文本键 → 加进这一串，重新计时。
- 停顿 / `Paste` / 鼠标 / resize / 绑定键 → 这一串结束。

再由 `pasted()` 判断这串是什么：

- **一个键** = 键盘。单个 Enter 照旧发送，打字行为不变。
- **带 Enter 的一串** = 粘贴：整段作为文本插入，Enter 变 `'\n'`，**不发送**。
- **不带 Enter 的一串** = 快速打字：逐个回放给 `handle_key`，所以飞快敲 `/se`
  仍然会开补全弹窗——要拦的只是「发送」，不是「打字」。

10ms 低于键盘可能做到的间隔（两个键滚在一起也 > 20ms），而粘贴是终端一次写入，
中间没有间隙。

## 一个自己发现并修掉的设计错误

第一版把这一串收集在 `burst()` 这个 future 的局部 `Vec` 里。但 `next_event` 是
`run.rs` 里 `select!` 的一个分支，**网络先到就会被取消**——而按键成串最可能发生在
模型正在流式输出的时候，也就是网络最忙的时候。那样攒到一半的键会被整批丢掉。

所以 `pending` 放在 `self` 里：等待被取消，下一轮带着已有的键继续。事件读取本身
（`EventStream::next` 是 channel recv）是取消安全的，两头都不丢。

被这一串打断的那个事件（`Paste`/鼠标/resize）存进 `stashed`，下一轮开头取回——
延迟，不丢。

## 验证

- `only_text_joins_a_run`：哪些键进这一串（字符、Shift+字符、裸 Enter），哪些不进
  （Shift+Enter、Alt+字符、Ctrl+字母、Esc、方向键）。
- `a_key_run_with_a_break_in_it_is_a_paste`：单个 Enter 仍是发送；带 Enter 的一串
  变成 `"ab\nc"`；不带 Enter 的一串不算粘贴。
- 既有 `a_paste_is_inserted_not_submitted`：插入路径不发消息。
- `cargo test --workspace`（17+55+13+5+141+50）全过；fmt / comments-check /
  test-layout-check 全过；clippy 无新增警告。
- **未验证**：真终端里真按 Ctrl+V。试过用 ConPTY（`CreatePseudoConsole`）写个驱动器
  来真机复现，两次把 shell 卡死 120s，已删除，不留坑。要确认请自己粘一次。

如果实机仍然逐行发送，说明这台终端的按键间隔大于 10ms：把 `PASTE_GAP` 调大
（20–30ms 仍远低于人手间隔），逻辑不用动。

## 另一个坑

`cargo build` 期间换不掉 `target/debug/gongga.exe`——那正是用户正在用的 TUI 进程
握着它。用独立 `CARGO_TARGET_DIR` 构建，**不要**为了构建去杀用户的进程。
