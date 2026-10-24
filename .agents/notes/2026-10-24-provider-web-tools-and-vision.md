---

# 服务端：provider 内置 web search / web fetch，以及识图

## 问题

`llm.rs` 说的是 Responses API，但只发本地 `function` 工具；provider 侧工具没有任何通道。
流式解析里 `ItemKind::Unknown` 直接吞掉，所以 `web_search_call` / `image_generation_call`
这类 item 静默消失——用户看到的是"这轮忽然安静了"，模型其实正在读网页。
`ResponsePart` 只有 Reasoning / ToolCall / Output，没有承载"模型在联网"的变体。
roster 里 `modalities` 早就写了（`deepseek-flash` 就是 `["text", "image"]`），服务端不读。
`HistoryItem::ToolResult.output` 是纯字符串，装不下图片。

## 改法 A：provider 内置工具

- **roster 字段**：`[[provider.model]]` 新增 `builtin_tools = [...]` 与 `modalities = [...]`。
  `builtin_tools` 的值是 **provider 自己的拼写**（`web_search` / `web_search_preview` /
  provider 自己的 `web_fetch`），服务端**原样透传**成 `{"type": "<name>"}`。不做映射表：
  那等于把"provider 会什么"写死在一个读不到 provider 文档的服务端里。未声明 = 不发
  （发一个 provider 不认识的名字，是每一轮每个请求都 400）。
- **`build_body`**：本地 function 与内置工具**合进同一个 `tools` 数组**；两者都空时不发这个
  key（空数组和"没有 tools"对部分 provider 不是一回事）。
- **解析**：判定规则是 `*_call` 后缀（`function_call` 先匹配，那是我们的），不是名字白名单——
  没听说过的 provider 工具也必须能被看见。item 变成新的 `ResponsePart::Activity`：
  行首是工具名（`web_search`），后面接 item 自己给的尾巴（query / url / revised_prompt）和
  provider 自己的进度拍（`searching` / `in_progress` / `generating`），同一个 stream key。
- **cli 渲染**：`Activity` → `LineKind::ToolCall` + `"[session] provider: "` 前缀。用
  `provider:` 而不是 `tool:` 是因为两者语义不同：一个是我们执行的，一个是 provider 已经执行完的。
  没有它，"看不见"只是从上个任务换了个地方。
- **citation**：`response.output_text.annotation.added` 的 `url_citation` → 一行
  `[session] provider: cited "title" — url`，同时落一条 `notice` 行（只给操作者看，不回喂模型）。
- **回放**：内置工具 item 存原来的 JSON（`builtin_call` 行），下一次请求原样放回 `input[]`。
  这条最容易漏：`function_call` 有 `function_call_output` 配对，内置工具没有本地回执——
  它们的 item 本身就是 "already executed by the provider"。丢掉它，下一轮请求会因为这个
  transcript 引用了一次没人执行过的调用而被拒；只回放一部分同样会被拒。
  `image_generation_call` 带 base64 `result`：入库前把 payload 摘掉（transcript 保持文本且小），
  并把 item 标成不可回放——摘掉 payload 的 item 不是 API 会接受的形状。**这是取舍**：
  生成图当场可见，之后不再回放。

## 改法 B：识图

- **`read_image{path}`** 本地工具：相对路径按会话 cwd 解析；先问 roster（`modalities` 里没有
  `image` 就**明确拒绝**，消息里点出是哪条 roster 条目、缺哪个 modality），再读文件；认不出
  格式就列出它认识的扩展名。
- **进对话的通道**：`ToolOutcome` 多一个 `images: Vec<ImageRef>`（path + media_type）。
  请求组装时把 `function_call_output.output` 从字符串换成内容数组
  `[input_text, input_image]`，`input_image.image_url` 是 `data:<type>;base64,...`。
  模型拿到的是**图片本身**，不是一大段 base64 文本。
- **存储**：sqlite 里只有路径和 media type，没有 base64。理由：SPEC 要的是"结构化、便于
  agentic 检索/改写"的状态；base64 会把 `messages` 变成二进制垃圾场，而且每轮请求都重发一遍，
  `get_messages`（CLI 会打印）会把整张图喷���终端。
  **代价写在文档里**：文件就是状态。文件没了，回放就缺这张图。缺图时降级成一句
  `[the image at <path> is no longer readable (...); whatever it showed cannot be recovered]`，
  不让整个 turn 挂掉。
