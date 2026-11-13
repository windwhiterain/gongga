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

## 验证（真服务端 + 真 gongga-client 管道，临时 home 在 .dev/sticky2）

粘性文件按用户的形状写（`session` / `model` / `cwd` / `mode` / `thinking`）：

```
/new_session --id s1   → new_session --config {"model_config":{"model":"fake/flash-fake"}} --id s1 → ok
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
