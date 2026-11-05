# 列表向下长、会话行只报自己、被叫停的一轮不是"结束"

用户 2026-11-05 的三条裁定（原文三条抱怨，逐条落到代码）：

1. **"TUI 左侧的 session 栏改为向下增长，与 session 消息同构"** —— 再问一次后用户的原话是
   **"新的 session 放下面"**。所以要的就是顺序：会话列表和消息流一样从旧到新往下排，
   新建的会话出现在最下面。`store::list_sessions` 是 `created_at DESC`（最新在前），
   `Client::layout` 现在 `.rev()` 两次（`listed` 和 `shown`），旧的在上面/左边、新的在
   下面/右边。
2. **"session 在 render = true 时 idle 仍然显示为黄色，应当为绿色"** —— 一个 bug，两件事：
   - 黄色 = `Mark::Stopped` = "没画出来的时候结束了"（`unseen`），而 `tree::rows` 让**每一行**
     都取整棵子树里最强的状态，于是"既是会话又是组"的那一行（例如 `live_compact` 显示、
     `live_compact/remote_review` 没显示）会因为子会话停过而变黄。改成：`session` 行只报
     `own`，只有纯组行报 `under`。
   - idle 的颜色从暗灰 `○` 改成绿色 `○`：形状(= 是否在跑)与颜色(= 要多注意)分开承担，
     绿=没事、黄=没人看时结束了、红=出错。`Stopped` 同时改成空心，因为那一轮也没在跑。
3. **"cancel 不应当发起 notice/变黄色"** —— `cancel`（以及 `interrupt_session`、关窗口时窗口
   自己发的 cancel）让每个回合以 `TurnOutcome::Cancelled` 结束。原来 `cli` 只要状态不是
   `running` 就调 `Sink::stopped`，于是每个会话一条桌面 toast、没画出来的一律标黄。现在
   `Event::Status` 带 `outcome: Option<TurnOutcome>`（`done`/`cancelled`/`failed`），
   `Cancelled` 不发给 `Sink::stopped`：用户自己要的停止不需要通知，也不是"没看见就结束了"。
   `detail` 仍然是人读的句子（`idle (cancelled)`），程序不再解析它。

## 改了什么

- `protocol`：新增 `TurnOutcome`；`Event::Status` 加 `outcome`（`serde(default)`）。服务端
  自己的私有 `TurnOutcome` 删掉，两处同一个类型；`turn_end` 那条日志行直接序列化它。
- `cli`：`layout` 反序；`handle_event(Status)` 过滤 `Cancelled`。
- `tui`：`tree::rows` 拆 `own`/`under`；`render::status_mark` 上表的颜色；`Tui::stopped` 的
  文档从 `settled` 挪回自己头上（上一轮挪错了）。
- `scripts/tui_live_scenarios.py`：`session-tree` 的点击行号按新顺序改（lab 从第 0 行到第 3 行、
  work 从第 2 行到第 0 行、lab/x 从第 1 行到第 4 行）。

## 验证（真帧，`.dev/verify/` 里有记录）

- `session-tree`：帧里列表是 `work / api / web / lab / [x]`（旧到新，新的在下面），
  每个 idle 行的属性是 `2xG`（绿），不是旧的 `k`。
- 被 cancel 的一轮：会话窗口隐藏 → `/cancel` → 列表行仍是 `2xG`；把 cli 那行过滤撤掉再跑，
  同一帧变成 `2xY`（黄）。这是 L2 变异检查，两次都跑在真进程、真服务端上。
- 门禁：`cargo build`、`cargo fmt --check`、`cargo test --workspace`（8 通过）、
  `comments-check`（0；顺带修掉上一轮留在同一个文件里的两处超预算注释）、
  `test-layout-check`（0）、`scripts/skill_contract.sh`（PASS）、
  `scripts/ship.sh --check --local`（0）。

## 留下的

- `set_render_session --show false --session X` 之后，只要**下一条命令点到了 X**
  （`interrupt_session X` 也算），`follow()` 会把 X 重新设成显示。这是"命令点名的会话就画"的
  老规则，不是这次改的；测试脚本里因此改用 `/cancel`（不点名）来观察隐藏会话的标记。
- `SPEC.md` 没动（工作区里那份改动不是这次做的）：SPEC 的 UI 一节没有写列表顺序、也没有写
  标记颜色，这两条现在只活在代码与 `docs/notes/`。