- **压缩**：`summary_prompt` 对带图的 `ToolResult` 写一行 `(an image was shown here: <path>)`，
  对 `BuiltinCall` 写 `(<type> ran at the provider)`。摘要不能塞下图，但不能悄悄丢。

## 顺带修的一处（与任务相邻，必须说明）

`write_default_if_absent` 在**全新 home 上从来不生效**：首次启动时 `load_with_home` 生成 key 并
`persist_key` 写了 `[server] key`，等模板写入时文件已存在，于是直接返回。也就是说
`config.rs` 里那份模板注释（本来正是"用户靠它学字段"的地方）**没人看得到**。
改法：`is_key_only` —— 文件里只有那把 key（没有任何其他字段）时，用模板重写并保留同一把 key；
只要用户动过一个字段，就一个字都不改。加了两个测试。

## 验证

- `cargo test --workspace`：249 passed / 0 failed（server 侧 137）。新增 21 个测试。
- `test-layout-check --config test-layout.toml`：0 violations。
- `comments-check --config comments-check.toml`：我改的 6 个文件干净；**还剩 2 条违规在
  `crates/tui/src/log.rs:269` 和 `:465`，是另一个 agent 未提交的 block 渲染改动**（都在
  `+` 行上），按硬约束没有动别人的文件。
