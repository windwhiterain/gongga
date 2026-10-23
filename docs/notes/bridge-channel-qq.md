# bridge/channel/qq.rs

## qq (module doc)

为什么先做这个平台：它是国内唯一一个不需要 SDK、能把事件直接交给普通 Rust 程序的平台。
另外两个认真考虑过的选项（飞书长连接、钉钉 Stream）都把事件包在 protobuf 里，SDK 也只出
Go/Python/Node/Java；QQ 的 gateway 是 WebSocket 上的 JSON，`tokio-tungstenite` 本来就会说。

分成两半，因为它们需要的东西不一样：

* `Inbox` 持有 gateway socket：identify、heartbeat、重连。
* `Outbox` 只是 HTTP API，所以回复可以从 bridge 主循环里发出，不用碰 socket。

塑造了代码的平台限制（官方文档）：

* 被动回复只对 5 分钟内的消息有效，一条消息最多回 5 次（`msg_seq` 1~5）。
  所以「先回一句收到、跑完再回结果」要按 seq 计数，超时就必须改成主动消息。
* 主动消息有频控（认证主体 60/min，未认证 30/min，单个群 20/min，每群每天 1000 条）。
  一个人用远够。
* 群消息不支持流式，所以回复必须是拼好的整段。

## INTENTS

`1 << 25` 同时覆盖 `GROUP_AT_MESSAGE_CREATE` 和 `C2C_MESSAGE_CREATE`；guild 那组 intent
故意不申请 -- 这个 bridge 在频道里没有业务。

## Token

`access_token` 名义上有效期两小时，实际 TTL 取响应里的 `expires_in`（缺失时按 7200 秒），
并提前一分钟作废。惰性获取，两半共享同一份缓存，所以连 socket 不会为同一个值再花一次往返。

## Token::get

缓存读取单独包一层作用域，guard 在下面的 await 之前就丢掉：std mutex 跨 await 持有，
正是 bridge 把自己锁死的方式。

提前一分钟过期：一个在请求中途过期的 token，会变成一个事后没人能查明白的 401。

## Inbox::last_s

最后见到的 sequence number。随心跳一起发出去，平台据此区分「活着但安静」的连接和已经死掉的连接。

## Inbox::connect

连上、说 hello、identify。这里的错误由 [`recv`](Self::recv) 重试，所以网络抖动不会弄死
bridge。

## Outbox::send

先试被动回复：它把回答挂在问题下面，这才像一个聊天。超过五分钟（或者第六条回复）平台就会
拒绝，这时一条普通消息是长任务唯一的送达方式。

## strip_mentions

如果不去掉，@ 会变成 prompt 的一部分：agent 会看到 "<@!1234> 跑一下测试"，并且很可能
判断这个 id 是有意义的。
