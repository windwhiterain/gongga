---
name: proxy
description: 让某台机器的某个 host 走指定的上游（HTTP/SOCKS5/直连），并证明它真的走了。用户提供任何 API/代理端点就能接。任何"这个 API 连不上/到底有没有走代理/怎么只让某个域名走代理"的问题用它。
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

## 自己验证它

```bash
python3 test_rules.py      # 16 项：后缀/子域/大小写/CIDR 边界/规则顺序/配置拒绝
```

这些用例有齿：把 `suffix` 改成 `startswith`、让 `cidr` 忽略掩码、倒着匹配规则、取消
catch-all 强制——每一个都会让测试变红。
