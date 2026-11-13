# Sticky filled nothing but `session`, and only a no-default slot may be filled

用户报「sticky 功能好像不怎么生效」。`~/.gongga-client/client.toml` 里存着六条：
`session=description`、`model=deepseek-official/deepseek-flash`、`cwd=c:/resource/`、
`remote=lichen-vm`、`mode=work`、`cache_keepalive=false`（旧名）。但真正会生效的只有
`session`；其余连同 `thinking` / `ctx_limit` 早已填不进任何命令。

第一版把这些值统统填进 `--config` 的叶子里，用户随即纠正：**叶子大多有默认值，sticky 只该
补没有默认值的**。这一版就按这条重做。

## 断在哪

`set_session` / `new_session` 的参数从一串平铺的 `--model` / `--thinking` / `--cwd` 变成了
一个 JSON 对象 `--config`（`session_config::flatten` 再把它摊回平铺设置）。粘性值只认
`ArgSpec.kind`，而 `config` 没有 kind、它的叶子也没有名字可供填充，于是：

- `/set_sticky --type model …` 直接被拒：「model is not a type any published argument declares」；
- 就算写进文件，`new_session --id x` 也只发出 `--config` 缺失的状态，服务端按自己的默认值建会话。

同类走样还有 `ls_skill` / `run_skill`：它们的 `session` 是 `pos(...)` 建的位置参数、没有 kind，
sticky `session` 填不进（报 missing required argument session）。

## 规则：只补「调用者必须给却没给」的那个值

SPEC 把 sticky 记在类型上（`Session` / `Model` / `ModelConfig` / `SessionConfig`），并说
「auto fill in missing **required** argument」。于是判据只有一条：

> 这个槽位有默认值吗？有 —— 值是类型定的，粘性条目不填；没有 —— 那是缺的参数，可以填。

对应到今天的目录：

| 槽位 | SPEC | 填吗 |
| --- | --- | --- |
| `session`（各命令的会话参数） | `Session := path`（必填） | **填** |
| `model_config.model` | `model: Model`，无默认 | **填** |
| `keep_alive` | `keep_alive: bool`，无默认 | **填** |
| `thinking` | `= provider_default()`，`# re-default if model change` | 不填 |
| `ctx_limit` | `= 70%` | 不填 |
| `cache_tier` | `= provider_default()` | 不填 |
| `workspace.remote` | `= none` | 不填 |
| `workspace.cwd` | `= "."` | 不填 |
| `mode` | 本 build 自己的，默认 `work` | 不填 |

## 改了什么

- 协议：`StructField.default: Option<String>`（SPEC 的拼法，`None` 表示没有默认）；
  `ArgSpec.sticky` 整个删掉 —— SPEC 不再有逐参数的 `(sticky)`，填与不填由「有没有默认」决定。
- 服务端：`session_config::schema` 给 `thinking` / `ctx_limit` / `cache_tier` / `remote` / `cwd`
  标上默认（`defaulted`，一处）；`model` 与 `keep_alive` 不标。新增 `kind_pos`，
  `interrupt_session` / `ls_skill` / `run_skill` 的会话参数统一用它。
- `Sticky::fill`：顶层只填 `required` 的参数；结构化参数逐叶填，跳过 `default.is_some()` 的叶子。
  一个叶子都没填就整块不生成。
- `commands::sticky_kinds`：**能填的类型** = 必填参数的 `kind` + 无默认的叶子。三个读者共用：
  `set_sticky` 的拒绝名单、`--type` 的补全、以及回写（写回一个永远填不上的条目等于往文件里
  写谎话）。今天的列表就是 `keep_alive, model, session`。
- 回写：`set_session` / `new_session` 成功后，把行里**写到的**、且属于上面那张表的类型写回；
  `--config` 的叶子逐个写回。填进去的值不写回。
- 模型两种拼法：`ModelValue` 接受 `{provider, model}` 与 `provider/model`（文件里存后者，JSON 里
  要前者）；客户端 `leaf_value` 按叶子形状插入（`model_ref` 的叶子读成 JSON，`keep_alive` 读成布尔）。

## 类型是整体的：Session / Model / ModelConfig / SessionConfig

用户第四问：`Config` / `ModelConfig` / `Workspace` 是否作为整体被 sticky。SPEC 只在
`Session`、`Model`、`ModelConfig`、`SessionConfig` 上写了 `# sticky`，**`Workspace` 没有**。
于是条目就是四个：

| 条目 | 类型 | 值 |
| --- | --- | --- |
| `session` | `Session` | 一个名字 |
| `model` | `Model` | `{provider, model}`（或 `provider/model` 字样） |
| `model_config` | `ModelConfig` | 一个对象 |
| `config` | `SessionConfig` | 一个对象 |

