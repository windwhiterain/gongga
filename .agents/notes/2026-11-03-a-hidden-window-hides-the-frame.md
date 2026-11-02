# A hidden window hides the frame, not the lines

用户 2026-11-03 的裁定：**render 和数据是分离的**。`set_render_session` 决定一帧画什么，
不决定客户端被告诉什么。

## 报告的现象

`hierarchy` 会话在 16:13:25 之后"自己停下来了"：没有报错、没有输出、状态一直不变。
服务端侧完全正常（`turn end outcome=Done`，回答完整落在 `messages` seq 659），
窗口的 journal 也在 16:13:25 之后一条转录行都没有。

## 原因

`crates/cli/src/lib.rs` 的 `Client::drawn` 拿 `Event::session()` 与 `Shown` 比，
`handle_event` 用它决定要不要把事件交给 sink。被丢弃的事件因此既不画、也不进 journal——
而 TUI 落盘只有一条路：`Sink` → `Panes::line` → `Log::push` → `Cache::append`。

丢弃还是**半边**的，这一点让它隐藏了很久：`Event::session()` 对 `Cmd`/`CmdResult`
回答 `None`（它们的窗口来自 `args.session`），所以隐藏会话保留了操作者的命令行和
每条回答，只丢掉中间的对话——正是"看起来还在工作，但什么也没说"的形状。

`docs/notes/tui-log.md` 早就写了正确行为（"At width 0 … which is what a hidden session is
while it streams"，以及 `Log::set_hidden (gone)` 一节），`Panes::end_frame` 也早就按
"没人画的窗口不留 body"实现了。代码与这些描述冲突，而不是描述过时。

## 改法

删掉 `drawn`，`handle_event` 无条件把事件交给 sink。视图由 `Layout::shown` /
`Panes::rebuild` 决定，内存由 `end_frame`/`evict` 决定，两者都不需要事件在更上游被筛。

## 验证

`.dev/verify/07db597-cli-hidden-journal.json`：真帧驱动（`scripts/tui_live.py`）+
一份只留 `hierarchy` 的库副本（不调 LLM）。隐藏时读 `get_messages`，再显示——修好的 build
把整段转录画了出来（journal 7 条），撤掉修复的 build 只剩命令行与结果（2 条）。
同一 sha 再验引用 JSON，不重跑。

## 留下的洞（未修，等指示）

`crates/server/src/llm.rs:917` 用 `String::from_utf8_lossy` 解 SSE 分块：跨块切断的多字节
字符被永久写成 U+FFFD（库里 61 行、10 个会话），并且会随下一次请求发给模型。
