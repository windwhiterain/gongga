---
name: computer_use
description: 用鼠标和键盘真的操作桌面——截图看屏、按控件树定位、点击/打字/按键，然后截图确认。任何只能通过图形界面完成的事（原生应用、装不了 CLI 的软件、拖拽、菜单）用它。Windows 已验证，macOS/Linux 后端已写但未实测。
---

# 操作桌面

`.agents/skills/computer_use/cu` 是一条命令，三个平台：`shot`、`tree`、`click`、
`type`、`key`、`focus`、`windows`、`doctor` 在哪儿都叫这个名字，只有后端换掉。

```bash
.agents/skills/computer_use/cu doctor      # 先确认这台机器能用
```

`cu` 优先跑 `cu.py`（uv 解析内联依赖，不需要你先装任何东西），Windows 上没有
uv/python 时回落到 `cu.ps1`。**跨平台的是 `cu.py` + 每个平台一个后端**，不是把
PowerShell 翻译成 Python 就完事——两者调的是不同的系统 API。

## 先看它能承诺什么

| 平台 | 后端 | 状态 |
|---|---|---|
| Windows | `cuwin.py`：ctypes + UIAutomation | **实测通过**（截图、控件树、点击、打字、组合键、焦点） |
| macOS | `cumac.py`：Quartz + Accessibility + `screencapture` | 未实测，按文档写的 |
| Linux (X11) | `culinux.py`：`xdotool`/`wmctrl`/ImageMagick | **实测通过**（Ubuntu 24.04 + openbox + Xvfb：8 条命令全跑通，中文字符串完整落地） |

**macOS 仍未实测**——`doctor` 会明说 `UNVERIFIED`，别把它的“能跑”当成已知事实：第一次用它时，先跑 `doctor`，再拿一件小事验证。

Linux 的实测环境是 WSL2 里的 Ubuntu 24.04 + Xvfb + openbox。**它验证了 X11 协议和工具链，不是每一台 Linux 桌面**：WSLg、GNOME、Wayland 各自还有自己的脾气。

Linux 还有一层分裂：X11 下这些工具直接驱动会话；**Wayland 按设计不允许客户端注入
输入或读别的窗口**，`click`/`type` 要 `ydotool`，控件树退化成窗口树。`doctor` 会报出
它认到的是哪儿。

## 主循环：看 → 定位 → 动 → 再看

```bash
cu shot --scale 0.5          # 打印 @C:\...png，harness 自动把图交给模型
cu tree --depth 5            # 控件名 + 屏幕矩形
cu click 1719 851 --window 47974752
cu type --window 47974752 "你好"
cu shot --window 47974752 --scale 0.5     # 确认真的发生了
```

`tree` 是关键：截图告诉你要点什么，控件树告诉你在哪个像素。

```
Button "Reopen file" @1581,823 277x56
Edit "地址栏" @364,83 1524x56
```

中心点是 `x + w/2, y + h/2`。按名字点比按像素猜稳，尤其界面会重排。

## 坐标：物理像素，同一空间

截图像素 == `tree` 坐标 == `click` 坐标。为此后端一进来就 `SetProcessDPIAware()`。

不做 DPI 感知的进程，`CopyFromScreen` 返回缩放后的桌面（1463x823），而 UIA 报物理
像素（2560x1440）——按 tree 的坐标点下去必偏。每次 `shot` 都会多打一行映射说明，
**这一行你能看到**（harness 把 stderr 并进工具输出，不是丢掉；TUI 里只是另外上色）：

```
captured 577x352 src=1922x1173@467,144 scale=0.3
         ^图大小  ^源矩形     ^源原点    ^缩放
```

算回屏幕坐标：`screen = src原点 + 图内坐标 / scale`。

**别小看原点。** 记事本窗口原点 (467,144)、scale 0.3，「Edit」菜单真实中心 (610,244)
本该落在图里 (42.8, 30)；忘掉原点会算成 (182.8, 73.2)——偏 140x43 px，点到别的东西。

## 截图为什么能直接变成图

`shot` 打印一个 `@C:\...\cu-xxxx.png`。harness 扫描**每个工具**的输出，按空白切词，
见到 `@` 开头的已知图片扩展名就：读文件 → 上传 Files API → 请求里作为
`input_image{file_id}` 发出。**不是 base64**，同一文件一轮只传一次。

- **路径不能有空格**——按空白切词，`@...\has space.png` 会被切成两半。`shot` 检测到
  空格会告警。
- 模型不支持图片时引用被跳过，日志有 warn——**打印了 `@` 不等于你看到了**。

实测确认过：把随机码画进图、再看图读出来，与封存答案一致。

## 给输入的每个命令都带 --window

`type` / `key` / `click` 会先把指定窗口拿到前台，**拿不到就报错拒绝**：

```
could not focus 198124 "SPEC.md - gongga"; refusing to send input blind
```

