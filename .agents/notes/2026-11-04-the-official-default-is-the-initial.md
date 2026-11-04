# 不写 --thinking 就是厂商的默认，不是最硬那一档

用户要求：`thinking` 参数没给的时候，填**官方 default 对应的 effort**。

原来 `Entry::level_in_force` 的顺序是"存着的档 → 发布的最高档 → `reasoning_default`"，
也就是没写 `--thinking` 时给的是**最高档**。这在 deepseek-flash 上是错的：它发布
`none/low/high/max`，官方默认 `high`，于是 harness 自作主张把它顶到 `max`。
SPEC 那行现在是 `[--thinking <thinking>](initial: default)`——初始值就是"默认"这个词，
不是"最高"。（`docs/notes/server-commands.md` 早就这么写了，是代码跟文档对不上。）

## 改了什么

`level_in_force`：存着的档（模型仍发布它）→ **roster 记录的 `reasoning_default`** →
发布的最高档。

- 第二步过 `reasoning_mapping`，和别的档一样：roster 用调用方的写法（`medium`）写默认、
  却发布另一个写法（`high`）时，答案仍是请求能带的那一个。
- 第三步只在 roster 没写默认时兜底（`["on","off"]` 这种开关模型，或者根本没写
  `reasoning_default` 的）。
- `--thinking default` 走的也是这条路（`level_in_force(None)`），所以这个词现在的
  含义和"什么都不写"完全一致——补全里那个 `default` 选项终于名实相符。

## 验证（真服务端 + 真 `gongga-cli`，假 roster 三种情形）

```
one    levels none/low/high/max, default high        → thinking_effort='high'    (以前是 max)
two    levels low/high, default medium, medium→high  → thinking_effort='high'    (映射过的默认)
three  levels low/high/max, 没有 default             → thinking_effort='max'     (兜底最高档)
one --thinking default                               → thinking_effort='high'
one --thinking max，再单独发一次 set_session          → thinking_effort='max'     (存着的档仍然优先)
```
