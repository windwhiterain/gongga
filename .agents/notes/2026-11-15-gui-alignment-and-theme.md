# 2026-11-15 Avalonia GUI：与 TUI 对齐 + 视觉重做

用户判决：`frontends/avalonia` 太丑；先检查功能是否与 TUI 对齐，再把界面美化到 WebGUI 的
审美水准。分支 `feature/avalonia-gui`，worktree `.worktrees/gui`。

## 一、功能对齐审计（对着 `crates/client/src/render_wire.rs` 与 `crates/tui/src/`）

C# 的 `WireReader` 只认 17 个 `RenderMessage` 里的 14 个，下面四个是本轮实测/读码确认的缺口：

| 缺口 | 证据 | 后果 |
| --- | --- | --- |
| `older` 帧没解析 | `WireReader.Parse` 的 switch 里没有 `"older"`；而 `crates/client/src/main.rs:363` 的 `PipeSink::older_page` 走 `wire.prepend` → `RenderMessage::Older` | 翻页拿不到历史：C# 还在用「三秒窗口 + 行数」猜（`ClientModel.PageAt`），而 wire 早就把整页作为一帧送来了。TUI 在 `crates/tui/src/run.rs:120` 处理它 |
| `render` 帧没解析 | switch 里没有 `"render"`；`RenderMessage::Render{block_size, box_edge}`（`render_wire.rs:43`） | SPEC 的 `block_size`（块高上限、内部滚动）与 `box_edge`（开块两侧让给事件的列）在 GUI 里完全不生效 |
| `rewind` 帧没解析 | switch 里没有 `"rewind"`；TUI 在 `run.rs:172` → `log.rewind` 丢掉该窗口**未封口**的块 | 一次重放会把半截流式块留在屏上，直到永远 |
| `quota` 形状变了 | `Layout.quota` 已是 `Vec<QuotaRow>`（commit `8ec8a897`），C# 仍按 `Object` 读 | `active_quota` 整块永远不显示 |

另外三处与 SPEC/TUI 不一致（不是 wire 缺口，是 GUI 自己没做）：

- **会话列表没有树**。SPEC `Session name: path, rendered in hierarchy`，TUI 的 `tree.rs` 每个前缀是
  一行、组行有 `|` 折叠标记。GUI 是平铺列表，长名字里的 `/` 不表达任何结构。
- **Ctrl+Down 没绑**。SPEC hot-key：`Ctrl+Down: move sticky session to bottom`，TUI 在
  `lib.rs:1297` 的 `Arrow::Bottom`。
- **`output` 不是 markdown**。SPEC 写的是 `output: colored markdown`，TUI 有 `markdown.rs`
  （pulldown-cmark + 表格 + 折叠）。GUI 一律按纯文本画。

## 二、视觉重做（WebGUI 审美）

取舍（已定，不再问）：深色、低饱和、少边框、大留白、单一面板层级；语义色沿用 TUI 的
`LineKind → 颜色`映射（`render.rs:style_for` 是契约，不自己发明），但换成一套现代调色板；
UI 字用 Inter（`Avalonia.Fonts.Inter` 已在包引用里），代码/工具输出用等宽。

设计 token 全部进 `App.axaml` 的资源字典，XAML 里不再出现裸 hex（现在 `MainWindow.axaml`
里散着 20 多个十六进制值，这是"丑"的结构性原因）。

## 三、验证

- `dotnet build` 退出码；
- 临时 home（`.dev/gui-home`，本 worktree 内）+ 随机端口 + `gongga-tui.exe cli` 播种会话；
- `dotnet run -- --shots <dir>` 的 `facts.txt` / `tree.txt`：几何与帧是真进程、真 client 子进程。
  截图我读不了像素，所以判据是 facts 与 tree 里的数字，视觉判断留给用户。

## 四、本轮实测（`.dev/gui-shots2/facts.txt`，真 client 子进程 + 临时 home + 端口 55911）

`dotnet build` 退出 0。shot run 的 facts（只列与本次改动相关的）：

```
PASS sessions listed: 2
PASS one pane opened: 1
PASS capacity reported: {"intent":"room","windows":[["alpha",7],["beta",7]]}
PASS PageUp scrolls up: 323 -> 0
PASS an older page was asked for: {"intent":"older","windows":["beta"]}
PASS older rows land above the held ones: 1 -> 3 blocks
PASS two panes / two rows lit / header click moves focus: alpha
```

这一行是本轮修掉的病：`older` 意图发出去、`older` 帧回来、页落在已持有行**上面**（1 → 3 块）。

已知不通过 / 未观察：

- `history refilled: 1 blocks`：clone 来的 5 行 transcript 只画成 1 块。没查清，怀疑是初始 fill
  的多行被合成一个 stream 块，属于 `ClientModel.ApplyStream` 与 fill 的交互，本轮没动那块。
- `a finished turn is announced: no toast`：这一跑的真实回合超过 harness 的 20s 等待；下一行
  `the new row is in view` 是 PASS，说明行后来到了。
- `block_size` / `box_edge` 生效没有单独断言：帧解析与 `Dress` 已通，但没有一次 `set_render`
  的真帧观测。
- markdown 没做（SPEC 的 `output: colored markdown` 仍未对齐）。

## 2026-11-15 收口：pane 宽度差的那一列

交接时留的两条，一条结案、一条量清楚了：

- **宽度不再猜**：`NotifyColumns` 用 `SpanLine.Bounds.Width`（行自己拿到的框），
  不再减 `BlockPadding`/`2*box_edge`/bar——那三样都在行的框外面，减一次就少六列。
  实测同一 pane：55 → 61 列；同一 994 框里最宽行的墨迹 936 → 986px。
- **差的那一个字符是 `▏`**：Consolas 没有 U+258F，走 fallback 画成 7.9px 而它只占 7.2px 的列，
  于是恰好满行的引用行溢出 1 列、被 Avalonia 折成第二行（`w/rap` 那种孤字）。换成 `│`（U+2502，
  实测 7.2px）。marker 是 client 的选择，所以列宽算得准的前提是「画得出、且恰好一列」。
- **兜底**：`TextLayout.TextLines.Count > 1` 说明某行溢出，减一列；只在「这一轮没改宽度」时判，
  且只判字符数 ≤ 当前列数的行（否则读到的是上一轮的旧排版，会一路减到 20）；定下来的列数按
  pane+框宽记住，否则下一轮估计值又把它加回去，来回跳。
- **paging 那两条仍是 FAIL**，现在有数了：3 轮（13 blocks）能到顶并发出 `Older`，但 pane 本来就
  握着全部 13 块，没有更早的可landing；5 轮（19 blocks）六次 PageUp 只到 offset 833，没到顶。
  形状像是「client 第一页就拿了整份 transcript」，要改的是那个页大小或 harness 的步数。
