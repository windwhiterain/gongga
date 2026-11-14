# 2026-11-14 home 直接读 checkout，不再装拷贝

## 问题（已取证）

`<home>/config.toml` 不是内容仓的那份，而是 `.ship-base` + `decide(ours, base, theirs)`
三方合并的产物。base 从旧 pin 来，ours 相对 base 没有任何新增，18 行差异全是内容仓也删掉
的旧字段——本质是快进，被判成冲突 → 保留 ours、base 不前进 → 每次启动重复同一场冲突。
后果：这个 home 早就不再接收内容仓对 config 的任何更新。

## 用户判决

「home 不再存拷贝，直接读 checkout 里的那份」。config 读 `<home>/ship/config/config.toml`，
skill 读 `<home>/ship/skills/<name>`，`GONGGA_SKILL_LIB` 指 `<home>/ship/python`。
`.ship-base` / `seed_base` / `decide` / 清单 / 删除记账全部消失。
硬约束：Windows 上不靠 symlink/junction；缺 config/skills 时大声拒起并点名路径。

## 决定

- 没有 install pass。`prepare` = adopt → clone（没有 checkout 时）→ resolve → settle →
  检查三条路径 → tidy。`inspect` 是同一次读，但不写（`--check-config` 用）。
- `settle`：正常启动 checkout 已经站在那个 commit 上，一行 git 都不跑；只有在内容"跑到了
  这个 build 读不懂的 protocol"时，walk-back 点名的旧 commit 才需要 git 把工作树移过去
  （有分支就上分支，没有就 detached）。**保留 walk-back，即 SPEC `Git Managed Config` 的
  auto-choose-latest-within-protocol**；否则内容一提 protocol，所有没升级的机器立刻起不来。
  工作树里有会挡路的改动时拒绝启动，不替你丢。
- `[ship] url` 还在，但它不能是读 config 之前就该知道的答案（config 在 checkout 里），
  所以它在 `follow` 里生效：指 origin、fetch、`refresh`。同一次启动内落地。
- tidy 的判据是**字节**：内容自己的那份 → 删；不一样 → 留，并在日志里点名。名字内容从没
  发过 → 根本不看。`.ship-base` / `.ship-pin` 无条件删。`retired_python::retire` 挪进 tidy
  最前面（那三个名字是老 build 的字节，内容仓里没有可比的）。
- `prompts::install` 不再从 git 取"shipped defaults"：同一个文件既是默认也是覆盖。
- `gongga-cli` shim 里的 `$home/lib/gongga_cli.py` 改成按 `ship::python(home)` 算出来的相对
  路径。

## live track 的边界（写进报告）

- config：`Current::now()` 每请求比 mtime/size，改 checkout 里的文件下一次请求就换；锚定值
  （home/db/bind/key/scripts dir/config 路径本身）仍是启动时读一次。
- skill：`ls_skill` / `run_skill` 每次调用走目录 → 加一个 skill、改一个 description 立即生效。
- python surface：环境变量指向目录，进程下次启动就是新的。
- 内容移动：`refresh` 在端口后面跑，resolve + settle，不用重启。

## 代价

home 不再自包含：`<home>/ship` 没了就起不来（`inspect` 明说没 checkout）。这是用户接受的
代价，写进报告。

## 遗留

`scripts/spawn_dir_selftest.sh` 4/5 两条断言失败（对端 session 无目录的场景），没跑 main 的
基线，归属待定；`scripts/cmd_selftest.sh` 里 5 条依赖 `http://127.0.0.1:1` 的假 provider，
本来就答不出。见 `.dev/verify/90e67cb0-one-writer.json`。
