---
name: computer_use
description: 用鼠标和键盘真的操作这台 Windows 桌面——截图看屏、按 UIA 控件树定位、点击/打字/按键，然后截图确认。任何只能通过图形界面完成的事（原生应用、装不了 CLI 的软件、拖拽、菜单）用它。
---

# 操作桌面

`.agents/skills/computer_use/cu` 是一个 PowerShell 脚本，通过 .NET UIAutomation 和
SendInput 驱动真实桌面。**零依赖**：不需要 pyautogui、pywinauto、AutoHotkey。

先确认这台机器能用：

```bash
.agents/skills/computer_use/cu doctor
```

它检查 DPI 感知、UIAutomation、截图是否非纯色、`INPUT` 结构体大小。**`verdict: usable`
才往下走**——锁屏、无桌面的会话会在这里就失败，而不是在后面莫名其妙地没反应。

## 主循环：看 → 定位 → 动 → 再看

```bash
# 1. 看。这一行会打印 @C:\...\shot.png，harness 自动把图交给模型。
.agents/skills/computer_use/cu shot --scale 0.5

# 2. 定位。别靠眼睛估坐标，问控件树要精确矩形。
.agents/skills/computer_use/cu tree --depth 5

# 3. 动。--window 同时把窗口拿到前台。
.agents/skills/computer_use/cu click 1719 851 --window 47974752
.agents/skills/computer_use/cu type --window 47974752 "你好"

# 4. 再看，确认真的发生了。
.agents/skills/computer_use/cu shot --window 47974752 --scale 0.5
```

**第 2 步是关键。** 截图告诉你要点什么，控件树告诉你在哪个像素——`tree` 输出形如：

```
Button "Reopen file" @1581,823 277x56
Edit "地址栏" @364,83 1524x56
```

中心点就是 `x + w/2, y + h/2`。按名字点比按像素猜稳得多，尤其界面会重排的时候。

## 坐标是物理像素，两者同一空间

截图里看到的像素 == `tree` 报的坐标 == `click` 吃进去的坐标。为此脚本每个命令开头都
`SetProcessDPIAware()`。

**这是实测踩过的坑**：不做 DPI 感知的进程，`CopyFromScreen` 返回缩放后的桌面
（1463x823），而 UIA 报物理像素（2560x1440）——按 tree 的坐标点下去会偏到别处。
`cu shot` 的 stderr 会写 `screen=2560x1440`，用 `--scale` 缩小后记得**按比例除回去**：

```
scale 0.5 时图里 (400,300) -> 真实 (800,600)
```

## 给输入的每个命令都带 --window

`type` / `key` / `click` 会先把你指定的窗口拿到前台，**拿不到就报错拒绝执行**：

```
could not focus 198124 "SPEC.md - gongga"; refusing to send input blind
```

这不是多余的小心。Windows 默认拒绝后台进程抢焦点（`SetForegroundWindow` 静默返回
false），于是 `SendInput` 会把字打到**用户正在看的那个窗口**里。实测过一次：以为在给
记事本打字，字进了 VS Code。脚本用 `AttachThreadInput` 绕过这个限制，然后**验证**
`GetForegroundWindow()` 真的换过来了才继续。

不带 `--window` 时它会打印当前前台是谁，让你先看一眼：

```
target: foreground hwnd=198124 "SPEC.md - gongga - Visual Studio Code"
```

## 安全边界

这是**用户的真实桌面**，不是沙箱：

- 打字前确认目标。别往用户正在编辑的文档里写东西——要试打字，开一个自己的临时文件。
- 关闭自己开的东西，别留下垃圾窗口。
- 破坏性的界面操作（删除、覆盖保存、安装）先截图确认按钮是什么，再点。
- 界面在动的时候（动画、加载中）等一拍再截图，`Start-Sleep` 就够。

## 命令

