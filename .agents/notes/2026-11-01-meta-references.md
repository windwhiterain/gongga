# SPEC 的 `## File` 换成 `## Meta`，`skill` 命令删掉

SPEC 把「`@<path>.<图片后缀>` 换成一个文件 id」改成了三条引用，并且删掉了
`skill --session --skill` 这个命令。

## SPEC 说了什么

- `@image: <path>`：换成一个文件 id；引用的图片提前用 File API 上传。
- `@skill: <name>`：换成技能元数据 {description, path}。
- `@file: <file>`：原样不动。
- 这三个在 cmd_input 里都要自动补全。
- `Cmd` 的 server 一节不再列出 `skill`。

## 定下来的四个取舍（用户拍板：全选 A）

1. **语法只有一套**。工具输出里的附图也改写成 `@image: <path>`，没有第二套
   `@x.png`；`image::references` 因此删掉，解析集中到 `meta::mentions`。
   代价是命令行打印 `@shot.png` 不再自动附图；收益是一个语法，补全和解析读同一份。
2. **上传仍在回合前**（发送时就传）。字面「in advance」读作「在这个请求之前」，
   和工具结果现在的做法一致：`deliver` 拿到会话将要使用的那个 client，上传得到的
   id 直接写进那一行。
3. **入库前替换**。库里存的就是替换后的文本：技能变成一行元数据，图片从文本里
   拿掉、记在行的 `images` 上。发送那一刻机器上是什么，重放就是什么。
4. **允许改既有测试**：删 `skill` 命令的两个测试、改图片语法的断言。

## 落点

- `crates/protocol/src/reference.rs`（新）：`Reference::{Image, Skill, File}` 和三条
  前缀。server 解析、cli 补全共用一份，否则补出来的东西可能解析不了。
- `crates/server/src/meta.rs`（新）：`mentions` / `expand`。不可解析的引用原样留在
  文本里——读不到的图、没装的技能，都只是这条消息的一部分。
- `crates/server/src/commands.rs`：`deliver` 里展开一次，`begin_turn` 与 inject 的
  pending 载荷都带展开后的文本和图片（`Expanded::to_json`）。
- `crates/server/src/store.rs`：`user_row` 写 `images`；`deliver_pending` 从载荷读回。
- `crates/server/src/transcript.rs` / `body.rs`：`text` 行也开始带图，图片落进
  `content`（工具结果落在 `output`）——同一个 `with_pictures`，按 kind 选字段。
- `crates/cli/src/catalog.rs`：不以 `/` 开头的行不再一律返回空，改补 `@` 引用：
  先三条前缀，再各自主值（图片/文件是路径，技能是名字）。
- 删掉 `ImageRef::body` 与 `skill` 命令后留下的死代码。

## 没做的

- 没有端到端跑一次真上传（要有 provider）。单测覆盖到「行里怎么存」和「请求里
  怎么拼」，上传那一跳只有编译期的保证。
