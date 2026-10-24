---
name: proxy
description: 让某台机器的某个 host 走指定的上游（HTTP/SOCKS5/直连），并证明它真的走了；同时直接驱动跑着的 mihomo/clash core——列策略组、换节点、测延迟和吞吐、切模式、热重载。任何"这个 API 连不上/到底有没有走代理/怎么只让某个域名走代理/换哪个节点/哪个节点快"的问题用它。
---

# 按 host 路由，并证明它真的走了

`.agents/skills/proxy/proxy` 是一个**自己实现的路由代理**：纯 Python 标准库，按规则把
每个连接送到指定上游，并把每条决定记下来。没有第三方包，不需要装任何东西。

```bash
.agents/skills/proxy/proxy doctor      # 先看这条链每一环
```

## 为什么不是「配个代理」

两件事会让通常的做法失效，而且都会让人查错方向：

1. **按规则路由的代理对未列出的 host 是直连的。** 所以「请求失败」证明不了代理坏了，
   「请求成功」也证明不了它走了代理。
2. **`HTTP_PROXY` 环境变量不是证据。** curl 和 Node 认，Go 和不少 Python 库要别的名字，
   而按规则路由的代理仍可能把某个 host 送直连。

所以这里的判定单位是**每条连接的真实去向**：路由是代理自己决定的，它把决定写进
`~/.proxy/decisions.jsonl`，`check` 读的是这个记录，不是猜。

## 接一个上游（用户提供任何 API）

```bash
# HTTP 代理（最常见）
.agents/skills/proxy/proxy upstream add corp 127.0.0.1:8080 --kind http
.agents/skills/proxy/proxy upstream add corp 127.0.0.1:8080 --kind http --auth user:pass

# SOCKS5
.agents/skills/proxy/proxy upstream add tun 127.0.0.1:1080 --kind socks5

# 另一个按流量计费的服务，一个上游一个名字即可，规则里按名字选
.agents/skills/proxy/proxy upstream add premium api.example.com:3128 --kind http
```

然后决定谁走它：

```bash
.agents/skills/proxy/proxy add github.com       --via corp          # 后缀匹配（含子域）
.agents/skills/proxy/proxy add api.openai.com   --via premium --type exact
.agents/skills/proxy/proxy add 10.0.0.0/8       --via direct  --type cidr
```

## 三条命令的主线

```bash
# 1. 改配置后重启，规则在启动时读入
.agents/skills/proxy/proxy serve                 # 前台；要常驻用 detached bash

# 2. 让客户端指向它
.agents/skills/proxy/proxy env                   # 打印 export，含 git 那行
export HTTPS_PROXY=http://127.0.0.1:7899
export NO_PROXY=localhost,127.0.0.1,::1

# 3. 证明这个 host 真的走了你指定的上游
.agents/skills/proxy/proxy check github.com
```

`check` 的输出形如：

```
host              github.com
rule              suffix github.com -> relay
upstream          relay (http)
proxy listening   yes
request           HTTP 200
proxy decision    github.com via relay

verdict           ROUTED VIA relay
```

`proxy decision` 那一行来自代理自己：它记录了这条连接选了哪个上游、命中哪条规则。
`verdict` 只有这几种：`ROUTED VIA <name>` / `DIRECT` / `ROUTE FAILED` / `REQUEST FAILED` /
`NOT RUNNING`。

## 两层：分流是一个东西，出口是另一个

`proxy` 自己管**这台机器上按 host 分流**。真正把流量送出去的上游，通常是一个 mihomo
（Clash.Meta）core：它带一份节点目录、一堆策略组（GitHub 走哪、YouTube 走哪），还有一个
运行时可读写的 HTTP API。

分开看这两层，否则会问错问题：

| 层 | 谁决定 | 改它用什么 |
|---|---|---|
| 分流：这个 host 走不走代理 | `proxy.py` 的规则，或 core 自己的 rules | `proxy add …` / `proxy core mode` |
| 出口：走哪个节点 | core 的策略组 | `proxy core use …` |

**换节点、bench 节点，全在 core 的 API 上，不需要任何客户端。** 这一点值得单独说：任何
CLI（这台机器上是 `flm`）都只是这个 API 的一层壳。壳没了，能力还在。

## 找到控制器（"怎么配 API"）

core 的 API 叫 external-controller：一个 HTTP 端点 + 一个 secret。它们写在 core 自己的配置里：

```yaml
external-controller: 127.0.0.1:9090
secret: "…"
mixed-port: 7897          # 客户端要指向的端口——不是 API 端口
```

```bash
.agents/skills/proxy/proxy core doctor
```

`doctor` 按这个顺序找，并且**打印它用的是哪一条**：

1. `--controller` / `--secret`
2. `MIHOMO_CONTROLLER` / `MIHOMO_SECRET`
3. `~/.proxy/mihomo.json`（`core connect … --save` 写下的）
4. core 自己的 config.yaml：`~/.config/mihomo/`、`~/.kualemon/mihomo/`、`/etc/mihomo/`
5. 某个客户端的状态文件（这台机器上是 `~/.kualemon/state.json`）
6. 探 9090 / 9091 / 9097 / 6170

