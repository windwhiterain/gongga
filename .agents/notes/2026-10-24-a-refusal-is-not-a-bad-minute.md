# 2026-10-24 — 拒绝不是「坏的一分钟」：400 不再无限重试

## 现象（user 报告）

「test1 failed to request llm service, likey context explision, fix it」

## 取证：test1 的上下文没有爆

- `sessions` 行：`context_tokens=109293`（后来一次请求是 81360），roster 里
  `deepseek-flash` 的 `context = 1000000`，`compress_threshold` 默认 70% →
  预算 700000。**109k 是窗口的 11%**，根本没到通知线（`notice` 行一条都没有）。
- 把 ~/.gongga 复制成 `.dev/repro-home`、用同一个二进制在 9701 起一份服务端，
  对 test1 发一条消息：请求 312443 字符，`headers status=200 OK`，2.4s 结束，
  outcome=Done。
- 在**线上**服务端对 test1 发一条 ping：18:57:44 记下 user 行，18:57 回 `ok`，
  状态 idle。
- 反转取证：一次失败的请求会先写入 user 行、再把 status 置 `error`；test1 里
  18:34 之后既没有新的 user 行，status 也是 idle。**所以那天没有任何回合跑过**
  （18:51:42 那次 `updated_at` 变化是 `set_session`，`apply_tuning` 也写这一列）。

结论：不是 context explosion，而且现在请求是通的。user 看到的那行只可能是
`Event::Log`——它是广播、不落库，所以事后查不到；这也解释了「什么都没留下」。

## 真 bug：拒绝被当成坏的一分钟，重试到永远

`llm.rs` 的 `retryable()` 只看 `produced`（模型有没有开口）。`max_retries = 0`
是「无上限」，于是**确定的拒绝**（400 请求本身不对）也会 2s/4s/8s/16s/30s…
无限重试：

- 回合永远不结束（`status` 一直是 running），
- 永远不报错，`Event::Log` 只有一句 "model request failed or came back empty;
  retry N until cancelled"，
- 会话看起来「卡住/失败」，而库、transcript 里一点痕迹都没有——正是 user 描述的
  「failed to request llm service」，也正是上下文真的爆了时的形态。

## 改法

1. `LlmEvent::Failed` 带 `retryable`：`status_retryable()`（4xx 里只有 408/429
   算坏的一分钟，5xx 算）＋ `error_retryable()`（流内 `error` 事件按 `type`/
   `code` 判，未见过的一律仍可重试）。
2. `stream()` 遇到 `retryable == false` 立即把失败交给调用方 → 回合 Failed、
   status error、TUI 红字，30ms 级结束。
3. `with_size_hint()`：拒绝理由是「太长」时补上 provider 不会说的那句——用
   `.agents/skills/compact/transcript.py collapse` 折掉旧的一段再发。

## 验证

- 新测试 3 项（`a_refused_request_is_not_retried`、`a_streamed_error_decides_by_its_kind`、
  `a_size_refusal_says_how_to_fold`）；`cargo test --workspace` 全绿。
- **端到端**：`.dev/fake400.py`（永远 400 + "maximum context length"）＋
  `.dev/fake-roster.toml`（base_url 指过去）＋ 复制的 home，在 9701 起服务端：
  - 改前（把 `status_retryable` 变异成 `true`）：`retrying attempt=2..5 delay=2s/4s/8s/16s`，
    永不结束；
  - 改后：`failed to model request: HTTP 400 …maximum context length…` +
    「The transcript no longer fits this model. Fold it, then send again:
    `.agents/skills/compact/transcript.py collapse --session <name> --upto <seq>`」，
    outcome=Failed，用时 35ms。

## 追加：为什么「第一次」的失败查不到 —— 它是被设计丢掉的

user 追问「第一次请求为什么失败」。逐个翻过之后，答案是：

| 可能的痕迹 | 结果 |
|---|---|
| `messages` | 失败请求不写任何行（连 user 行都在 `route()` 之后才写） |
| `events` | 只有 `tool_result` 一种 |
| `pending_tasks` | 只剩 help-test1 那条 |
| `tool_calls` | test1 有两条 `finished_at IS NULL`，但都是 16:49/16:56 被 `cargo clean` 杀掉的旧回合；18:20–18:34 的回合带着它们照样 Done |
| `outputs/` | 18:4x 没有落盘 |
| 服务端 stdout | 在 VS Code 终端里，ConPTY 缓冲区只有 26 行，18:4x 早被冲掉 |
| mihomo（代理） | 进程 10-23 19:44 起就没重启过，不是代理掉线 |

真正的原因在代码里：`stream()` 收到 `LlmEvent::Failed` 后 **`continue`**（注释写着
「如果后面还有重试，这次的错误就不是答案」），而 `Retrying` 只带 `attempt/max/delay`。
于是 `agent` 只能打出「model request failed or came back empty」——**第一次失败的原文
在任何地方都不存在**。再叠加 `Event::Log` 只广播不落库，事后连这句都没有。

### 所以这一轮又补了两处

1. `LlmEvent::Retrying` 带 `reason`；`retry_reason()` 在没有 provider 原文时也一定会给出
   一句（"completed the response with no content in it" / "the stream ended before the
   response completed" / "the stream ended with nothing in it"）。`retry_warning()` 打
   `... failed (HTTP 500 ...); retry 2 until cancelled in 2s`，tracing 同步带 `reason=`。
2. `Agent::error()` 除广播外，**把这句话写进 transcript 的 `notice` 行**——`notice`
   可读（`get_messages`）、但 `history_since` 会跳过，不进 prompt。
   有现成测试 `a_notice_is_readable_but_never_sent_back_to_the_model` 守着这条不变量。

### 验证

- 假 provider「第一次 500、第二次 200」：服务端日志
  `retrying attempt=1 delay_ms=1000 reason=HTTP 500 Internal Server Error: upstream connect
  error or disconnect/reset before headers`，turn 随后 Done——即第一次失败的原因现在留得下。
- 假 provider 永远 400：`get_messages` 读回
  `seq 7015 notice "test1: failed to model request: HTTP 400 ... maximum context length ...
  Fold it, then send again: .agents/skills/compact/transcript.py collapse ..."`，
  且下一条请求照常成功（notice 不进 prompt 已被实测）。
- 新测试：`a_retry_names_the_previous_attempt`、`a_retry_warning_names_why`。服务端 148 项全绿。

## 没做的事 / 注意

- **线上服务端是旧二进制**（16:58 启动，比 seq-only 合并还早）���本修要
  `evolve` 一次才生效。
- 线上 test1 的 transcript 多了两行（18:57 的 ping 与 `ok`），无害，没删。
- 复制 home 起第二份服务端时，**必须先把 `pending_tasks` 清掉**：否则它会把
  正在跑的另一个会话的回合重放一遍（第一次复现就撞上了，两个自己同时动仓库）。
