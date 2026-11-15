# 2026-11-15 markdown 为什么没画出来，以及该把它挪到 client

## 结论一：Avalonia 的 `Inlines` 是好的（有图，`.dev/inline-probe`）

`.dev/inline-probe`（一次性工程，跑完删掉）用 Headless + Skia 渲染四种写法并出 PNG：

| 写法 | 结果 |
| --- | --- |
| `TextBlock.Text = s` | 画出原文 |
| `Inlines.Add(run)` 逐个加 | 画出，粗体生效 |
| `Inlines = new InlineCollection{...}` 整份赋 | 画出，粗体生效 |
| `SelectableTextBlock` + `Inlines` | 画出，粗体生效 |
| 先设 `Text` 再赋 `Inlines` | 画出 runs（不残留旧 `Text`） |
| 先设 `Text`、`Text=null`、再赋 `Inlines` | 画出 runs |
| **`DocText`（复刻我的控件：`Source`+`Kind` 两个 StyledProperty、变更即重写、文档分支赋 `Inlines`）** | **画出，标题掉标记变粗体、bullet 变 `•`** |
| 同上，放进 `ItemsControl` + `DataTemplate` | 画出 |
| `kind != Assistant` 分支（`Text = Source`） | 原文，符合预期 |

所以先前四次“Inlines 不生效”的判断是错的：机制没问题，问题在窗口里那条路径。
已排除：属性变更回调是否触发（探针里 285 次重写、257 次 `kind=Assistant`）、
`Text` 与 `Inlines` 的先后顺序、`SelectableTextBlock` 与 `TextBlock` 的差别、DataTemplate 的包裹。

第二轮探针（`.dev/binding-probe`，同样一次性）把**编译绑定**也复刻了：`x:DataType` 的
`DataTemplate` + `{Binding Kind}` 把枚举喂给同一个自定义控件。结果分两半：

- 值确实到位——日志是 `kind=Assistant source=191 first="## Session notes …"`，所以编译绑定、
  DataTemplate、控件本身都不吃这个值；
- **但那个窗口整张空白**（连 `Text = Source` 的 User 块也没画）。这一版探针自身有 confound
  （XAML 建 Application + XAML 建控件），我这一轮没能把它和“XAML 建的控件不画 Inlines”分开。

所以结论只能落到这里：**不是 Avalonia 画不了 Inlines**（代码里建控件、赋一次 Inlines，有图为证）；
问题在“XAML 建控件 + 多次改 Text/Inlines”这条路上，且窗口里那块最终显示的是 `Text` 分支的原文。
继续在 GUI 里和它缠没有意义——按下面的形状做，控件只赋一次 Inlines（探针证实可画），
冲突面自然消失。

## 结论二：这件事不该由前端做

SPEC 的 `output: colored markdown` 与 TUI 的 `crates/tui/src/markdown.rs`（448 行 + `fold.rs` 131 行
+ `table.rs` 181 行）说明文档渲染是**一份逻辑**。GUI 再写一份，就是第二套换行、第二套表格宽度、
第二套代码块规则——两边一定会漂。

用户判决：**把逻辑挪到 client**，前端只画。

### 形状

1. `crates/client/src/document.rs`：markdown → `Vec<Row>`，`Row = Vec<DocumentSpan>`，
   `DocumentSpan { text: String, class: SpanClass }`，
   `SpanClass { Text, Strong, Emphasis, Strike, Code, Marker }`。
   换行在 client 完成（client 需要 `unicode-width`，与 `fold.rs` 同一套规则），所以行是**终态**。
2. 宽度从哪来：`UiIntent::Room` 现在说「每窗口能画几行」。加 `UiIntent::Columns { windows: Vec<(String, usize)> }`
   ——每个窗口能画几列，前端在 pane 尺寸变化时发一次（GUI 已有 `LayoutUpdated` 的 `NotifyRooms` 现成挂钩）。
   TUI 在 `render.rs` 里本来就知道 pane 宽度。
3. wire：`RenderMessage::Document { window, key, kind, tag, rows, done }`。client 按 key 保留答案的
   原文，宽度变化时重发行（与 `rewrite` 的语义相同）。`Line`/`Stream` 对非文档 kind 保持不变。
4. 前端：GUI 把 `rows` 画成「一行一个 `TextBlock` + 每 span 一个 `Run`」——**不需要换行、不需要解析**，
   也绕开了结论一里那层没定位的绑定；TUI 把 `rows` 直接变成 `ratatui::Line`，可以删掉自己的
   `markdown.rs`/`fold.rs`/`table.rs`。

### 代价与顺序

- 一次改四个 crate（protocol 不改；client + tui + 前端），`crates/client/src/lib.rs` 已 3.6k 行，
  文档状态要跟着答案原文走（目前原文只在 TUI 的 `Entry.source` 里）。
- 先落 client + wire + GUI（画面收益直接可见），再把 TUI 切过去并删掉它那三个文件。
  中间态是「两个渲染器并存」，要在同一条提交链里尽快消掉，不能长期留着。

## 落地：slice A（client + wire + GUI）已提交