第 4 步排在第 5 步前面是有意的：**被访问的是 core，不是客户端**。core 可以手工从配置文件启动，
那时第 5 步的文件根本不存在。实测这台机器上 doctor 报的是
`found via C:\Users\…\.kualemon\mihomo\config.yaml`——一次都没碰 flm 的 state。

换一个端点：

```bash
proxy core connect 10.0.0.2:6170 --secret xxx --save    # 记住它
MIHOMO_CONTROLLER=10.0.0.2:6170 MIHOMO_SECRET=xxx proxy core groups   # 或者只用一次
```

## 换节点

```bash
proxy core groups                   # 有哪些策略组，各自现在走谁
proxy core nodes 漏网之鱼           # 这个组的候选，标出当前
proxy core use 漏网之鱼 "日本 I1"    # 切换，然后读回来确认
proxy core use 漏网之鱼 印尼         # 唯一匹配才允许省全名
```

三条纪律：

- **歧义拒绝，不猜。** `日本` 命中 10 个会直接报错并全部列出来；"取第一个匹配"是错路由的
  常见来源，而且在界面上看起来完全正常。
- **notice 不是节点。** 供应商会把「未读消息」塞进候选列表。选它不报错，但所有继承它的组
  会一起坏掉。`use` 会拒绝。
- **切换要读回。** API 返回成功只说明请求收下了。`use` 之后重新 GET 那个组，`now` 不等于
  目标就报错。

真正的证据是出口 IP，不是 API 回显：

```bash
curl -s -x http://127.0.0.1:7897 https://api.ipify.org      # 103.196.116.251
proxy core use 漏网之鱼 "日本 I1"
curl -s -x http://127.0.0.1:7897 https://api.ipify.org      # 45.146.163.49
proxy core use 漏网之鱼 "印尼 B46"
curl -s -x http://127.0.0.1:7897 https://api.ipify.org      # 103.196.116.251
```

**前提是这个 echo 站点和目标 host 命中同一组规则**——上面成立是因为 `api.ipify.org` 在
白名单里，走的就是 `漏网之鱼`。不在白名单的 echo 站点走直连，地址不变，什么也证明不了。

## bench：延迟和带宽是两个轴

```bash
proxy core bench 漏网之鱼                                   # 延迟，快的在前，超时在最后
proxy core bench 漏网之鱼 --throughput --seconds 8 --limit 5
```

不写组名时它会挑"真实节点最多的那个组"并在输出里说明——那常常不是你关心的组，所以**写清楚
组名**。

只按延迟选节点会选错：一条便宜的共享线路可以答得很快、传得很慢。

**吞吐必须证明字节真的过了那个节点**，这里有个会静默说谎的坑：在 `rule` 或白名单模式下，
测试 URL 很可能命中一条直连规则，于是测出来的是**本机自己那条线**的速度，而标签上写着节点名。
这正是"换节点救不了下载慢"这类结论的来源之一——那个数字可能压根没经过节点。

所以 `--throughput` 先把 core 切到 `global` 模式（规则不再参与决定），再逐节点跑固定窗口的
传输，最后在 `finally` 里把模式和原来的选择一起还原。bench 把整台机器留在 global 模式是
不能接受的，被中断也一样要还原。

`--limit` 不是贴心功能：81 个节点 × 8 秒是 11 分钟。

## 模式与热重载

```bash
proxy core mode              # 现在是 rule / global / direct
proxy core mode direct       # 全部直连——排查"是不是代理的问题"最干脆的一刀
proxy core reload [path]     # 改完 config.yaml 立刻生效，不重启 core，不断连接
```

`reload` 走 `PUT /configs?force=true`，是唯一"改规则不用重启"的路径。白名单就是这样生效的。

## 没有客户端还剩什么

把 `flm` 删掉之后：

| 能力 | 没有它时 |
|---|---|
| 换节点 / 策略组 / 模式 | **不受影响**，全在上面那些命令里 |
| bench 延迟和吞吐 | **不受影响** |
| 看某条连接走了哪个节点 | 不受影响（`/proxies`、`/connections`） |
| 改白名单 | 把规则写进 config.yaml 再 `proxy core reload` |
| 启停 core | core 就是个普通进程：`mihomo -d <目录> -f config.yaml` |
| 系统代理开关 | Windows 上一个注册表键，与 core 无关 |
| **刷新订阅**（节点列表变了） | **需要**账号那一步 |

只有最后一项真的依赖供应商账号：账号 API 返回一个 Clash 订阅 URL，那个 URL 就是全部配置
（节点 + 策略组 + 规则）。换成一个自己的订阅，core 照样吃：

```yaml
proxy-providers:
  main:
    type: http
    url: <订阅 URL>
    interval: 3600
    path: ./providers/main.yaml
```

然后 `proxy core reload`。core 的配置里必须有 `external-controller` + `secret` + `mixed-port`
——少任何一个，上面所有命令都找不到它，`doctor` 第一个检查的就是这件事。

**结论：把 flm 换掉要写的只是"登录 → 拿订阅 URL"那一步，其余都是 core 的 API，已经在这儿了。**

