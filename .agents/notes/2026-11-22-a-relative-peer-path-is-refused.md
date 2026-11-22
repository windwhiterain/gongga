# 相对路径点名另一台机器：拒绝，并说出是哪两台

2026-11-22，用户转述 kv-mask 的话："read/write/edit 用 `remote://whiting_main/...` 路径无效"。

## 结论：说法不成立，但它撞上的是真事

`remote://<名字>/` 这个 spelling 一直是有效的。改之前我先用真调用量过：

- `read remote://local/SPEC.md` → 本机 SPEC.md，21068 字节；
- `read remote://whiting_main//home/whting/.bashrc` → **对端**的真文件，117 行
  （本机没有这个文件，所以确实是 ssh 过去读的）；
- `read remote://whiting_main/.bashrc cwd=/home/whting` → 同上。

坏的是**相对 tail 的目录**：`space_for` 从路径里取机器名（这一步对），但默认目录是
`session_cwd()` —— 本机 db 里那一行，对端空间也照用。于是

```
read remote://whiting_main/SPEC.md
→ could not open "C:/resource/gongga/SPEC.md" on the endpoint: No such file or directory
```

而那个文件**本机就有**（第一行证明过），所以这次读确实发到对端去了，只是带了一个
Windows 形状的路径。错误文本里既没有对端名字，又报成一条本机路径 + "the endpoint"，
读起来就是"这个路径不存在" —— 于是被记成"路径无效"。

顺手查了一下 kv-mask 到底试没试：home 的 `messages` 与 `tool_calls` 里，`kv-mask*`
全部会话中含 `remote://` 的行是 **0 条**。它是读代码推的。

## 同一处还有第二个 bug

`machine_of` 把"没点机器"和"点的是本机"都返回 `None`：

```python
if name in ("", "local", "none"):
    return (None, tail)          # 与 `return (None, text)` 撞在一起
```

所以 `remote://local/x` 在一个站在对端的 session 里，名字仍然是 `GONGGA_REMOTE`（用错机器），
而 tail 里还留着 `remote://` 前缀（解析出一条根本不存在的路径）。这是我第一轮验证里
唯一一条 FAIL 抓出来的——写测试表时把它写进去才看见。

## 改法（内容仓，`021ffa5`）

1. `Space.is_absolute`：一条"这个 spelling 下算不算整条"的规则，`resolve` 与判断共用。
2. `space_for`：机器不是 session 自己那台、且 tail 是相对路径、且没给 `cwd` → 拒绝，
   并把两台机器都点名：

   ```
   'SPEC.md' is relative and no `cwd` says where it is, so this call would look for it
   under 'C:\resource\gongga', the session's own directory, which is on this machine.
   A relative path is resolved by the machine that holds it, so a path on remote
   'whiting_main' has to be absolute in that machine's spelling, or `cwd` has to be a
   directory there.
   ```

   绝对路径不需要目录，`cwd` 是调用者自己的陈述，两者都不拒绝。
3. `machine_of`：本机是**空名字**而不是 `None`，于是"点本机"与"没点"可以区分。
   `image` 里 `if stated is None and ...` 那句判的正是"没点"，语义反而更准了。
4. `file` 的 SKILL.md 与三个 docstring：`ef14c08` 之后 `remote` 参数已经没有了，
   正文却还写着"Every function takes `remote`"、"naming a peer that is not the
   session's own is refused"——两句都不成立。按现行设计重写（路径带机器、任何已配置
   的对端都可达、对端的相对路径要 `cwd`）。

## 验证

`.dev/verify/021ffa5-file-remote-path.json`：17 条真调用（真 ssh 到 whiting_main、
真读 117 行），2.3s 全绿；`write`/`edit` 的拒绝发生在任何写入之前（SPEC.md 字节数
21068 不变）。门禁：`ship --check` 0、`skill_contract.sh` 0、`test-layout-check` 0、
`mbx fmt --check` 0、`mbx build --lib -p gongga-server` 0。

**`mbx build`（整棵）是 101，但不是编译错**：`failed to remove
target/debug/gongga-tui.exe: 拒绝访问 (os error 5)`——用户正在跑的 tui 握着那个 exe。
没有杀任何进程。`comments-check` 是 1，但这次改动一个 `.rs` 都没碰，那是树本来的
既有违例（`crates/tui/src/run.rs`、`crates/client/src/runtime.rs` 等）。

## 没做的

- 运行中的服务端要等下一次启动或每小时 pull 才装上：推完之后 `~/.gongga/ship`
  还停在 `f428664`（local）。所以"live 门已经变好"这句话现在不成立，下一条 agent 别拿
  旧行为当回归。
- 对端其它失败（权限、连不上）仍然不指名机器——那是 B 方案——**后来做了，见下节 `4e65e51`**。

## 追加：B 也做了（`4e65e51`）

用户随后说"加"。于是 `_Connection` 收下对端的名字，**它自己抛的每一句**（greeting、
`_send`、`_frame`、id 不对、runner 自己的 `err`）都带上 `remote '<名字>': ` 前缀。
一处就够：对端的所有流量都走这个类，没有第二个地方要写这条规则。

本机的失败**不加**前缀：那里路径就是本机的，没有歧义。

```
remote 'whiting_main': could not open "/home/whting/nope.txt" on the endpoint: No such file or directory (os error 2)
remote 'whiting_main': could not open "/etc/shadow" on the endpoint: Permission denied (os error 13)
remote 'whiting_main': could not start no-such-program-xyz on the endpoint: No such file or directory (os error 2)
remote 'ghost': could not start the runner: [WinError 2] 系统找不到指定的文件。
remote 'mute': the runner closed without answering
```

`.dev/verify/4e65e51-peer-refusal-names-the-peer.json`：8/8 PASS，真对端 117 行、真 EACCES、
真"程序不在"；`ghost`/`mute` 两个假对端走 `.dev/` 里的临时 catalog（跑完删掉，0 残留）。
门禁：`ship --check` 0、`skill_contract.sh` 0；Rust 这一轮同样一个文件都没碰。
