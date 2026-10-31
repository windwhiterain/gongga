# 会话自己指定模型，而 `--model` 会记住上一次（WIP）

User 的指令，2026-10-31，两句：「我是看到半天连接不上就重启了」之后的追问，以及
「把 model 参数弄成靠 sticky 保存设置，跟 session 一样，看 SPEC」。这条记的是**结论**：
默认模型与 sticky 各自负责什么，以及为什么不是当初那版「空白常量 + 强制 --model」。

## SPEC 怎么说

- 第 77 行：`set_session --session (sticky) <name> --model (sticky) <provider/model>`。
  `session` 早就是 sticky，`model` 不是 —— 这是这次改的全部理由。
- 第 104 行（evolve）与第 85 行（cancel）不涉及这里。

## 三个模型来源，顺序固定

1. `--model` 这次点了什么；
2. 这个会话自己的行里已经写着什么；
3. `config::DEFAULT_SESSION_MODEL`（常量，`deepseek-official/deepseek-flash`）。

**配置里的 `[model]` 不在其中**：那是服务端自己想事情用的模型。`Agent::route` 对一个
没有模型的会话直接报错，而不是拿服务端的模型顶上——顶上就是"没人给它选过模型，它却在花钱"。

## 为什么常量填了值，而不是留空强制 --model

留空那版（`e4c5672` 之前）在真机上立刻反噬：窗口里开一个新会话必须先敲模型名，用户看到的
就是 `set_session is missing required argument(s): model`。

- 常量是**代码里的**，不是 config 键：config 键会变成"服务端悄悄决定每个会话跑什么"，
  而会话的模型是调用方声明的东西。
- 常量有值 ⇒ 不带 `--model` 也能建会话，落在常量上；`--model` 覆写并写进该会话的行。
- 常量清空 ⇒ 回到"必填"，仍然是一行标准文案。这条分支留着，因为它是一个没写错的构建选择。

## sticky 让 `--model` 变成"模式"而不是"复述"

`ArgSpec` 新增 `sticky` 字段（`#[serde(default)]`）。客户端只从 sticky 填**必填**或
**声明 sticky 的**参数；`set_session --model` 是全目录里唯一声明者，并有测试钉住。
被填的时机是"服务端接受之后"（`settle_follow`），所以一次被拒绝的 `set_session` 既不
改 sticky `session` 也不改 sticky `model`。没点 model 的命令不动它。

`del_sticky --name model` 释放它，之后回落到常量。

## 验证

- L1：`cargo test --workspace` 10 组全绿。
- L2：把 `a.required || a.sticky` 改回只看 required，新增测试立刻红。
- L3（临时 home + 随机端口）：`set_session --model kimi-coding/k3` → `cli.toml` 记住 →
  下一条不点 model 的命令继承并把新会话写成该模型 → 点新模型覆盖 → `del_sticky` 后回落常量。
- `comments-check` 78 = 合并前基线。

## 还没做的

- **运行中的构建早于这次合并**（pid 28588，`payload/125d8efe1c1931de`，23:50）。sticky 要
  服务端目录声明它、客户端才会填，所以必须换一次手才生效。换手会 cancel 本回合，等用户点头。
- **ship pin 未发布**：内容仓库 `c7db7dc`（subagent skill 改成"sticky 而非必填"），
  `crates/server/ship.toml` 仍 pin 在 `b3ffb51`。`scripts/ship.sh` 要先 push 再写 sha，本轮无网络。