不是多余的小心。Windows 拒绝后台进程抢焦点时是**静默失败**，于是 `SendInput` 会把字
打到用户正在看的窗口。实测踩过：以为在给记事本打字，字进了 VS Code。现在用
`AttachThreadInput` 绕过限制，然后**验证** `GetForegroundWindow()` 真换了才继续。

不带 `--window` 时先打印当前前台是谁：`target: foreground hwnd=... "..."`。

## 安全边界

这是**用户的真实桌面**，不是沙箱：

- 打字前确认目标。要试打字，开一个自己的临时文件，别往用户正在编辑的文档里写。
- 关闭自己开的东西，别留垃圾窗口。
- 破坏性操作（删除、覆盖保存、安装）先截图确认按钮是什么。
- **截图会把屏幕上的秘密一起带上**：编辑器里的 API key、密码框、私信。截到的图会上传
  并进入对话记录，之后收不回。范围尽量用 `--window` 收窄，别整屏；图里有密钥就先切走。

## 命令

| 命令 | 作用 |
|---|---|
| `cu doctor` | 自检：DPI、控件树、INPUT 结构、截图 |
| `cu windows` | 顶层窗口：hwnd、pid、矩形、标题（`*` 是前台） |
| `cu shot [--window W] [--scale F] [--out P]` | 截图，打印 `@路径` |
| `cu tree [--window W] [--depth N] [--max N]` | 控件树：类型、名字、屏幕矩形 |
| `cu click X Y [--double] [--right] [--window W]` | 点击 |
| `cu type [--window W] <文本>` | 输入文本（Unicode，中文正常） |
| `cu key [--window W] ctrl+s` | 组合键 |
| `cu focus W` | 把窗口拿到前台 |

`W` 可写 hwnd（`47974752`）或标题片段（`"Notepad"`）；片段匹配多个时报错列候选。
`key` 支持 `ctrl` `shift` `alt` `win` `f1`–`f24` `enter` `tab` `esc` `space`
`backspace` `delete` `up/down/left/right` `home/end` `pageup/pagedown` `insert`
`printscreen` `capslock`，或单个字母，用 `+` 连。

## 踩过的坑（都别再踩回去）

| 现象 | 真因 | 现在怎么做 |
|---|---|---|
| UIA **在 root 上** `FindAll` 卡 **210 秒**，CPU 0 | UIA core 在等某个从不应答的顶层 provider。.NET 的 `UIAutomationClient` 自带 managed client-side provider，所以 PowerShell 里同样的调用 0.3s | 顶层窗口走 `EnumWindows`，UIA 只问**单个窗口**；`tree` 用递归 `FindAll(Children)` 而不是 root 查询 |
| `IUIAutomationTreeWalker` 调 `GetFirstChild` 报 `AttributeError` | comtypes 生成的模块把这个接口给成了无方法的裸指针，`QueryInterface` 也一样 | 不用 walker，递归 `FindAll(Children)` |
| `SendInput` 返回满数但字没进去（PowerShell 版） | PowerShell 不能给**嵌套结构体字段**赋值，写进副本后丢弃 | 输入搬进 C#；Python 版直接在 ctypes 里构造 `INPUT`，`sizeof` 自动是 40 |
| 同上，换布局才好 | x64 上 `INPUT` 的联合体必须从偏移 **8** 开始 | Windows 后端用 `_anonymous_` 联合体，`doctor` 断言 `sizeof(INPUT)` |
| `tree` 报 `Cannot convert "∞" to Int32` | 无矩形的控件 UIA 返回无穷大 | Python 版按 right/bottom 是否 0 判空，打印 `?` |
| `key enter` 报成功但没反应（Linux） | X11 的键名是 `Return`；`xdotool` 遇到未知键名时打印 `No such key name` 却仍然 exit 0 | `culinux.py` 显式映射友好名→keysym，未知键直接 ValueError，并检查 stderr |
| Windows 上多出「Windows 输入体验」之类幽灵窗口 | UWP 隐藏窗口，`EnumWindows` 看得见但点不着 | 用 `DwmGetWindowAttribute(DWMWA_CLOAKED)` 过滤 |
| `--window` 被 PowerShell 当自己的参数 | PS 参数解析套在脚本参数上 | PS 回落版走 `CU_ARGV` 环境变量（`\x1f` 分隔） |
| 这版 PowerShell 缺 `[ushort]`/`[uint]` 别名 | PS 5.1 的行为 | PS 回落版用全限定 `[System.UInt16]` |

## 一个完整例子

```bash
cu windows                        # 找到目标 hwnd
cu tree --window 12345 --depth 4  # 读 Edit 框和 Button 的矩形
cu click 700 400 --window 12345   # 点进输入框
cu type --window 12345 "fix: 折叠上下文"
cu click 1780 851 --window 12345  # 坐标来自 tree
cu shot --window 12345 --scale 0.5
```

## 先问一句：真的需要点界面吗

点界面是最慢最脆的一条路。**能用 API、CLI、配置文件完成的，就用那些。** 只有当那个
东西只有 GUI 时，才用这个 skill。
