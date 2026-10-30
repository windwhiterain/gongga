# 光标不再跟着 events 跑；窗口退了自己起的 server（WIP）

三件事，都是「用户报的现象 → 量出来的根因」。

## 1. 光标跟着 events 走

我先把 `Terminal::draw` 换成了手写的一帧（`get_frame` + `render::draw` + `flush` +
`swap_buffers`），因为它**每帧**都无条件 `show_cursor` + `MoveTo`。探针（一个记录 backend
调用的 `Backend` 实现）证实：五帧同位置，ratatui 发五次 `show` + 五次 `move`。在 Windows 上
`SetConsoleCursorPosition` 正是 IME 重挂组合窗/候选窗的时机，于是流式出帧时光标和候选框一起闪。

但只删不管会留下另一半问题：`flush` 最后写的那一格在 events 区，控制台光标就**留在那里**。
所以现在 `present()` 每帧只做一次锚定 —— `set_cursor_position(caret)`，位置由
`render::caret` 和渲染器共用同一个算法。真帧量：128 帧里光标不在输入行的帧数 0。

顺带：`raw.bin` 里仍有大量 `?25h/?25l`，但第 0 字节就是 `?25l`（那时还没画第一帧），
所以那是 ConPTY 自己发的，不是这个程序。

## 2. 输入法留下的字

IME 把组合串画在**控制台**上，ratatui 的 buffer 永远不知道那层墨，diff 只写「模型变了」的格，
所以比提交文字更长的组合串会留下尾巴，没有任何一帧会去覆盖它。现在每次输入变化后，把输入框
每行「文字结束处到右边界」擦掉（`render::input_ends`）——**擦的都是这一帧画成空白的列**，
所以它只可能抹掉控制台自己写的墨。

试过、并放弃的写法：把整行重写一遍（那次真帧里 119 帧有 31 帧把下面一行弄坏）。

## 3. 退出的窗口不收自己起的 server

原来 `cli::start` 是刻意 detach 的（`CREATE_NEW_PROCESS_GROUP` + `Stdio::null()`）：一个
一次性的命令几毫秒就退出，server 不能跟着死。但**窗口**不是一次性命令：它退出了，它起的
server 就没主人。现在退出时若 `link` 还在、地址仍是它起的那个、而且确实是它起的
（`starts_a_server` 那边记下的 flag），就发一条 `cancel` —— 用 SPEC 的 stop（把不能序列化的
活存下来），不是 kill。别人起的 server 不动；`connect_server` 换过地址也不动旧的那个。

## 4. SPEC 又改回来：`set_session` 写 sticky

`set_session` 后面那条 `plus set_sticky_cmd_argument --name session --value <session>` 恢复，
所以 `follows` / `settle_follow` 回来，但**只在 ack 之后**、且只跟 `set_session`（`del_session`
不再跟）。`scripts/tui_live_scenarios.py` 里我上一轮加的显式 `set_sticky_cmd_argument` 删掉了，
让场景走真正的路径。

## 验证

- `cargo test --workspace` 447 通过（cli 87、tui 83、protocol 26、server 230、bridge 21）。
- 真帧：`caret off the input row: 0`（128 帧）；`/set_session --session main` 之后 footer 出现
  `session=main`，下一条 prose 自动带上 `--session main`，答 `message accepted by main`。
  场景里补了 `alt q`，走的正是「告别 + 收掉自己起的 server」那条路。
- `test-layout-check` 0；我动过的文件 `comments-check` 干净。

## 踩到的坑，写在这以免下次再踩

`cargo build` 报「拒绝访问 (os error 5)」时二进制**没有被替换**，我因此拿旧 exe 验证了两轮，
得出过「sticky 没写」的错误结论。之后改用 `CARGO_TARGET_DIR=target/verify` 构建到别处，
再用 `--client-exe/--server-exe` 指给 harness —— 用户正在���的窗口占着 `target/debug` 时也能验。
