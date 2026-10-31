# provider 是公司，model 是脑子：一次把「名字」理清的改名

日期：2026-11-01

## 症状：同一个东西有两个名字，而且都不对

上一轮把 `--llm` 改成了 `--provider`，改的动作是对的（`llm` 不是用户会说的话），
落到的地方却错了一格：

- `set_session --provider deepseek-official/deepseek-flash` 里的值**不是** provider，
  是 provider + model。名字说"提供商"，传的是"哪个模型"。
- `SessionInfo.provider` 同理：标题上显示 `provider deepseek-official`，
  而 operator 真正要的是"这轮是谁在答"，那只说了半句。
- 配置表 `[provider]` 里装着 `model`、`base_url`、`temperature` —— 表名说的是公司，
  内容是"跑哪个模型、怎么跑"。
- DB 的 `sessions.provider` 存 `provider/model`，`sessions.model` 存裸模型名。
  两个列名一个比一个含糊：看到 `model` 以为是全套，看到 `provider` 以为是公司。

## 定名：一个模型是 provider + model name

三层的名字现在说的是同一件事：

| 层 | 名字 | 内容 |
| --- | --- | --- |
| 命令 | `set_session --model <provider>/<model name>` | 一个模型，可以只写名字 |
| 线上 | `SessionInfo.model` | `provider/model_name` |
| 配置 | `[model]` 表，`model_name` 字段 | 跑哪个模型、怎么跑 |
| 数据库 | `sessions.model` | `provider/model_name` |

`provider` 这个词只剩一个意思：**谁提供的**。所以 `Quota.provider` 还是 `provider`
（额度是账号的，不是模型的），`[[provider]]`/`providers.toml` 还是 `provider`（那是名单）。

`Entry` 上因此有两个字段而不是一个：

- `model`：`provider/model_name`，调用方记下来的、标题显示的那个；
- `model_name`：API 自己的 `model` 字段吃的名字（provider 不吃自己的 id）；
- `model_title`：名单里的人读的名字。

在每次用的时候按第一个 `/` 切一次是能做，但"怎么切"只有一个正确答案，
应该落在读名单的地方，而不是散在十几处调用点。

## 改名怎么不丢数据

`migrate` 里现在有三次原地重命名，顺序有讲究：

```
llm     -> provider   （上一轮的措辞）
model   -> model_name （裸名字让位）
provider -> model     （整套模型进来）
```

顺序反过来就会把 `model` 覆盖掉。`SESSION_SELECT` 读的是
`COALESCE(s.model, s.model_name)`：一行只知道它自己名字的老数据，升级后仍然能读出
它的模型，哪怕名单已经改了。两条迁移路径都有单测
（`the_older_model_columns_are_renamed_not_dropped`、
`a_row_that_only_knows_the_name_still_reads_its_model`）。

配置两个旧写法都能读：`[provider]` 是 `[model]` 的 serde alias，表里的 `model` 是
`model_name` 的 alias（`the_older_table_and_field_names_are_still_read`）。
所以一个老 home 不需要改文件，下一次安装把新表合并进去就行。

`pick_model` 多了一条规则：`model_name`/`model` 里自带 `/` 时按全套解析，
provider 那一半只当补充说明。这条规则让"两种写法"变成同一个设置的两种精度，
而不是两个可能互相矛盾的设置。

## ship 内容

技能是模型照着敲的字面量，`set_session --provider` 在新服务端上会直接报
unknown argument，所以这一改必须同时发内容仓库：

- `config/config.toml`：`[model]`，`model_name`，`image_refusal` 的提示改成 `--model`；
- `subagent` / `evolve` / `self-control` 三个 skill；
- `heir/transcript.py`：不再往 `provider` 列写（那列没了），只写 `model`。

`scripts/ship.sh -m "..."` 一次做完：提交、push、写 pin。`heir_selftest.sh` 拿真
schema 跑过——它的 `--schema-source crates/server/src/store.rs` 就是把"脚本的 insert
和服务端的 schema 对不对得上"当断言。

## 验证

- `cargo test --workspace` 全绿（server 251、tui 116、cli 94）；
- `scripts/heir_selftest.sh` 全绿；
- 真服务端 + 假 roster（`.dev/rename-smoke`，用完删掉）：
  `--model fake/m1`、`--model m1`、不给 `--model`、`--model nosuch/m1` 四种输入
  分别得到「原样」「补成 fake/m1」「服务端默认 fake/m1」「`--model nosuch/m1: no
  provider "nosuch"; known: fake`」；
- 库里的行是 `('one','fake/m1',None)`：`model` 有全套，`model_name` 只有老行才有值。

## 没做

- 没有把 `--model` 拆成 `--provider` + `--model-name` 两个参数：一个模型是一个选择，
  能不能拆开写是**写法**的自由（`pick_model` 允许），不是接口的自由。
- 没动 `DESIGN.md`：它从 `--llm` 那一轮起就没跟上，属于历史记录。
