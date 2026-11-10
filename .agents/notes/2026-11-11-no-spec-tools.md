# SPEC 之外的工具（WIP）

判据是操作员的原话：**SPEC 没有的东西必须删掉**。SPEC 的 `## Tool` 只有 `exec`、`wait`、
`live_compact`，以及调用时由 skill 动态给出 schema 的 `fn <tool>(**args)`。

## 已经删掉的

- 内容（`ship` `3bfe91c`）：`[tool.bash]`、`remote_bash`、`check_bash`、`check_job`、
  `del_bash`、`del_job`、`list_bash`、`read_file`、`write_file`、`replace`、`skill`
  这十一行，连同它们的 `[tool_text.*]`，共 383 行。留下 `exec`、`wait`、`live_compact`。
- 代码：`ToolRegistry::command()` 里 `read_file`/`write_file`/`replace` 与 `skill` 的
  装配分支。那几个名字现在无论行怎么写都建不出工具。

## 没有删的，以及为什么

`detached`、`job`、`skill`、`python_remote`、`file` 五个模块留着。它们不只是工具的实现：
`detached` 是通道的 detach/adopt，`job` 是 background 的作业表，`skill` 里有 `run_skill`
命令用的 `installed_for`/`programs`，`file::Reply` 被 `python_remote` 用。删掉会产生
43 个编译错误，横跨 12 个文件——那是重写执行模型，不是删工具面。

## 顺手修的一处

`fork::rebase_personal` 以前把 `git rebase` 的任何失败都当成“与上游冲突”，且只 warn。
实际最常见的原因是工作区有未提交改动（git 直接拒绝 rebase），于是日志一直说谎、home
永远停在旧内容。现在把 git 自己的第一行抱怨放进错误消息，并按 error 报。

## 待办

- 无。`scripts/cmd_selftest.sh`、`disk_selftest.sh` 已改说 `exec`；`remote_selftest.sh`
  删掉了文件工具与 `bash`/`list_bash` 两段（两个机器的说法由 `channel_remote_selftest.sh`
  承担）。`tool_selftest.sh` 本来就是断言这些名字**不再**被提供，保留。
- 三个脚本都还没在真机上跑过：这棵树里没有可指向的 `gongga` 构建，只过了 `bash -n`。
- 提示词：`prompts.rs` 目前没有点名这些工具（grep 为 0），没有要删的句子。
- SPEC 第 39 行要求每个调用带 `discription`；代码原来只认 `description`。现在
  `note_of` 两种拼写都认，SPEC 的拼法不会再被当成"没留备注"。
