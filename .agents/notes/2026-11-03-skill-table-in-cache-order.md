# 前缀缓存顺序 + 静态系统提示（WIP）

User 的指令：「把 github 上 windwhiterain/liana 项目的老分支拉下来，里面有一个专门的前缀缓存
优化算法，把那个模块移植到这个项目，用于优化 skill 加载顺序的选择」。

## 源

`https://github.com/windwhiterain/liana`，分支 `v1`（HEAD `15a0978`，2026-07-27）。
注意命名是反的：`master` 反而更老（`770bd06`，2026-07-12），`v1` 才是新分支，模块也更大
（330 行的 `memory.rs` 对 232 行）。`master` 的 `find` 代价走法相同，但节点大小靠手改，
还多一个 `remove_memory`；`v1` 的形状更适合移植。

移植的实体：`Manager`/`Node`/`find`/`messages` → `prefix_cache::Layout`/`Node`/`plan`/`order`。
成本模型原样保留（`MISS_SCALE = 64` 即 liana 的 `cache_miss_cost_scale`）。

## 两处**故意不同**

1. liana 的 `find` 会整条链发出，包括本轮没被选中的 memory（同一条对话里的旧内容，不算
   错）。skill 可以被卸载，所以「链上出现 `wanted` 以外的 key 的候选链」直接不作候选
   ——`cold_chains` 因此返回 `Option`。测试 `a_chain_through_an_absent_item_is_not_a_candidate`。
2. liana 用 `remove_memory` 事后修补被删子树的大小。这里没有删除，只有「每次重新量」，
   于是 `recost` 一趟按节点顺序重算即可（父节点索引永远更小）。测试
   `a_shrunk_item_is_cheaper_than_sending_it_cold`。

## 挂在哪

顺序是**会话**的属性，不是服务端的：缓存是每个对话一份，两个会话列同一张表是两个不同的
前缀。所以 `SessionInner` 多一个 `layout: Arc<Mutex<Layout>>`，随会话消亡；`ToolContext`
带过去；`index_skill::ordered` 用它重排，key 是 `bucket:name`（同名 skill 在不同 root 是
不同行），大小是该行序列化后的字节数。

`installed`/`in_dir` 不动：优先级仍是走查顺序（project → home → machine），`@skill:` 打开
的还是同一个文件；只有表格的行序归 layout 管。tool description 加了一句
「顺序不代表优先级」，免得模型从位置推重要性。

## 验证

- L1 `cargo test --workspace`：294 + 全绿（`tools::detached` 的两个进程树测试在并发下会
  偶发失败，单跑都过，与本改动无关）。
- L2 变异：把 `index_skill::ordered` 换回原序 → `the_table_is_the_order_it_was_first_emitted_in`
  变红；去掉「非 `wanted` 链不作候选」→ `a_chain_through_an_absent_item_is_not_a_candidate`
  变红（写的时候就是它先抓到的）。
- `comments-check` 新文件 0 violation（仓库整体仍有 154 处既有违规，未扩大）。
- `test-layout-check`：0 violation。

## 还没做

`MISS_SCALE` 的来源是 liana 的调参，不是实测：真比率属于 provider，没在这里测过。如果要，
落成 `.dev/verify/<sha>-prefix-cache.json` 那种数据，而不是改一个更大的常数。

## 第二轮（2026-11-01）：system prompt 不再渲染

User：「还没有 load skill 呢，你就接线了。先把 system prompt 的字符串模板功能删掉，只留最简，
其他说明放对应的 skill（大概率已经有了），否则 system prompt 都不稳定。」

- **代码**：`prompts::SYSTEM`（`"instructions"`）改成只 `text()`、永不 `render()`，新访问器
  `Prompts::system()`；`agent::prompt_in_force` 不再传 `{{session}}`。测试
  `the_system_prompt_is_never_rendered`。通报类模板（context_notice / output_cap / size_hint /
  image_refusal / interrupted_message）保持模板，它们的值本来就每次不同。
- **内容**：`instructions` 从七段压到三句——身份（会话名走 `$GONGGA_SESSION`）、一句授权
  （能跑命令、别问、别留垃圾）、一句"程序都在 skill 里，用 `index_skill`"。
- **搬迁**：shell 持久化 + `gongga-cli` 输出形状 → `ship/skills/self-control` §0；三个 skill 桶、
  `evolve` 为什么不在桶里 → 同文件 §12（原来写的"两个桶、项目目录不是来源"已经和代码不符，
  一并改成 project → home → machine）；「读得聪明、从测试开始、早点写、绿了就停」→
  `~/.agents/skills/whiting-dev` 新增 `## Working rhythm`（用户自己的常驻开发规矩 skill）。
- **发布**：ship `09537ef`，`crates/server/ship.toml` 跟上；`scripts/ship.sh --check` 一致。
- **安装验证**：临时 home 起一次服务端，日志 `pin=09537ef79b written=15`，装出来的
  `config.toml` / `skills/self-control` 都是新文本，`overridden=[]`；真 home 的两处文件与旧 pin
  逐字相同，所以下次启动是干净的三方合并。临时 home 已删。
