# 插话把等待中的 bash 交给后台

Status: done, verified on Windows (Git Bash), 2026-10-24.

## 症状

「A 创建完 B 之后，我跟 A 说话，它半天才反应。」看起来像多 session 不能并行，
其实并行是好的——当时 test1 和 peer 两个 session 在同一分钟内交错跑了十几条
bash 工具调用。

真正卡住的是 A 自己：`tool_calls #447` 从 23:59:30 到 00:04:30，耗时整 300 秒，
output 写着 `still running after 300s`。这五分钟里 A 的 step 一步都没走完，
而 A 的插话只能在 step 边界被 `deliver_inbox` 读到，于是消息一直躺在
`pending_tasks(kind=inject)`。

## 为什么一个 step 会这么长

一个 step = 一次模型请求 + 这轮所有工具调用。inbox 只在边界投递（模型和
 provider 不允许在 function_call 与其 output 之间插入用户行），所以工具跑多久，
插话就等多久；bash 那边唯一的期限是 `timeout_ms`，那一次被设成了 300000。

## 改了什么

给「deadline 到了」加一个并列的触发源：「有人说话了」。

- `SessionInner::inbox`：一个计数，不是队列——只关心「我这次等待期间有没有新消息」。
- `SessionHandle::note_message()` / `inbox_signal()`：`send_message` 在 busy 时 bump；
  工具调用在开始等待前取一份快照。
- `ToolContext::inbox`：`Agent::run_tools` 每个 call 取一份新快照，所以一条消息只会叫醒
  一次等待，不会让之后每个 call 都被丢进后台。
- `BashTool::run` 的 select 循环里 `INBOX_POLL`（250ms）检查该信号，命中则 break 成
  `WaitingMessage`，走 `leave_running`。
- `leave_running`：把原来 deadline 分支里「换壳 + 起一个 task 收尾 + 文案」抽出来共用。
  命令不会被杀，继续在旧 shell 里跑，输出照旧作为 inject 回来。

`WaitingMessage` 有自己的类型，原因和 `Cancelled` 一样：一个要杀 shell，一个不许杀，
靠字符串区分迟早出事。

只对 bash 做：`read_image` 之类没有等待可言。

## 验证

- `tools::tests::a_message_arriving_mid_command_hands_it_to_the_background`：真 Git Bash 跑
  `sleep 20`，300ms 后 bump 信号，断言这次调用在 10 秒内返回并说明原因，且名字还
  能继续用（下一次调用换了新快照）。
- `session::tests::an_inbox_signal_sees_only_what_arrived_after_it_was_taken`。
- `cargo test --workspace` 142 passed，clippy / fmt / comments-check / test-layout-check 全过。

用 `--config` 之外的 target 目录跑测试会让 `main.rs` 的 cli_path 测试失败：它假定
`target/debug/gongga.exe` 是兄弟文件。先 `cargo build --workspace` 再测即可。

## 接手的另外两处（同一症状上的 2、3 号问题，也已改完）

### 1. `net.rs`：`dispatch` 不再在 select 分支里内联 await

原来那个 select 同时负责读 socket 和转发事件，`dispatch(...).await` 写在
`incoming` 分支里，于是一条命令跑多久，这条连接就有多久既不读新命令、也不转发
事件（包括正在跑的 turn 的流式输出）。

现在每条连接额外起一个 `run_commands` task：读循环把命令塞进队列就回去 select，
队列那头**一次一条、按到达顺序**执行。

为什么不是一个命令一个 task 让它彻底并行：同一条连接的两个命令是允许互相依赖的
（`set_session` 接着给它刚配好的 session 发消息），而 `dispatch` 内部没有任何东西
替它们排序。并行会变成改语义，不是改速度。Client 本来就是按 `id` 匹配结果的。

### 2. `commands.rs`：`set_session` 取 cwd 不再抢 shell 锁

`set_session` 最后 `session.cwd().await`，而它要去拿目标 session 的 **default shell
的互斥锁**——那把锁被一条正在跑的命令握着。B 在跑长命令时，A 里的
`$GONGGA_CLI set_session --session B` 就会一直挂到那条命令结束。这正是「A 创建完
B 之后半天才有反应」的最后一环（在此之前 bash 等待那一家五分钟才是主因）。

`ShellSet` 里那个 `cwd` 字段是 spawn 根，不是现场值（`cd` 之后就不对了），所以不
能用它顶替。做法：`BashShell` 把自己的 cwd 镜像到一个共享 cell（`pending` / `jobs`
早就这么干），`ShellSet` 持有 `default` 那个 shell 的 cell，`SessionHandle::cwd()`
改成同步读它。写两侧都走同一个 `set_cwd`，所以两份不会对不上；`ShellSet::get` 在
换壳时把 cell 也换掉，避免退役的那个还在往里写。

`agent.rs` 里每次工具调用构造 `ToolContext` 时也会问 cwd，一并变成同步的。

## 验证（两处合并后）

- `session::tests::the_session_cwd_does_not_wait_for_a_running_command`：真 Git Bash，
  像长命令那样握住 default shell 的锁，断言读 cwd 立刻返回。这是这次改动的回归测试。
- `session::tests::a_cd_in_the_default_shell_reaches_the_session_cwd`：没人写的镜像会
  永远回答 session 的起始目录，只有真 cd 一次才证明它在动。
- `net.rs` 没有单测：要端到端必须把整个 server（Dispatch、Agent、LLM roster）拼起来，
  不划算。改用真实连通性验证代替：另建一个 home（端口 9711）起 server，用
  `gongga.exe set_session/get_messages` 走完整往返，命令、结果 id、cwd 回显都正常，
  连接/断开日志也对得上。验证完已删掉那个临时 home 和它的进程。
- `cargo test --workspace` 143 passed，clippy / fmt / comments-check /
  test-layout-check 全过。剩下的那条 warning（`long_output_is_clipped_in_the_summary_prompt`
  never used）来自另一个 session 正在改的 `agent.rs`，不是这次的。

comments-check 的 preamble 只有 120 字符：长解释一律挪去 `docs/notes/`，代码里只留
一行摘要加一个指针。这次被拦了 4 次才过。