## 出口 IP 只在规则相同时才说明问题

`check` 会比走代理和不走代理的出口 IP，但**那只在那个 echo 站点和目标 host 命中同一条
规则时才有意义**。如果不相同，它会明确说出来：

```
exit note         icanhazip.com follows a different rule, so that address says nothing
                  about github.com; the decision log below is what settles it
```

这是这个 skill 存在的原因之一：拿一个未路由的 echo 站点去推断「代理没生效」，会把人
带进完全错误的方向。`doctor` 也一样——它比较出口只在有用时才比，否则明说「不适用」。

## 配置

`~/.proxy/config.json`，也可以直接手改：

```json
{
  "listen": {"host": "127.0.0.1", "port": 7899},
  "upstreams": {
    "direct": {"kind": "direct"},
    "relay":  {"kind": "http", "address": "127.0.0.1:7897"}
  },
  "rules": [
    {"type": "suffix", "value": "github.com", "via": "relay"},
    {"type": "all", "via": "direct"}
  ]
}
```

规则按顺序匹配，**第一条命中就定**。规则类型：`suffix`（含子域）、`exact`、`prefix`、
`cidr`、`all`。

**最后一条必须是 `all`。** 少了它，一个不匹配任何规则的 host 没有定义好的去向，而
"大概直连吧"正是让代理看起来坏了的那种假设。`validate_config` 会直接拒绝这种配置。

## 命令

| 命令 | 作用 |
|---|---|
| `proxy doctor` | 逐环检查：curl、配置合法性、catch-all、监听、**每个上游能否真的建立隧道** |
| `proxy serve` | 前台跑代理 |
| `proxy status` | 配置、监听状态、最近的决策 |
| `proxy rules` | 规则表 |
| `proxy check HOST` | 这个 host 到底走没走、走的哪条 |
| `proxy add VALUE --via NAME [--type]` | 加规则（插在 catch-all 之前） |
| `proxy rm VALUE` | 删规则 |
| `proxy upstream list` / `add` | 管理上游 |
| `proxy env` | 打印客户端需要的 export |
| `proxy core doctor` | 控制器在哪、能不能答、模式与端口 |
| `proxy core groups` / `nodes` | 策略组与候选节点 |
| `proxy core use GROUP NODE` | 换节点，并读回确认 |
| `proxy core bench [GROUP] [--throughput]` | 延迟或吞吐，快的在前 |
| `proxy core mode [rule\|global\|direct]` | 报告或切换模式 |
| `proxy core reload [PATH]` | 热重载配置，不重启 |
| `proxy core connect HOST:PORT [--save]` | 记住一个控制器地址 |

## 实现上几个不显然的决定

- **所有上游走同一条路**：先建到 `host:port` 的隧道，再在隧道里说普通 HTTP。所以
  CONNECT 和明文请求是同一段代码，直连/HTTP/SOCKS5 也是同一段。分支越少，能错的地方越少。
- **不用 `SO_REUSEADDR`（Windows 上）**：它允许第二个实例绑同一端口，连接会分给任意一个。
  一个还活着、带着旧规则的实例，看起来就像"新加的规则没生效"——这个坑我踩过。现在端口
  被占用会直接拒绝启动。
- **`doctor` 检查上游能否建立隧道，不检查端口通不通**：一个接受连接但拒绝 CONNECT 的代理
  看起来是活的，却毫无用处。
- **只读标准库**：HTTP CONNECT、SOCKS5 握手、PNG、relay 都是手写的，`uv run` 瞬间启动。

## 限制

- 只处理 HTTP 和 HTTPS（通过 CONNECT）。其他协议的流量不会经过它。
- 不做 DNS 代理：域名在代理进程里解析，客户端看到的是自己解析的地址。需要 fake-ip 一类
  行为的话，这一套不够。
- 规则在启动时读入，改完要重启 `serve`。
- `check` 的判定依赖代理写日志；日志路径不可写时它会退化成"没有记录"，而不是假装成功。
- `proxy core` 只驱动**已经在跑**的 core。它不启动 core、不下载订阅——那是另一件事。

## 自己验证它

```bash
python3 test_rules.py      # 16 项：后缀/子域/大小写/CIDR 边界/规则顺序/配置拒绝
python3 test_mihomo.py     # 48 项：配置抓取/名称解析/排序/bench 还原/发现顺序
```

这些用例有齿。规则那边：把 `suffix` 改成 `startswith`、让 `cidr` 忽略掩码、倒着匹配规则、
取消 catch-all，每一个都会红。core 这边做过同样的验证：把排序改成"失败也算 0 延迟"、
让 `node_choices` 保留组引用、把 `finally` 还原改成只在成功时还原、把客户端 state 排到
config 前面、去掉 notice 识别、让速率忽略时间窗口——六项全部变红。

其中一条值得记住：**"bench 失败也要还原"最初没被测到**，因为逐节点的错误在循环里就被
吞掉了，`finally` 和"只在成功时还原"在那些用例里表现一致。改成用 Ctrl-C 那种会逃出循环的
异常去测，它才真的红。测试没红，先怀疑测试。
