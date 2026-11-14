# 端口先答，网络在后面排队

用户报「重启后连不上，其实连上了但要等十几秒」。量下来不是连接的问题，是服务端在
`listening` 之前把三件网络活全做完了，而且客户端那个 6 秒的快速轮询窗口正好在服务端
准备好之前用完。

## 断在哪

启动顺序里 `run()` 是串行的，`bind` 在最后。之前这四件事排在 `bind` 前面：

| 步骤 | 走什么 | 实测 |
| --- | --- | --- |
| 模型发现脚本（`opencode-go` 的 `GET /models`） | 网络 | 2.2s |
| `ship::resolve` → `fork.sync()` 的 `git fetch` | 网络 | 1.2–2.1s |
| 社区克隆 `fork.sync()` | 网络 | 1.2s |
| `ship::install` 的本地 reconcile | 磁盘 | 1.3s |

从进程创建到 `listening`：**10.3s**。然后客户端：`SPAWN_ATTEMPTS=120 × SPAWN_POLL=50ms`
= 6 秒的快速轮询，用完就掉进 `250ms → 5s` 的退避阶梯，于是 `listening` 之后又空了 4.4s。

顺带纠正一个我一开始读错的点：`the models a script names are in the roster` 那条日志
出现在启动中段，但它来自**后台线程**，不是它卡住主线程。真正卡主线程的是 ship 的 fetch。
唯一的例外是配置里那个模型的 provider/名字在文件 roster 里放不下——那时 config 加载会
`settle_scripts()` 去 join 脚本线程，那是故意等的（否则这个 home 没有可用的默认模型）。

## 改了什么

1. `ship::resolve(home, online)`：启动只读 checkout，**不 fetch**。缺 checkout 的那一次
   仍然克隆（那一次没有别的办法）。
2. `ship::refresh(home)`：新的后台条目，`fork.sync()` 之后只回答「仓库现在的 commit 是
   哪个」，**不安装**——在跑的回合下面重写 skills 是工具可能正在读的文件，而且 transcript
   会和它构建时依赖的根对不上。挪到的那个 commit 由下一次启动来装。
3. `lib.rs`：`agents::start()`（社区同步）和 `ship::refresh` 都移到 `bind` 成功之后，
   三件网络活（加上 roster 读时就已经在后台的发现脚本）从此并行、且在端口后面。
4. 发现脚本本来就不阻塞（`spawn_scripts` 是线程），除了上面那个「文件放不下这个模型」的
   例外，没有改动。

## 实测（同一份 home 的副本：233MB db、同样的 ship checkout 和 `.ship-base`）

```
listening                                     +1.81s   （改前 10.3s）
ship refresh 回来（content: this home holds…）  +4.67s   在端口后面
社区同步回来                                    +4.29s   在端口后面

start step: config 79ms · home lock 2ms · ship prepare 239ms · migrate 0
            ship install 1285ms · prompts 63ms · store 41ms · scripts 57ms
            repair 37ms · sweep 19ms
```

剩下最贵的一步是 `ship install` 1.3s——那是本地 reconcile，不是网络，也没在用户这次要求的
范围里。要做的话得先看它为什么每次都要合并（`conflicts=["config/config.toml"]` 每次都在报）。

## 客户端那一半

`SPAWN_ATTEMPTS=120 × 50ms` 的窗口是「猜 6 秒」。服务端修好之后冷启动 1.8s 落在窗口内，
所以这次不用改；但任何超过 6 秒的启动（首装克隆、慢盘）还会照旧多等 ~4s。真要根治，
判据应该跟着子进程的命走：pid 活着就一直 50ms 轮询，死了才退避。

## 验证方式

`target/debug/gongga-server.exe` 在一份整 home 副本上跑（自己的 `--home`、内核分配端口），
读它自己的 `start step` 调试行和 `listening` 的相对时间。测完 kill 掉 pid（从
`server.lock` 读，不是 `$!`）并删掉副本。
