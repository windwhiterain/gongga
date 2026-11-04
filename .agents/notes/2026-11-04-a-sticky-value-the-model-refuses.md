# 粘性值配不上模型时，不是命令的错

用户报：`model = longcat/LongCat-2.5` 建会话报错 `` `low` is not a level this model takes; it accepts on, off ``。

## 它不是用户这次打的值

`~/.gongga/cli.toml` 里粘着 `thinking = low`。`set_session` 没写 `--thinking`，客户端
把粘性值填进去，服务端按 roster 校验：longcat 的三个模型都写的是
`reasoning_levels = ["on", "off"]`——一个开关，没有低中高档（roster 的 `reasoning_note`
也是这么说的：Chat/Messages 上只有 `thinking{type}`，档位只在 Responses 面才有）。

于是"建个会话"这条命令失败在一个调用者根本没写的参数上。而且它不会自己好：
只要粘性还是 `low`，任何落在 longcat 上的 `set_session` 都失败。

## 改了什么

粘性值是**记住的默认**，不是这次说的话。默认配不上就不应用——服务端对"存着的档"
本来就是这么办的（`Entry::level_in_force`：存着的档模型不发布就忽略，不报错）。客户端
现在也问得出这个问题了，因为它手里有 greeting 带来的模型表：

- `Catalog::refused_by_model(line, args)`：按补全同一套解析算一遍，报出"这个模型不收的
  值"。它只量 `CompletionSource::ModelLevels` —— 那个来源的候选**就是**模型接受的集合；
  别的来源（`ctx_limit` 那十个百分比、`remote` 的名字表）说的是"值得提供"，不是"允许"，
  拿它当门槛会把 `50` 这种服务端明明收的值也丢掉。
- 客户端只丢**自己填进去**的那些：动手前记下命令行本来写了哪些参数名，填完再问一遍，
  命中的去掉并说一句 `sticky thinking = low is not one this model takes; left to the model's own`。
  调用者**亲手写的** `--thinking low` 照旧发出去、照旧被服务端拒绝——那是一句陈述，
  值得一个拒绝。

## 验证（真服务端 + 真 `gongga-cli`，假 roster 只有 longcat 一家）

粘性文件照抄用户的：model/thinking=low/ctx_limit=50/cwd=.

```
set_session demo                     → ok, thinking_effort=null, stderr 一句说明
set_session demo2 --thinking low     → 仍然拒绝：it accepts on, off
set_session demo3 --thinking off     → ok, thinking_effort="off"
```

第一条的 echo 里没有 `thinking` 键，说明它真的没被发出去。