- `cargo fmt --all -- --check`：干净。
- 端到端冒烟（自建端口 8810/8820/8830 + 自建 home `.dev/home-webtools*`，不碰 9700）：
  - web：`[main] provider: web_search (searching) "rust async book"` 一行、citation 一行；
    第 2 轮请求的 `input[]` 里 `{"type": "web_search_call", "action": {"query": ...}}` 原样出现。
  - image：`read_image` 回执在下一轮变成
    `[{"type":"input_text",...},{"type":"input_image","image_url":"data:image/png;base64,..."}]`；
    删掉文件后同一位置降级成 `input_text` 的说明句，turn 不失败。
  - blind（`modalities = ["text"]`）：工具回执是
    "`read_image shot.png` did not run: this model cannot look at images: the roster lists
    modalities = [\"text\"] for fake-blind..."，且请求体里**没有** `input_image`。
- 测试床：`scripts/fake_llm.py` 加了 `FAKE_LLM_DUMP`（把收到的请求体逐行追加到文件）、
  `FAKE_LLM_WEB`（web_search_call + citation）、`FAKE_LLM_TOOL`（指定工具名，仅首次请求），
  并把"首次请求发工具调用、之后答话"做成有状态的，不然会无限循环。
  冒烟需要一个能看渲染行的客户端，`cli` 的 one-shot 模式在 `cmd_result` 就返回（看不到
  turn 的事件），所以 `.dev/probe/` 放了一个只依赖 `cli` 的小程序打印 Sink 收到的
  `prefix/text/key`。它的 `Cargo.toml` 用完改名成 `Cargo.toml.off`，否则
  `test-layout-check` 会把 `.dev` 当成第二个 workspace 扫进去（改名即可复跑）。

## 没做 / 没验证的

- **没有真实 provider 验证**。`web_search` 的 item 形状、`builtin_tools` 的真实接受度、
  `input_image` 在各家的实现差异，都只对 fake 端点验证过。openai-responses 之外的
  命名（`web_search_preview`）没跑过。
- **`image_generation_call` 只做了"可见 + 不入库 payload + 不回放"**。生成的图没有被存成文件
  再当作 `ImageRef`（那样才可能回放），因为那要把 provider 的 base64 落盘、还要决定目录与清理，
  这一轮没做。
- **回放的正确性依赖 provider**：如果某家的 API 不接受 `input[]` 里出现自己产出的
  `*_call` item（比如要求 `store: true` 或 `previous_response_id`），那么这一轮的"原样回放"
  策略在那家会失败。fake 端点不会暴露这件事。
- **图片没有大小/数量限制**（AGENTS.md 禁止设预算），所以一次 `read_image` 一张几十 MB 的图
  会整块进请求体。真实 provider 的请求体上限会以 413 的形式出现，目前没有专门的降级路径。
- **压缩时图片只留一句话**：摘要之后模型知道"这里曾有一张图、路径是什么"，但它**再也看不到**
  那张图。如果要"压缩后仍能看"，得让摘要保留 ImageRef（而不是只留文字），那是改
  `HistoryItem` 的摘要形状，本轮没做。
- **`Activity` 在 TUI 的分块渲染里借用了 `LineKind::ToolCall`**，所以它和工具调用同色同组。
  要不要单独一种颜色，属于 TUI 那边的事（本轮不动 TUI）。

## 复核后修的缺陷：`builtin_call` 行在 get_messages 里是空的

### 问题

`MessageRow::parse`（给 `get_messages` 用的那个解析点）没有 `builtin_call` 分支，于是
落到最后那句 `other => tracing::warn!("unknown message kind {other}")`：

- `text` 留空 → 回看历史时那行只有 kind 没有内容，读者看不出发生过联网；
- 每读一次 transcript 就打一条 "unknown message kind builtin_call" 的 warn —— 那不是
  unknown，是我们自己写的 kind，日志会一直喊。

两个解析点：`MessageRow::parse`（展示）和 `history_between` 里那个（请求组装）。后者
本来就有 `builtin_call` 分支且是对的，缺陷只在展示侧。**只修了展示侧，回放一个字没动**
（回归证据见下）。

### 改法

- `activity_name` / `activity_detail` 从 `llm.rs` **移到 `store.rs`** 并 `pub(crate)`：
  它们描述的是"一个被存下来的 item"，而描述这个东西现在有两个读者（流式那一行、transcript
  那一行）。留在 llm.rs 会逼 store 反向依赖 llm（现有方向是 llm → store），所以放 store，
  llm 去 `use` 它——**同一份提取逻辑，两处渲染不可能漂移**。
- 新增 `activity_summary(item)`：`activity_name` + `activity_detail`，形如
  `web_search "rust async book"` / `web_fetch https://example.org/a` /
  `image_generation "a red cube on a table"`。取不到的字段直接省略，不写 `{}`、不猜；
  item 连 `type` 都没有时给 `a provider tool ran (its item was not recorded)`——
  空行会被读成"这里什么都没发生"，那正是这个缺陷本身。
- `MessageRow::parse` 加 `"builtin_call"` 分支，不再走 `other` 那条 warn。

### 诊断那一句（你问的第 1 件）

原来只有 "an activity item with a binary payload cannot be replayed; dropping it" ——
不知道是哪一条。现在带上 `item_type` 与 `item_id`，实测输出：

```
WARN gongga_server::llm: this provider tool item lost its binary payload when it was stored,
so it cannot be replayed; leaving it out of this request
item_type="image_generation_call" item_id="ig_1"
```

指向性够了（说了原因、说了后果、说了是哪条）。

### 新增测试

- `a_provider_tool_row_reads_as_the_search_it_ran`：`web_search "rust async book"`、
  `web_fetch https://example.org/a`。
- `a_provider_tool_row_never_renders_blank`：四种残缺 item（无 detail、failed 状态、
  无 type、整条没有 item），断言**没有一行是空白、没有一行漏出 `{}`**；裸 item 是
  `web_search`（无尾随空格），failed 是 `image_generation (failed)`。

### 验证

- 端到端（fake + 自建端口/home）：`get_messages` 的行现在是
  `2 builtin_call -> 'web_search "rust async book"'`；server 日志里
  `unknown message kind` 出现 **0** 次。
- **回放没动**：第二轮请求 input 仍是
  `{"action":{"query":"rust async book","type":"search"},"id":"ws_1","status":"completed","type":"web_search_call"}`。
- image_generation_call（新增 `FAKE_LLM_IMAGE_GEN=1` 测试床）：两轮请求体里
  **没有任何 base64**，第二轮那条被丢弃并打出上面那句 warn；行文本是
  `image_generation "a red cube on a table"`。
- 四个检查全绿：`cargo test --workspace` 251 passed（新增 2 个）、`test-layout-check`
  0 violations、`comments-check` 干净、`cargo fmt --check` 干净；`cargo build --workspace`
  零 warning。

### 顺带写进文档

`docs/notes/server-llm.md` 新增「待确认」一节：如果某家 provider 要求 `store: true` /
`previous_response_id`，回放自己产出的 `*_call` item 会被拒，表现大概是
`invalid_request_error` 的 400 点名该 item 或缺失的 `previous_response_id`，而且**每个**
带这条 item 的会话请求都会 400（看起来像会话坏了，而不是��条 item 坏了）。真撞上时的
出路也写了：`[llm] store = true` 已经是现成开关，但发送形状要改成 `previous_response_id`
+ 只发新 item，那是改 `build_body`。

### 看到但没改

`crates/bridge/src/chat.rs` 的 `final_reply` 对 `_ => {}`，所以 `builtin_call` 不算"回答"
（这是对的），但如果一轮里只联网、没说话，兜底是「跑完了，但没有任何输出。」——
没提到它搜过网。属 bridge 的表述问题，不在这次范围内，记在这里。
