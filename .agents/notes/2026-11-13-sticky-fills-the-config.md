# Sticky reached nothing but `session` and `mode`

用户报「sticky 功能好像不怎么生效」。`~/.gongga-client/client.toml` 里存着六条：
`session=description`、`model=deepseek-official/deepseek-flash`、`cwd=c:/resource/`、
`remote=lichen-vm`、`mode=work`、`cache_keepalive=false`（旧名）。但真正会生效的只有
`session` 和 `mode`，其余三条连同 `thinking` / `ctx_limit` 早已填不进任何命令。

## 断在哪

`set_session` / `new_session` 的参数从一串平铺的 `--model` / `--thinking` / `--cwd` 变成了
一个 JSON 对象 `--config`（`session_config::flatten` 再把它摊回平铺设置）。粘性值只认
`ArgSpec.kind`，而 `config` 没有 kind、它的叶子也没有名字可供填充，于是：

- `/set_sticky --type model …` 直接被拒：「model is not a type any published argument declares」；
- 就算写进文件，`new_session --id x` 也只发出 `--config` 缺失的状态，服务端按自己的默认值建会话。

另外两处同类走样：`ls_skill` 与 `run_skill` 的 `session` 是 `pos(...)` 建的位置参数，没有 kind，
于是 sticky `session` 填不进它们（`ls_skill` 报 missing required argument session）。

## 改了什么

SPEC 今天把 sticky 记在**类型**上（`Session` / `Model` / `ModelConfig` / `SessionConfig` 都带
`# sticky`），并把填充规则收成「missing **required** argument」。据此：

- 服务端：新增 `kind_pos`（位置参数的 kind），`interrupt_session` / `ls_skill` / `run_skill`
  的会话参数统一用它；`config` 声明 `sticky`（可选但可以填）；`mode` 也声明 sticky。
- `Sticky::fill`：必需参数照旧填；**可选参数只有声明了 sticky 才填**（`del_subscription --session`
  这类不再被偷偷填上）；结构化参数逐叶填，行里写了的叶子不动，一个叶子都没填就整块不生成。
- 叶子名就是粘性条目的名字（`model` / `thinking` / `ctx_limit` / `cache_tier` / `remote` /
  `cwd` / `keep_alive`），与 `flatten` 的平铺设置同名——一套词汇，不是两套。
- 回写：`set_session` **和** `new_session` 成功后，把行里写到的值按类型写回；`--config` 的
  叶子逐个写回。填进去的值不写回（它本来就在文件里）。
- `client::commands::sticky_kinds`：参数 kind + 结构叶子，两个读取处（`set_sticky` 的拒绝、
  `--type` 的补全）共用一个走查。
- 模型两种拼法：`ModelValue` 接受 `{provider, model}` 与 `provider/model`。文件里存的是后者，
  而 JSON 里要前者；客户端 `leaf_value` 按叶子的形状决定插入什么（`model_ref` 的叶子读成 JSON）。
- 旧的 `restates` / 「left off」那套「只有和行一致才填」的规则删掉了：SPEC 说 sticky 是
  **模式**，`set_session --id s1` 就该套上记住的 model/cwd；真正该拦的是**模型不收的值**
  （`thinking=low` 而 roster 只发布 `on, off`），这条照旧：`refused_values` 现在也走结构叶子，
  且模型从**参数**里读（粘性 model 也算），拒绝的叶子从对象里切掉并说一句。

## 验证（真服务端 + 真 gongga-client 管道，临时 home 在 .dev/sticky）

粘性文件照用户的形状写（model / cwd / mode / session）：

```
/set_sticky --type model --value fake/flash-fake      → sticky model = fake/flash-fake
/new_session --id s9        → 发出 new_session --config {"model_config":{"model":"fake/flash-fake"},
                              "workspace":{"cwd":"C:/resource/gongga"}} --id s9 --mode work → ok
/ls_skill                   → ls_skill s9（不再 missing required argument）
/new_session --id s10 --config '{"model_config":{"thinking":"high"}}'
                            → config = {model: 粘的, thinking: 写的, cwd: 粘的} → ok
/new_session{"id":"s11"}   → 同样填好（JSON 形式与 flag 形式一条规则）
/set_session --id s1        → 粘性 config 套到一个已存在会话上 → ok
/del_subscription           → 只报缺 path；可选 --session 没有被填（SPEC 的 required）
```

回写：上面 `--config {"thinking":"high"}` 之后 `client.toml` 多出 `thinking = high`。
模型拒收：粘性 `thinking=low` + `model=fake/flash-fake`（roster 只发布 `on, off`）→
`note: sticky thinking = low is not one this model takes; left to the model's own`，
对象里没有 `thinking`，会话照样建成功。

`comments-check` / `test-layout-check` / `scripts/skill_contract.sh` 均退出 0。

## 没做的

- sticky 仍按**参数/叶子**存，不按 SPEC 的类型名整体存：`--type model_config` / `--type config`
  会被拒（拒绝信息里列出可用的类型）。`ModelConfig` / `SessionConfig` 的粘性由它们的叶子实现。
- `cache_keepalive` 是旧名，不会被当成 `keep_alive`。
