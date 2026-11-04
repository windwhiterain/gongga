# thinking 的补全得看是哪个模型

用户报：TUI 里 `--thinking` 的补全没按模型实际支持的来，选项里也没有 `default`。

原来的两份事实各自都对，只是没有连起来：

- 服务端按 `Entry::reasoning_levels` **校验**每一个值（`reasoning_effort`），
  一个模型不接受的档当场拒绝，报错还列出它能接受的档。
- 补全却用的是 `providers::LEVELS` 这一张**全机通用的梯子**：`none minimal low
  medium high xhigh max`。于是 kimi 只认 `low/high/max`，Tab 仍然把 `xhigh`
  递到手上；只能靠提交后的报错纠回来。

而且 `none` 这种"我不在乎"的写法根本没有位置：不写 `--thinking` 的意思是"留着
上回那个"，没有词能表达"回到模型自己的默认"。

## 改了什么

1. **greeting 带模型表**：`ServerMessage::Hello.models: Vec<ModelInfo>`，
   每项是 `provider/model_name` 加 roster 为它写的 `reasoning_levels`（原样，
   可能为空）。这和 `--model` 的候选是同一份列表，所以 `--model` 的补全也从
   `Static(roster().models())` 改成 `CompletionSource::Models`——一份数据一个来源，
   否则同一帧 greeting 里两份列表可能说两件事（roster 会在两次 `await` 之间重读）。
2. **`CompletionSource::ModelLevels`**：补全"这一行跑的那个模型"接受的档。
   模型 = 行上的 `kind = "model"` 参数值，否则粘性 `model` 值——正是
   `set_session` 自己会填进去的那个，用的是同一个 `parse_args` 绑定器
   （和 `session_named` 一个写法）。所以 `set_sticky thinking <TAB>` 也跟着
   粘性模型走。
3. **`level_choices`**：模型没写档（roster 不描述它 / 它一个档都不发）时，
   补的是所有模型发过的档的并集。不写档的模型什么都收（`reasoning_effort`
   直接放行），所以并集里没有一个值会被拒；此时给空列表才是客户端在替 roster 立规矩。
4. **`THINKING_DEFAULT = "default"`**（protocol）：两个列表末尾都追加它。
   `set_session` 读到它就当作"没说"，走 `level_in_force(None)`——即 SPEC 的
   "highest in supported or default if unknown"，把之前存的档清掉。它住在 protocol
   里，因为补全端（cli）和读取端（server）必须拼同一个词。

## 验证（真进程，真 greeting）

`.dev/probe`（临时探针，不入库）+ `.dev/thinking-levels/run.sh`：假 roster 三个模型
（4 档 / 不写档 / 2 档），真 `gongga-server`，探针用 `protocol::connect` 握手，
把 greeting 里的 commands/models 塞进真的 `cli::Catalog`，再问整行补全：

```
/set_session --model alpha/one   --thinking  -> [none, low, high, max, default]
/set_session --model alpha/two   --thinking  -> [none, low, high, max, default]   (roster 没写档)
/set_session --model beta/three  --thinking  -> [low, high, default]
/set_session                     --thinking  -> 并集 + default
sticky model=alpha/one 时         --thinking  -> 跟 alpha/one 走
```

命令层同一台服务器上：`--thinking low` → `low`；`--thinking default` → `max`
（最高档，这就是"模型自己的默认"）；`--thinking ultra` → `max`（映射）；
`beta/three --thinking none` → 拒绝，报 "it accepts low, high"。

## 没动的

- SPEC 没改。`--thinking default` 是新增的一个**值**，SPEC 那行
  `[--thinking <thinking>](default: highest in supported ...)` 说的是缺省行为；
  要不要把 `default` 这个词写进 SPEC，请用户定。
- `level_in_force` / `LEVELS` / `rank` 一字未动：`highest in supported` 那一半
  仍然是它说了算，`default` 只是复用它的答案。