- `crates/client/src/document.rs`：markdown → `Vec<Vec<DocumentSpan>>`，换行在 client 做完（`unicode-width`，行内空格不丢）。
- `RenderMessage::Document { window, key, kind, tag, rows, done }`；`UiIntent::Columns { windows }`。
- pipe daemon（`crates/client/src/main.rs`）按 key 存答案原文并发行；宽度变了重发全部行。
  TUI 仍然收 `Stream` 的文本、用自己那份渲染器（`run.rs` 里对新帧是 `{}`），所以这一片不改 TUI 行为。
- GUI：`SpanLine` 一行一个控件、构造时一次赋 runs；`MainWindow` 用真实的字形宽度量出列数（`_ruler`）并发 `Columns`。

### 实测（`.dev/gui-shots4`，真 client + 真 server + 临时 home）

两面板那张图：标题掉标记变粗、bullet 变 `•`、行内 `code` 有自己的底、围栏代码块独立成块、
空格不再丢。**但右侧被切约 2–3 个字符**：client 按 71 列排（窗口当时报 71），pane 实际只放得下 ~68。
原因已定位：`Columns` 只算了 pane 的 viewport 和 `box_edge`，没减 ScrollViewer 的 padding 与块自身的 padding。
已改成 `viewport − padding − 12 − 2*edge`，再留 3 列余量，得到的数字是 62（同宽 pane，曾在 wire 上看到）。
**这一版没有重新出图**：最后一次 shot run 的临时服务端已经退出，`connected` 直接是 FAIL，与改动无关。

### 还没做

- TUI 切到 `Document` 并删掉 `markdown.rs` / `fold.rs` / `table.rs`（slice B）。
- 62 列那一版的图要补一次。
- `.dev/verify/<sha>-document.json` 还没写。

### 补图（62 → 60 列）

- 62 列 + `NoWrap`：右边切掉约 2 字符。
- 62 列 + `Wrap`：不切了，但溢出 1 列时掉孤字（`i` / `n` 各占一行）。
- 60 列（余量 5 列）+ `Wrap`：段落干净折行、标题粗体、bullet、行内 code、代码块都对；
  仍有一处词被拆（`tha` / `t`），即估宽还差 1 列。

**结论**：余量是猜的，不该猜。下一步把 `Columns` 改成读**行自己控件的 `Bounds.Width`**
（布局后已知，不含任何 padding 假设），而不是 `viewport − 常数`。

### 试过的第二种量法（否定）

把 `Columns` 改成读 `viewer.Content.Bounds.Width`（以为那是行拿到的宽度）：
同宽 pane 上它给出 **64 列**，而 60 列才是真正放得下的——因为 `Content` 的宽度把
ScrollViewer 的 padding（32px ≈ 4.3 列）也算在内。图上是 `cargo / ru / n -p ...` 这种孤字。

所以现在用的是：`viewport − padding − 12 − 2·edge`，再按实测留 5 列余量（= 62 → 60）。
要彻底不猜，得拿 presenter **内部**的可用宽度（`ScrollContentPresenter` 的 content width），
不是 `Content` 也不是 `Viewport`。

## slice B 落地：TUI 收起自己的渲染器

- `Entry` 新增 `document: bool`：为真时 `layout()` 不再排版、`shown()` 直接给 `styled`、
  `rows()` 用 `styled.len()`。
- `Log::document(window, key, kind, tag, rows, done)`：块按 key 复用（与 `stream` 同一条建块路径 ——
  `Entry::with_tag` → `attach` → `open` 维护），行**整份替换**，因为文档是从原文派生的。
- `Panes::document` → `Tui::document` → `run.rs` 的 `RenderMessage::Document` 分支。
- 答案的 `Stream` 在 TUI 侧不再建块（`Tui::stream` 对 `Assistant` 早退）：同一个答案只来一次，
  否则文本块和文档块会各画一份。
- 删除 `markdown.rs` / `fold.rs` / `table.rs` 与它们的 `mod`：三者互相依赖、唯一外部引用是
  `log.rs` 的 `use crate::markdown` 和 `layout()` 里的 markdown 分支，都已随之删掉。

### 实测

- `cargo build -p gongga-tui` exit 0；`cargo test -p gongga-tui` **10 passed, 0 failed**。
- `python scripts/tui_live.py --scenario single-stream`（真 ConPTY + 假 LLM）：50 帧，
  答案正常折行、工具调用/结果照旧；`.dev/live/20261115-151648/frames.txt` 里
  `**` 与 `##` 的出现次数是 **0** —— 说明标记不是被隐藏，而是根本不再是文本。

### 第三种量法：读行的 `Bounds.Width`（采用）

行会拉伸到块的内容框，所以拿一个已经布局的行（`SpanLine`）的宽度，就是这一行真正拿到的宽度，
不需要再猜 padding 常数。同宽 pane 上它给出 **57 列**（之前算出来要 60 才放得下）。

效果：**不再切边、不再掉孤字**（安全的一侧），代价是右边留白约 3 列，且仍有一个长词在 pane 边缘被折断。
原因：我的 `Advance` 是用一个普通 `TextBlock` 量的，而行是 `SpanLine` 画的——两者字形不一致就会差一列。
下一步：用 `SpanLine` 自己量 advance（同一控件、同一字体），把最后那一列误差去掉。