`cwd` / `remote` / `thinking` / `keep_alive` 不是类型，是成员，只随 `config` 走。目录用
`ArgSpec.kind`（`config`）和 `StructField.kind`（`model_config`、`model`）说出这些类型。

- 填：结构化值按**自己的**条目整体填（再把行里写的盖上去），窄的压宽的（`model` > 
  `model_config` > `config`），并且任何**有默认值**的字段在哪一层都不填（`prune` 也把旧
  条目里残留的这类值切掉）。
- 回写：`absorb` 把行里写的剪枝后**合并**进该类型的条目，字段有自己的类型就各自再写一份——
  所以今天的 `set_session --config {model_config…}` 既更新 `config`，也更新 `model_config`
  和 `model`。
- `set_sticky` 的可选类型因此正好是 `config, model, model_config, session`；`cwd`、
  `keep_alive` 这类会被拒。

## struct update 不填，只回写

用户第三问：`SessionConfig{..}` 是 struct update，不该被 sticky 填。SPEC 的两个签名正是两种东西：

- `new_session(id, config: SessionConfig)` —— 一个**值**，其中 `model` 无默认，是缺的必填参数，粘性填；
- `set_session(id, config_delta: SessionConfig{..})` —— 一个 **struct update**，说的是「改什么」，
  被粘性填就等于「改一个字段」变成「重申整份配置」，所以不填；它写到的叶子回写进条目。

目录里用 `ArgSpec.update` 说出这件事（`config_arg(remotes, update)`，`set_session` 传 true，
`new_session` 传 false），客户端 `fill_struct` 见到 update 直接返回。

## 验证（真服务端 + 真 gongga-client 管道，临时 home 在 .dev/s3）

粘性文件按用户的形状写（`session` / `model` / `cwd` / `mode` / `thinking`）：

```
/new_session --id s1   → new_session --config {"model_config":{"model":"fake/flash-fake"}} --id s1 → ok
                         （config 是值：model 无默认，填）
/set_session --id s1   → set_session --id s1（update 不填，行原样发出）                        → ok
/set_session --id s1 --config '{"model_config":{"thinking":"high"}}'
                       → set_session --config {"model_config":{"thinking":"high"}} --id s1 → ok
                         （只发调用者写的；thinking 有默认，也不回写）
/set_sticky --type model_config --value '{"model":{"provider":"fake","model":"flash-fake"}}'   → ok
/set_sticky --type config --value '{"keep_alive":true}'                                        → ok
/set_sticky --type cwd --value …      → error: the types are: config, model, model_config, session
/set_sticky --type keep_alive --value true → 同上（它是 config 的成员，不是类型）
/new_session --id s1   → --config {"keep_alive":true,"model_config":{"model":{"provider":"fake","model":"flash-fake"}}} → ok
/set_session --id s1 --config '{"model_config":{"model":{…}}}'
                       → 只发写的；回写后 client.toml 里 config 合并（保留 keep_alive）、
                         model_config 与 model 各写一份
/new_session --id s2 --config '{"workspace":{"cwd":"C:/resource"}}'
                       → --config {"model_config":{"model":"fake/flash-fake"},"workspace":{"cwd":"C:/resource"}}
                         （调用者写的 cwd 保留，粘的 model 补上）                              → ok
                         （cwd 有默认 "."、thinking 有 provider_default、mode 有 work：一个都没填）
/set_sticky --type cwd --value C:/resource
                       → error: "cwd" is not a type any published argument declares; the types are:
                                keep_alive, model, session
/set_sticky --type keep_alive --value true
/new_session --id s5   → --config {"keep_alive":true,"model_config":{"model":{...}}} → ok（布尔不是字符串）
/ls_skill              → ls_skill s2（不再 missing required argument）
/set_session --id s1 --config '{"workspace":{"cwd":"…"},"model_config":{"thinking":"on"}}'
                       → 行里写的 cwd/thinking 保留，粘的 model 合并进去 → ok
```

回写：上面那条之后 `client.toml` 里只多了 `model`（以及 `session`），**没有** `cwd`、`thinking`
——它们填不上，也就不该被记住。

`cargo build`（私有 target）、`cargo fmt --check`、`comments-check`、`test-layout-check`、
`scripts/skill_contract.sh`、`scripts/ship.sh --check` 均通过（ship 与内容仓无改动）。

## 没做的

- `cache_keepalive` 是旧名，不会被当成 `keep_alive`。
- 文件里的 `cwd` / `remote` / `mode` / `thinking` 旧条目留着不生效；要清就 `del_sticky`。
