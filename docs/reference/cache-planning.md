# Cache planning: where the design came from

This is the design source for `crates/server/src/prefix_cache.rs`. It is written by the
repository's author, published 2026-11-02, and says why a harness should simulate a cache
tree at all, what a node is, how a node is chosen, and what happens when a worker edits a
document the tree holds.

Original: <https://zhuanlan.zhihu.com/p/2100642339169506573> — 白定, 2026-11-02 18:05
(四川). Quoted verbatim below; the mapping to this repository's code is the table after
it.

## The article, verbatim

在 SuperAgent 框架中，worker 是朝生暮死，大量产生又大量销毁的，如果每个 worker 都重新探
索一遍仓库，将会产生极大的未命中输入。

容易想到，将项目的文档固定到 worker 的系统提示词里，每个 worker 共享。但每个 worker 关注
的问题是不一样的，不同的模块需要不同的系统提示词，这不得不要求 SueperAgent 维护一个提示
词库，还要根据文件的修改不断更新，这对于目前缺乏优良 pro 模型的 deepseek 来说难度很大。

对于一个几乎凭借本能行事的 SuperAgent，它最习惯的事就是给每个 worker 一段提示词，那么能
否通过这段提示词找到在缓存命中和切题两点上最合适的提示词呢？这就是缓存规划要做的事情。

我们可以在 harness 层面模拟一颗缓存树，每个节点就是一段记忆，例如一个 skill，一个文档，
一张图片。SuperAgent 的提示词本身不长，但它会引导 agent 阅读相关的 skill，文档，worker
读到这些文档，又会去进一步的探索引用的其他 skill，文档。

我们将提示词引用的 skill，文档的闭包作为需要注入的提示词模块，在从模拟的缓存树中找到价格
最优的节点，用该节点对应的顺序合成系统提示词，同时启发式的更新缓存树，就完成了缓存规划。

然而 worker 往往会修改相关的文档，这导致缓存树中的节点失效的很快。我们可以给节点加上修改
忍耐功能，在 diff 低于一定比例的情况下保留该节点，在系统提示词最后再重放对应的 diff。

缓存规划要做到真正可用，还离不开对每家模型的缓存价格，缓存时间的适配，Gongga Harness 收
集了国内主流模型厂商的相关信息，将缓存规划做到了真正可用，欢迎前来体验:

<https://github.com/windwhiterain/gongga>

## Sentence by sentence, and where it lives

| the article | the code |
| --- | --- |
| "在 harness 层面模拟一颗缓存树" | `Layout`: `items` plus `nodes`, where a path from a root to a node is one ordering that really went out. One row in `prefix_layout`, keyed by the unit that shares bytes. |
| "每个节点就是一段记忆，例如一个 skill，一个文档，一张图片" | `Item`: the spelling the index prints (`@file:…`, `@skill:…`), the text, its tokens. Pictures are not items: they travel as attachments through the Files API, so an image is the one kind of memory the tree cannot hold today. |
| "将提示词引用的 skill，文档的闭包作为需要注入的提示词模块" | `meta::roots`: the closure of every reference the messages name, with the directory's `AGENTS.md` seeded, so a session is under its rules without naming them. |
| "从模拟的缓存树中找到价格最优的节点，用该节点对应的顺序合成系统提示词" | `Layout::plan`: a cost walk over the emitted chains, priced by the roster's hit/miss ratio; the cheapest chain's order, then what is new in the caller's order. |
| "给节点加上修改忍耐功能，在 diff 低于一定比例的情况下保留该节点，在系统提示词最后再重放对应的 diff" | `index_change_tolerance` and the `reference_revision` template: an entry whose change stays under the tolerance keeps its node, and the patch is appended after the index (`Plan::revisions`). |
| "对每家模型的缓存价格，缓存时间的适配" | `Rates`: the roster's `price` ratio, its `cache_ttl_secs` tier, and this home's measured `cache_hit_posterior`. See `docs/reference/prefix-cache.md` for what each vendor actually sells. |

## The one thing the article settles that an agent got wrong here

The tree is the harness's, and it is shared: workers are ephemeral, and the whole point of
simulating the tree is that a worker does not pay again for what another worker already
sent. The implementation once keyed the layout by session -- one forest per conversation --
which cannot choose anything (a session plans its prompt index exactly once, on an empty
forest) and contradicts the article outright. It is keyed by the unit that shares bytes
now: a session's directory. The reasoning and the measurements are in
`docs/notes/server-prefix-cache.md`.