- **还没做**：`@skill:` 现在只展开成一行 metadata（`meta.rs::skill_metadata`），而 SPEC 第 41 行
  写的是 "replaced as skill content with wrapper"——真正的 load skill 还没接。这才是顺序该被
  缓存算法管的那条路（正文进 transcript 才是贵的部分），`index_skill` 的表格只是它之前的
  廉价一步。

## 第三轮：真正的接线——首轮 index + 可��列化的 layout

User：「现在做真正的 prefix cache opt 接线：给空 session 发的首轮提示词，递归地搜索出全部的
@...，然后使用 opt find 找到最佳顺序，把他们的内容和 `@...` 原文作为索引 append 到 system
prompt 后面，prompt 里的根 `@...` 不再替换，用来索引 opt 的内容」+「i need the cache opt
across anything, so it should be serializable」。

- **首轮 index**：`meta::roots(cwd, texts)` BFS、先见先得、每个 spelling 一次；读得到的是
  skill 的整份 `SKILL.md` 和文本文件的字节，读不到（图片、未知 skill）不伪造、不入 index。
  `meta::spelling(mention)` 是唯一的规范拼写（`@skill:heir`，无空格）——index 用它，
  `expand` 拿它比对，两处不一致会让同一段文本进两次。
- **落点**：`store::Prompt` 新增 `index` 与 `rooted`。`agent::index_of` 只在**没有 prompt 行**
  时跑，之后每行原样复制：index 在 transcript 之前，长大一次就要为后面全部缓存付费。
  `llm::build_head` 把 index 拼在 `instructions` 同一个值里（不是第二个 input item），
  这样它跟散文一起被缓存。
- **不再替换**：`meta::expand(..., indexed)` 对 `rooted` 里的 spelling 原样保留，作为 index 的
  键；`commands::expand_message` 在**还没有 prompt 行**时假定「刚要写的那行会收录这条消息里的
  全部引用」，于是首轮既不展开也不漏，第二轮起读行上的 `rooted`。
- **可序列化 + 跨一切**：`Layout` 整个 `Serialize/Deserialize`，存进新表
  `prefix_layout(id=1, layout TEXT)`（`CREATE TABLE IF NOT EXISTS`，老 home 自动获得）。
  `prefix_cache::plan(store, wanted)` 是唯一入口：首次从库里读、plan、写回。进程内 Mutex 只
  缓存一份解码结果。会话字段删掉了——上一轮的 `SessionInner.layout` 是错的：provider 的缓存
  是按 endpoint 的，不是按对话的，重启后更该还记得。
- **E2E**（`scripts/fake_llm.py` + 真服务端 + 真 CLI）：两个会话 + 第二回合共 3 次请求，
  `instructions` 只有 **1 种**，index 在全部里，消息里的 `@skill:helper` 保持字面，
  `@file:` 文本里再提到的 skill 也被递归收录，磁盘上一个 2 节点的链。
- 单测：`roots` 递归/去重/读不到就跳过，`spelling` 一致才不重复展开，layout 存读往返后
  仍给出同一顺序。
- 已删临时脚本与 `.dev/idx-e2e`（`.dev` 是 gitignore 的，但也不留）。

## 还没做

- index 只在首轮建立；之后新提到的 `@...` 仍按老路内联展开。要不要「长大」是缓存账，不是
  正确性问题——长大一次，后面整段 transcript 失去命中。
- `MISS_SCALE` 仍是 liana 的 64，没在真 provider 上量过。

## 第四轮：对照 SPEC 的检查 + 内容即身份

User：「再对照 SPEC 检查一下有无遗漏」然后「如果 @... 引用的内容变了，cache 里要删掉旧的，不能把
新的当成旧的」。

### 对照 SPEC 第 44-46 行（`### Meta Prefix Cache Opt`）发现的三个遗漏，全已修

1. **图片会丢**。原文「find all meta references」包括 `@image:`，但图片没有「content」，我把
   它排除在 index 外，就顺手把消息里的 mention 也留成字面了——结果首轮的���片不再上传。
   现在由 `meta::literal(cwd, text, rooted)` 决定哪些 mention 保持字面：只有「index 真的收录了
   文本」的那些，其余照旧展开。图片、未知 skill 因此恢复原行为。变异测试：
   `a_message_leaves_literal_only_what_the_index_carries`。
2. **排队的消息不在 index 里**。首轮消息可能先进 inbox（`kind='inject'`），它被留成字面是因为
   「马上要写的那行会收录它」；但 `index_of` 只读 transcript，那条消息就永久悬空。现在
   `store::queued_texts` 一并读。
3. **index 的说明文字写在 Rust 里**（`meta::INDEX_HEAD`）。这正是 `prompts::REQUIRED` 存在的
   理由，而且启动检查当场抓到：新 pin 里没有 `reference_index` 模板时服务端拒绝启动，而不是
   发出带洞的 prompt。已改成 ship 里 `[prompts] reference_index`。

### 内容变了就是新东西

