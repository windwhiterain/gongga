# 补全弹的是"别的模型的档"，而且看不出在补哪个参数

用户报两件事，都在这一个弹窗里：`--cache_tier "5m"` 被服务端拒了（"is not a tier this
model declares"），可弹窗里明明给了它；弹窗也没写正在补哪个参数。

## 一：tier 的空表不是"什么都收"

`tier_choices` 是照 `level_choices` 抄的，包括那条"模型没写就退到所有模型的并集"。
但两者对"空"的含义**相反**：

- `reasoning_levels` 空 = roster 没意见，这个模型收什么都行（`reasoning_effort` 直接放行）
  → 并集里的值不会被拒，所以退并集是诚实的。
- `cache_ttl_tiers` 空 = **没有一个档可选**（`Entry` 的 doc 原话："empty means none
  selectable"），服务端对任何档都报 "not a tier this model declares: "（列表为空）。
  → 退并集等于把一个必然被拒的值递到手上，拒信还晚了一条命令。

改成"就是这个模型声明的那几个"，模型没声明 / 行上的模型在表里找不到 → 空列表。
用户的 longcat 三个模型都只写 provider 级 `cache_ttl_secs`，没有 `cache_ttl_tiers`，
所以现在弹窗是空的，而不是递给他别人的 `5m`。

## 二：弹窗标题写参数名

`Completer::complete` 现在返回 `Offered { subject, candidates }`，不再只返回候选：

- `subject` 是"这些候选在补哪个参数"，`--cache_tier` / `--thinking`；命令名、旗标名、
  `@` 引用时是 `None`。
- 它跟候选一起返回，而不是另开一个问题：另开的话要么解析两遍行，要么标签和列表可能不一致。
- TUI 的 `Completion` 带上它，弹窗标题画成 `┌ --thinking ───┐`；没有 subject 时仍是
  `┌ completions ───┐`。

## 验证

探针（真服务端、真 `cli::Client`，假 roster：alpha 声明 `5m`/`1h`，beta 不声明任何档）：

```
--model alpha/one  --cache_tier   [--cache_tier] -> ["5m", "1h"]
--model beta/three --cache_tier   [--cache_tier] -> []              ← 用户的那种模型
--model alpha/no-such --cache_tier [--cache_tier] -> []
--name demo (alpha/one) --cache_tier -> ["5m", "1h"]                ← 会话的模型
--name other (beta/three) --cache_tier -> []
--model alpha/one --thinking      [--thinking]  -> ["low", "high", "default"]
--cach                            [-] -> ["--cache_tier", "--cache_keepalive"]
```

标题用 `scripts/tui_live.py` 在 ConPTY 里看真窗口（`keys /set_session --thinking` +
`key tab`，Tab 应用旗标后会重问值��：

```
┌ --thinking ──────────────────────────────────────────────┐
│ none                    level                            │
```

顺带记一笔：这个 harness 的 `seed_ship_checkout` 克隆 `ship/` 时，如果内容仓的 HEAD 在
`local` 分支上，克隆出来的仓库**只有 `origin/main`、没有本地 `main`**，服务端一遍历就说
"no commit on main speaks protocol 1"。先 `git -C <home>/ship branch main origin/main`
再 `--keep-home` 跑就对了。这不是产品缺陷，但下次跑 harness 撞上它会绕半天。
