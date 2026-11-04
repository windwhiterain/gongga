# 改了 providers.toml，窗口那边的补全也得知道

用户报：roster 改了，服务端好像自己就认了，TUI 不行。

## 为什么服务端自动、TUI 不自动

服务端那份 roster 是**每次用到才去问文件**：`providers::roster()` 拿 `providers.toml`
和 `keys.toml` 的 `(size, mtime)` 当戳（`Stamp`），戳变了就重读一遍，并给这次读编号
（`Catalog::generation`，每次**真的重读**才 +1）。所以 `set_session --model` 立刻就能
用上新模型。

客户端不一样：`--model` / `--thinking` 的候选是连接时那一帧 greeting 带来的表
（`ServerMessage::Hello.models`），一个窗口开着不动就永远停在那一刻。补全是客户端本地
算的（每敲一个键算一次，不发网络往返），所以没有任何路径会让它重读。

## 改了什么

1. **`Event::Models { models }`**（protocol）：整张表，不是差量。`providers.toml`
   是人手改的一个文件，收差量的一方还得记住"我不在的时候错过了什么"——线上没有这个状态。
   `Event::session()` 答 `None`，`Event::name()` 答 `"models"`。
2. **`providers::model_table()`**：`models()` 加上每个模型公布的档，一次转换两处共用
   （greeting 和事件），否则同一个客户端可能被 greeting 和刷新各喂一张表。
3. **重发点**：服务端本来就有一个 2 秒的 tick（催未投递的消息、扫失败的会话）。
   它顺手比一次 `roster().generation()`，变了就 `bus.publish(Event::Models { .. })`。
   租约、租约里的 generation 都还是原来那套：没有新状态。
4. **客户端**：`Event::Models` → `catalog.set_models(models)`。不在窗口里写一行日志——
   没人提问，也没出错。已经打开的补全弹窗不在这里重算，下一次按键就会重建（和别的值
   同一套规则）。

## 坏掉的编辑不发

`providers.toml` 语法错的时候，服务端服务的是**最后一次读成功的那份**，而且那份的
`generation` 还是旧的 —— 所以 tick 看不到变化，不发事件。客户端保持手里的表，等文件
改回来。静默在这里的含义是"没变"，不是"roster 空了"。

## 验证（真服务器、真连接、不重连）

`.dev/probe`（临时探针）：`protocol::connect` 握手，把每一帧都喂给**真的**
`cli::Client::handle_bytes`，再问 `client.completer()` 的补全。`.dev/models-refresh/run.sh`
起真 `gongga-server`，探针连着不动，脚本在运行中往 roster 里加一个 `gamma/four`
（`reasoning_levels = ["minimal", "high"]`），6 秒后再把文件写坏：

```
BEFORE "--model ga"                      -> []
BEFORE "--model gamma/four --thinking "  -> [none, low, high, max, default]   (模型还不存在,所以是并集)
EVENT models
AFTER  "--model ga"                      -> ["gamma/four"]
AFTER  "--model gamma/four --thinking "  -> ["minimal", "high", "default"]
(坏掉之后没有再收到 models 事件；服务端日志 generation=2 serving_cached=true)
```

一次连接，没有重连，补全从旧表换到新表，坏编辑期间保持旧表。