liana 的 memory 是追加式，id 即内容；这里相反——skill 正文、文件、表格行都会被改，名字不变、
字节变了。所以 `Item` 的身份是 `(key, content)`，`content` 是字节的 digest，`plan` 由调用方给：
`digest(&text)`。两条后果：

- 改过的文本是**新** item，旧链不能给它「预热」——否则就是「把新的当成旧的」，表现为每次
  复用该链都 miss，而 miss 恰恰是这模块要省掉的东西，且完全不可见。
- 旧 item 由 `forget_unwanted` 在每次 plan 后清掉（连同穿过它的链）；`forget` 放在 plan
  **之后**，否则会重编号 `ids`。留下的一定是刚被要过的。

`Layout::index` 是派生数据（JSON 没有元组 key），`#[serde(skip)]`，读盘后由 `reindex` 重建。

E2E 覆盖：首轮 index 有旧正文 → 改 skill 正文 → 之后两个会话 index 是新正文且两者逐字节相同
→ 磁盘 layout 里 `@skill:helper` 只有一个 item（新正文大小）。12 项全 ok。

## 第五轮：身份 = raw meta + 内容；每会话一个 cache；prompt 末尾放本回合事实

User：「判断是否相等除了内容还有 path，因为 path 在充当 index，也进了前缀」→ 更正
「充当 index 的是 raw meta，不一定是 path。总之就是 index 和内容都要相等」；以及
「给每个 session 分别建立 cache」；「在 system prompt 的最后，插入有用的动态相关信息
（session 名字，harness 名字+commit id，当前时间，cwd）」。

- **身份 = (raw meta, 内容)**：`Item` 的 key 就是 index 里印出来的那个 raw 拼��
  （`@skill:helper`），content 是正文的 digest。两者都相等才算同一个东西。path 不是
  特权字段——调用方印什么当名字，那个就是 key（skill 表格用 `SKILL.md` 的 path，因为那
  张表里 path 就是键）。
- **每会话一个 layout**：`prefix_layout(session TEXT PRIMARY KEY, layout TEXT)`，进程内
  `HashMap<String, Layout>`，`plan(store, session, wanted)`。`del_session` 经
  `forget_prefix_layout` 一起删掉——同名新会话是新对话，不能被旧链预热。
- **末尾动态尾巴**：`[prompts] dynamic_tail`，`{{session}} / {{build}} / {{now}} / {{cwd}}`，
  `protocol::BUILD` 是 commit id。`llm::build_head` 按 **散文 → index → 尾巴** 拼成一个
  `instructions` 值，顺序就是全部要点：越靠后越易变，前面才是共享前缀。新模块
  `clock.rs` 自己算日期（Hinnant `civil_from_days`），**分钟精度**——每秒变一次就等于每步
  重新计费整段 transcript。
- **顺手修的真 bug**：index 原来只在首轮建，之后原样复制 → 改了 skill 正文，该会话永远读
  旧文本。现在每回合重建，`prompt_in_force` 比对后再写行，所以只有真改才付那一次代价。
- **E2E 13 项全 ok**：首轮 index 旧正文 → 改正文 → 同会话下一回合索引到新正文且旧正文不
  再出现 → 磁盘上每个 raw meta 只剩一个 item → alpha/beta/gamma 三份独立 layout →
  gamma 在另一个目录，尾巴里是它自己的 cwd → 尾巴在 prompt 最后 → 三个会话开头的散文相同。
  单测 `a_layout_belongs_to_one_session`、`the_system_prompt_is_prose_then_index_then_this_turn`、
  `clock` 的日期与分钟。

## 第六轮：provider 按 item 切分缓存 → 动态内容单独成一条 message

User：「这说明 provider 可能以消息为单位切分 prefix cache，那么把动态的放到一条单独的
message 里注入」——判断正确，实测证实。

- **旧**：`dynamic_tail` 拼进 `instructions`（散文 → index → 尾巴一个字符串）。分钟一变，
  整段 10.3k 前缀作废：实测 98.7% → **1.2%**。
- **新**：尾巴是**最后一条 input item**（`RequestBody::own_facts`），写在 transcript 之后。
  同样场景：**98.2% → 99.3%**。
- 结论：缓存不是「字符串内部前缀」，而是「按 unit（这里是一个 `instructions` + input 数组）」
  逐项比对，**第一个不同的 unit 之后全部作废**。所以规则只有一条：**每回合都变的放最后，
  永不改变的放最前**；中间的 index 只在文件变化时变。
- `instructions` 现在只有散文 + index；尾巴仍记在 prompt 行的 `tail` 字段里（所以是有记录的），
  `scripts/replay_audit.py` 按「等于某行 tail」接受这条 item，与 handover note 同一套待遇。
- 单测：`body::the_last_item_is_written_after_the_transcript`（变异：把尾巴挪到 head 之后
  → 变红）；`llm::this_turns_facts_are_one_item_after_the_transcript`。
- 数据：`.dev/verify/ea40c7f-prefix-cache-tail-item.json`。��次测量里有一次第二请求只有
  1.8%，另两次 98.2–99.2%，判为 provider 侧冷读；场景与 sha 一变就要重测。
