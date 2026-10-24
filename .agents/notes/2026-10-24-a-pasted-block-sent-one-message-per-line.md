# 粘贴的多行块按行发了出去

Status: done（逻辑与单测验证；真终端需用户复粘确认），2026-10-24.

## 症状

用户「点了下左键」，随即一连串消息被发了出去。

## 证据（不是猜测）

transcript 里同一秒出现了 5 条 user 行，每条正好是所粘文本的**一行**：

```
seq 1415 19:31:51 'mode:'
seq 1416 19:31:51 '- all'
seq 1417 19:31:51 '- line: fold each non-output message block ...'
seq 1418 19:31:51 "- one: each session's all message fold ..."
seq 1419 19:31:51 'mode:'      ← 又粘一次
```

所以左键只是把「剪贴板 → 输入框」这一步做完了；发送是每个换行干的。
`handle_mouse` 只处理滚轮，左键在代码里什么都不做——不是鼠标的锅。

## 根因

上一轮的粘贴防线只认「一串里含 Enter」。但终端在换行处会停顿，实际的交付形状是：

```
[这一行的字符]      ← 一个 run，超时后按「打字」回放
[Enter]             ← 单独一个 run = 一个按键 = 发送
[下一行的字符]      ← 又一个 run
```

把 `PASTE_GAP` 从 10ms 调大**不安全**：它同时决定「人打字快时算不算粘贴」。

## 改法：按「谁能产生这个 run」判断，而不是按间隔

- `MACHINE_RUN` = 3：10ms 内到达 ≥3 个文本键，键盘打不出来。
- `GLUE_GAP` = 150ms：在这样一个 run 之后 150ms 内到达的**裸 Enter**，属于同一次粘贴，
  变成输入框里的 `'\n'`，不发送。
- `glue_break()` 是纯函数（键 + 上一个 run 的字符数），所以判据可以单测。
- 粘完之后 `settled` 继续更新，所以「行/换行/行/换行…」这条链每一环都能接上。

## 残余风险（诚实记录）

人手滚三个键、再在 150ms 内敲 Enter，会被当成粘贴换行。代价是多按一次 Enter，
方向是安全的——另一头是「一个粘贴变成 N 条消息」。

另外：如果终端把粘贴拆成 1~2 键的 run 并带停顿，仍然分辨不出来，因为终端不送标记。
真正的根治要 bracketed paste，而 crossterm 0.29 在 Windows 上不解析它（见本文档上一节）。

## 验证

- `a_break_after_a_machine_run_is_part_of_the_paste`：≥3 字符的 run 之后的裸 Enter 粘住；
  无 run、run 太短、带修饰键、或 run 里不止一个键时都不粘。
- `cargo test --workspace` 143+62+60+20+13+5 全过；fmt / comments-check / test-layout-check 全过。
- **待你确认**：重启 TUI 后重新粘一次多行文本，应当整段落进输入框、一条都不发。
