# 事件 UI：一个数字限高，一个开关分屏（WIP）

SPEC 把 `set_event_detail --mode all|line` 换成了 `set_event_ui [--block_size <line>]`
（「max line count of a block, auto scroll」），`set_event_split --split <bool>` 那句
「split events window by session name, cmd/cmd_result use a separate single line」留着不动。
代码里两套语义都还在（protocol 的目录、cli 的 `Detail`、tui 的 `folds`/`restart`/`sticky`），
所以这一轮是把它们换成 SPEC 的形状，并按新的分屏语义真做分屏。

## 用户定的四件事（问出来的，不是推出来的）

- **超高怎么办**：跟随尾部，头部丢弃。块最多占 `block_size` 行，超出的行不进窗口、
  也不能往回滚——全文在库里，用 `get_messages` 读。旧 `line` 模式「固定头部」的那条
  不变量（`streaming_a_folded_row_never_moves_its_start`）被明确推翻，随之推翻了它带来的
  `FOLD_KEEP=4096` 字节上限（AGENTS.md：setting any limit/budget is forbidden）。
- **谁不受限**：助手的回答。其余（thinking / tool call / 工具输出 / cmd 回显）都按
  `block_size` 收。这条例外是从旧 `folds()` 继承的，SPEC 新措辞里没写，靠判断留下。
- **分屏是真分屏**：events 区按会话名切成独立面板，各自滚动、各自贴自己的最新行——
  不是旧实现里「一个会话共用一个块」那种假分屏。
- **焦点**：滚轮在哪个面板就滚哪个并把它设为焦点；键盘（PgUp/PgDn、Shift+↑↓、Alt+↑↓）
  只滚焦点面板。`cmd`/`cmd_result` 走 footer 与输入框之间单独一行。
- `set_event_ui` 不带参数 = **打印当前值**（只读），`--block_size 0` 才是取消上限。
- 体验流程放 `scripts/`（跟踪），不放 `.dev/`、不做成 skill。

## 分层（谁决定什么）

- **哪些行进哪个窗口**是语义 → `cli` 决定：`Client::session_window` / `command_window`，
  并在每次 `refresh` 用 `Sink::windows(&[Window])` 公布面板清单。会话名恰好叫 `cmd` 时
  命令行窗口改名成 `"cmd "`，不让会话把它占了（有测试）。
- **一个窗口画几行、一块画几行**是绘制 → `tui`：`Panes`（清单顺序 + 每个窗口自己的
  `Log`/`scroll`/`max_scroll`），`Log` 只管「一块最多几行、显示最新几行」。
- 于是 `cli` 不再需要 `s:<session>` 这种分组 key（分屏本身就隔开了会话），`tui` 也不再需要
  `set_split` 的 sticky-rewrite hack；`Sink::split(bool)` 换成了 `Sink::windows(...)`。
- 收回分屏时面板进 `retired` 而不是丢弃：换的是视图，不是把 scrollback 扔了。

## 帧里查出来的三件事，都改了

体验流程是 `scripts/tui_live.py`（ConPTY + 自写网格 + 帧统计）+ `computer_use` 截图。
先按上面那版实现跑，帧给出三个结论：

- **F1 分屏把历史挡在屏外**：切到 split 后每个面板只有「切换之后」的行，正在流式的那条从半句
  话接起来（首行 `[peer]  what this run counts.`）。根因是「每个面板一份 Log」。改成
  **一份日志、多个视图**（`Ledger` + `View::{All, Window}`）：行只存一次，切换只改这一帧读哪
  个视图。代价是一个块被两个视图各计一次行数（两次 O(log n) 更新），换来的是切换不丢历史、
  合并视图仍保留跨会话到达顺序。
- **F2 面板高度**：你定「维持均分」。截图确认代价：刚连上、两个会话都没内容时，屏幕是两条表头
  加一大片空。
- **F3 命令行那一行吃掉 `get_messages` 的多行回执**：帧里只剩 `2 row(s) at seq 1..2`。按你的规
  则改成「命令带 `--session` 就归那个会话的面板」，回显 / 读回来的行 / 回执一起过去；那一线只
  留没点会话名的命令。`CmdResult` 只有 id，所以 echo 到达时就把窗口记进 `Client::results`，
  断线时清空。

顺带抓到并修掉两处：块被**窗口**（不是 cap）切到段落中间时还被硬贴标签；头部那条青色只覆盖文
字宽度，在真窗口里读起来像选中而不是标题栏。

渲染本身是健康的：峰值 21 帧/秒、字节到达→出帧最大间隔 46ms、逐键只改 1–2 格、整屏清空只有
启动那两次、row-loss 从 4 降到 2。

## 验证状态

- `cargo test --workspace` 全绿（cli 77、protocol 20+6、tui 70）。
- `comments-check`、`test-layout-check` 全过；`cargo clippy -p gongga-cli -p gongga-tui` 干净
  （server 那两条既有警告与本分支无关）。
- 帧抓取流程：`scripts/tui_live.py`（ConPTY + 自写网格）在写，随后用它体验并继续优化。
- docs/notes 同步**还没做**：`tui-log.md` 里 fold 那几节、`cli-lib.md` 的 `Sink::detail`、
  `cli-commands.md` 的 Detail/Split 都还是旧的；等渲染结论定了再一次性改，免得改两遍。
