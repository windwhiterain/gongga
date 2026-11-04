# `--thinking` 跟着会话的模型走，不跟粘性的

`2026-11-04-thinking-completes-what-the-model-takes.md` 里那条链是"行上的 `--model`，
否则**粘性 `model` 值**"。合进 main 时这条前提没了：同一天另一个改动把 `set_session`
的 `--model` 从 `(sticky)` 改成 `(initial: none)`，**模型不再有粘性条目**——所以那一级
永远为空，`--thinking` 的补全在没有 `--model` 的行上会掉到"所有模型档的并集"。

按命令自己解析模型的三步来：`--model` 写着的 → 这个会话已有的模型 → 服务端自己的默认。

- 中间那步读 `SessionInfo::model`（greeting 里本来就有），用的是 `session_named` 同一个
  绑定器 —— 和 `place` 找"这一行在哪个目录"是同一份逻辑，只是参数换成了 `&[CommandSpec]`
  和那一个 `message_session`，所以两个调用点不再各写一遍。
- 最后那步读 `ServerInfo.model`（服务端的 `[model]` 配置），比并集诚实：新建会话确实会
  落到这个模型上。
- 只有三样都问不出来、或者问出来的模型 roster 没写档，才退到并集。

## 验证

假 roster（alpha/one = `["minimal","low"]`，alpha/two = 不写档，beta/three =
`["low","high"]`），服务端默认 `alpha/one`，真服务端 + 真 `cli::Client`：

```
--model alpha/one --thinking   -> [minimal, low, default]      行上写的模型
--name demo      --thinking    -> [minimal, low, default]      demo 会话在 alpha/one 上
--name other     --thinking    -> [low, high, default]         other 会话在 beta/three 上
--name brand-new --thinking    -> [minimal, low, default]      会话还不存在 → 服务端默认
--model alpha/two --thinking   -> [minimal, low, high, default] roster 没写档 → 并集
```
