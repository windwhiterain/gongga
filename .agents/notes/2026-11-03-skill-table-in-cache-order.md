# skill 表格按「前缀缓存最便宜的顺序」发出（WIP）

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