| 命令 | 作用 |
|---|---|
| `cu doctor` | 自检环境能不能用 |
| `cu windows` | 顶层窗口：hwnd、pid、矩形、标题（`*` 是前台） |
| `cu shot [--window W] [--scale F] [--out P]` | 截图，打印 `@路径` |
| `cu tree [--window W] [--depth N] [--max N]` | UIA 控件树：类型、名字、屏幕矩形 |
| `cu click X Y [--double] [--right] [--window W]` | 点击 |
| `cu type [--window W] <文本>` | 输入文本（Unicode，中文正常） |
| `cu key [--window W] ctrl+s` | 组合键 |
| `cu focus W` | 把窗口拿到前台 |

`W` 可以写 hwnd（`47974752`），也可以写标题片段（`"Notepad"`）。片段匹配到多个窗口时
会报错并列出候选，不会瞎猜。

`key` 支持：`ctrl` `shift` `alt` `win` `f1`–`f24` `enter` `tab` `esc` `space`
`backspace` `delete` `up/down/left/right` `home/end` `pageup/pagedown` `insert`
`printscreen` `capslock`，或单个字母。用 `+` 连：`ctrl+shift+s`。

## 这条链路的实测坑（都已处理，别再踩回去）

| 现象 | 原因 | 现在怎么处理 |
|---|---|---|
| 字打进去没反应，`SendInput` 返回满数 | PowerShell 不能给**嵌套结构体字段**赋值：`$i.u.ki.wVk = x` 写进副本后丢弃 | 输入构造搬进 C# 的 `TypeText`/`SendVks` |
| 同上，且换了 `INPUT` 布局才好 | x64 上联合体必须从偏移 **8** 开始（4 字节 type + 4 字节对齐），写成 4 会静默丢弃 | `INPUT` 用 Sequential 包一个 Explicit `INPUTUNION` |
| `tree` 报 `Cannot convert "∞" to Int32` | 无屏幕矩形的控件，UIA 返回无穷大 | 打印成 `?` |
| `cu.p1 : A parameter cannot be found ... '-window'` | PowerShell 把自己的参数解析套在 `--window` 上 | 参数走 `CU_ARGV` 环境变量，用 `\x1f` 分隔 |
| `Unable to find type [ushort]/[uint]` | 这版 PowerShell 5.1 缺无符号整数别名 | 用全限定 `[System.UInt16]`/`[System.UInt32]` |
| 窗口在 `-32000,-32000` | 最小化窗口的坐标 | 先 `cu focus`；截图会明确报 "is minimized" |

`SendKeys`（`WScript.Shell`）能打 ASCII，但会吞掉 `{}~+^%()` 这些字符，而且不进
Unicode 输入法。**用 `cu type`**，别退回 SendKeys。

## 一个完整例子：把 git 提交信息填进 GUI

```bash
cu windows                                  # 找到目标 hwnd
cu shot --window 12345 --scale 0.5          # 看一眼，知道大概布局
cu tree --window 12345 --depth 4            # 找到 Edit 框和 Button 的矩形
cu click 700 400 --window 12345             # 点进输入框
cu type --window 12345 "fix: 折叠上下文"     # 打字
cu click 1780 851 --window 12345            # 点按钮（坐标来自 tree）
cu shot --window 12345 --scale 0.5          # 确认结果
```

## 先问一句：真的需要点界面吗

点界面是最慢、最脆的一条路。**能用 API、CLI、配置文件完成的事，就用那些。** 只有当
那个东西只有 GUI 时（原生应用、专有软件、必须拖拽/看渲染结果），才用这个 skill。

## 打印了图 ≠ 模型看到了

和 `image` skill 一样：`cu shot` 打印了 `@路径`，Harness 会去上传并附着，但**如果模型
不支持图片 modality，引用会被跳过**（服务端日志有 warn）。要确认自己真的看到了，就在下一
步描述图里的内容——描述得出来才算数。`cu doctor` 也会告诉你截图是否非纯色。
