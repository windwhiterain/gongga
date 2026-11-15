# 一个 28 字节的假 PNG 让三个会话永远发不出请求

2026-11-14 07:33 用户说「`@monitor/providers` 卡死了」。它没卡在工具上，它卡在**重试**上：
同一份 182737 字节的请求，每 30 秒发一次，永远失败，永远重发。

## 链条

1. `escape/uvpkg` 为了试 `image` skill，造了一个测试图片：
   `.worktrees/uvpkg/.dev/img/a.png` = 28 字节（`\x89PNG\r\n\x1a\n` + `"0"*20`）。不是 PNG。
2. `image.rs::media_type` **先看扩展名**（`a.png` → `image/png`），没有扩展名才看前 8 个魔术字节。
   没有任何一步真的解码图片。于是它被当成图片存了下来（`pictures` id 121，本 home 的副本
   `<home>/pictures/121`）。
3. `agent.rs::attach_references` 扫**工具输出**里的 `@image(...)`，扫到就附上。于是毒扩散：
   `escape/uvpkg` 造了它，`escape` 在读 transcript 时把它**打印**了出来，
   `@monitor/providers` 在调查时又把它**打印**了一遍。三个 transcript 各自带上了 `copy: 121`。
4. 此后每一条带这行的请求都失败：
   - deepseek（`escape`）：`HTTP 400 Bad Request: input[509].output[1]: You have uploaded an
     unsupported image`——非流式拒绝，turn 结束，会话 `error`。这才是 monitor 被叫醒的原因。
   - qwen-token-plan（`@monitor/providers`）：HTTP 200，流开了，然后
     `event: response.failed`，`response.error = {"code":"InvalidParameter",
     "message":"<400> InternalError.Algo.InvalidParameter: The image format is illegal and
     cannot be opened"}`。**harness 之外**用一条裸 POST 复现过（`qwen3.8-flash`，一个 28 字节的
     `input_image` data URL），所以不是 harness 构造错了请求。
5. `llm.rs::error_retryable` 读 `error.type` 再读 `error.code`；`InvalidParameter` 不在名单里，
   于是判成「可以重试」。而这份 home 写着 `max_retries = 0`，`config.rs` 的注释说那是
   **没有上限**。`server.log` 里是 `attempt=31 delay_ms=30000 ... reason=<400> ...`：
   turn 永不结束，排给它的 `inject` 永远发不出去，会话就永远停在 `running`。

## 两个缺陷

| 缺陷 | 位置 | 形状 |
| --- | --- | --- |
| D1 没有验证过的图片被当成图片 | `image.rs::media_type` + `agent.rs::attach_references` | 按扩展名/8 字节判类型，不要求它真是那个格式 |
| D2 200 流里的拒绝被无限重试 | `llm.rs::error_retryable` | 非流式那条路有 `status_retryable`（408/429/5xx 重试，其它 4xx 不重试），流式这条路**没有状态码可读**，落进「不认识的类型就重试」 |

D2 不是「加个上限就好」：`max_retries = 0` 是用户的设置，改成 3 次就是把「拒绝」当「坏的一分钟」。
该改的是**分类**——provider 自己说了这是客户端的错，那就不是再试一次能好的事。

## 顺手记下的两件事

- **monitor 的这次派活是误报。** provider 一点问题都没有，是 harness 给它塞了一个坏文件。
  `providers` skill 的第一节写的就是这个判据：外面也失败 → 不是 harness；外面成功而 harness 失败
  → 才是真 bug。这里外面也失败，但失败的**原因**是 harness 塞进去的字节，所以结论反过来。
- **毒无法靠删副本解掉。** `pictures::read_missing` 在副本没了之后会退回去读**原始路径**，
  所以只删 `<home>/pictures/121` 会把同一份垃圾重新 inline 进去；要同时清掉副本、
  `uploads` 行和那个 `.dev/img/a.png`，那一行才会退化成一句 note。这是数据手术，要用户点头。

修复派给了 `monitor/image-refusal`，worktree `.worktrees/image-refusal`，分支 `fix/image-refusal`。
测量留在 `.dev/verify/707bb371-image-refusal.json`。
