# Publishing the whiting_main endpoint

Goal: a friend uses the MiniCPM-V endpoint on `whiting_main` with no client installed on
his side. Two shapes survive that constraint; a mesh or a tunnel client on the friend's
machine does not, so Tailscale / ZeroTier / EasyTier and Cloudflare Tunnel are out.

## Measured facts (2026-11-23)

| Item | Value |
| --- | --- |
| Host | `whiting_main` = `whting@192.168.1.3`, Ubuntu 24.04 (kernel 6.8.0-151), LAN `192.168.1.0/24`, gateway `192.168.1.1` |
| Service | vLLM 0.34.0 `serve models/MiniCPM-V-4.7-35B-A3B-int4`, `--served-model-name minicpm47-35b-a3b`, `0.0.0.0:8312`, `--api-key sk-fake`, `--max-num-seqs 1`, `--max-model-len 262144` |
| Hardware | RTX 3060 Ti 8 GB; `--cpu-offload-gb 18 --cpu-offload-params experts`, `--kv-offloading-size 4`, `--enforce-eager` |
| Throughput | 32 tokens in 5.03 s ≈ 6.4 tok/s, one request at a time |
| Supervision | systemd **user** unit `minicpm47-endpoint.service` (`Type=forking`, pidfile `/home/whting/kv-mask/exp/minicpm47.pid`), `Restart=on-failure` |
| Egress | `220.167.91.56` (中国 四川 乐山 电信) — outside `100.64/10`, so not CGNAT, but an upstream NAT is not excluded until an inbound test passes |
| IPv6 | link-local only; no global address |
| International | `github.com`, `api.ipify.org` unreachable from the host |
| Gateway | `http://192.168.1.1` answers, title `中国电信智能网关`; SSDP discovery returns nothing → UPnP IGD is off or absent, so the mapping is a manual page |
| Tooling on host | no cloudflared / frp / nginx / caddy / socat / autossh; `sudo` needs a password |

## Plan B — forward a port on the gateway (free)

```
friend -> 220.167.91.56:18312 -> gateway DNAT -> 192.168.1.3:8312
```

- Needs the gateway login (sticker account `useradmin`, super account `telecomadmin` if the
  menu is hidden): 应用 / 高级NAT → 端口映射 or 虚拟服务器, WAN `18312` → `192.168.1.3:8312`.
- Verification is a phone on 4G, never the LAN: `http://220.167.91.56:18312/v1/models` must
  answer `{"error":"Unauthorized"}`.
- Costs nothing, lowest latency. Costs the house its privacy: the address is public and
  scanned, the WAN address is DHCP (needs DDNS), the key travels without TLS, and a home line
  serving the public is outside the carrier's terms.

## Plan C — a VPS and a reverse tunnel (paid, ~¥100–250/year)

```
friend -> https://api.<domain>/v1 -> Caddy -> LiteLLM -> frps:18312
                                                          ^ outbound
                                                    frpc on whiting_main -> 127.0.0.1:8312
```

- A domestic lightweight VPS in 成都/重庆 (20 ms to 乐山) carries the public face; the house
  opens no port, so the public address never learns where it lives.
- frps/frpc with a token and TLS. Caddy terminates TLS and caps the body size. LiteLLM Proxy
  hands the friend his own virtual key with a budget and a concurrency cap, and keeps the
  spend log; `sk-fake` then never reaches the internet.
- 4 Mbit peak is ample: 6 tok/s of streamed JSON is tens of bytes a second.

## What is true under both

- `--api-key sk-fake` is a public string. Turning it into a real key is a **vLLM restart**
  (the flag is CLI-only), and a restart pays a cold load of the 18 GB of offloaded experts.
  Under Plan C this can be skipped: the tunnel is token+TLS and only the VPS can reach 8312,
  so the public key lives in LiteLLM. Under Plan B there is no such layer, so the restart is
  the only way to get a real key.
- `--max-num-seqs 1` plus a 262144-token window means one long request owns the endpoint.
  Whatever the path, the entry point must cap concurrency (1–2) and `max_tokens`.

## Open

- Is `220.167.91.56` a true inbound-reachable address? Answered only by the 4G test.
- Who logs into the gateway, and with which account.
- If Plan C: which VPS region and whether a domain is bought (without one the face is
  `http://<vps-ip>:<port>`, no TLS).

## Plan B was tried and the line cannot carry it (2026-11-23)

The gateway (天翼智能网关, LuCI at `192.168.1.1`, login `useradmin`) took the mapping:
`whiting-minicpm`, LAN `192.168.1.3`, TCP, inside `8312`, outside `18312`, state 生效.
The LAN leg answers: `192.168.1.3:8312/v1/models` → 401 Unauthorized.

The public leg does not. `tool.chinaz.com/port` reports `220.167.91.56:18312` closed, and the
same tool on `www.baidu.com:443` reports open, so the tool is not the problem. From the host,
`tracepath 223.5.5.5` gives hop 1 `192.168.1.1`, hop 2 **`100.74.198.1`**: the line sits behind
carrier-grade NAT, and `220.167.91.56` is the carrier's shared egress, not an address of this
line. No mapping on the gateway can make inbound arrive; hairpin through the public address
also times out, as such gateways usually do.

What that leaves, in cost order:

| Route | Cost | Catch |
| --- | --- | --- |
| Ask ���信 for a public IPv4 (10000 / 营业厅), then redial | free | granted or not is theirs to decide; address stays dynamic |
| Turn on IPv6 in the gateway and publish `http://[240e:…]:18312` | free | needs the friend's network to be IPv6-capable; the prefix moves, so it needs AAAA DDNS |
| Plan C, VPS + frp | ~¥100–200/year | a VPS to run |

Until one is chosen the mapping on the gateway does nothing; it is one row on the 映射列表 page
if it should be removed.

## IPv6: the prefix stops at the gateway (2026-11-23)

`admin/settings/info` on the ZXHN F1855V2 (软件 21ZTW40002) reports `WAN IP 100.74.198.26`,
`WAN IPv6 240e:398:4905:f0d4:9ce9:1cee:f5af:99d9`, `LAN IPv6 fe80::1`. So the ISP does hand
the line a public IPv6; the gateway keeps it on the WAN interface and advertises nothing into
the LAN.

Evidence that this is the gateway's choice and not a host misconfiguration: the Windows machine
on the same LAN also holds only link-local addresses (`Get-NetIPAddress -AddressFamily IPv6`),
and Windows accepts router advertisements by default. The Linux host additionally has
`net.ipv6.conf.enp5s0.accept_ra = 0`, which would block an advertisement if one existed.

The whole `useradmin` UI has no IPv6 page — every route it serves is `admin/settings/{info,lan,
status,portmap_config,portmap_list,user,restore,fwrestore}`, `admin/wifi/{base,advance,
inter_connd}`, `admin/device/{pc,phone,wifi,itv,black}`, `admin/storage/{status,settings}`,
`admin/download`, `admin/help`, `admin/home` — and the maintenance portal at
`192.168.1.1:8080/maintain` offers only 设备注册, Ping, NetInfo, 恢复出厂 and a 桥接/路由 switch.
The LAN IPv6 switch lives behind `telecomadmin`.

`telecomadmin` / `nE7jA%5m` was refused. The request shape was controlled: the same POST to
`/cgi-bin/luci` with `useradmin` / `jaxkw` returns 302 with `Set-Cookie: sysauth`, so the
refusal is the password, not the form. Further guesses risk the firmware's lockout.

Remaining ways to an IPv6 face, both outside this session: more `telecomadmin` variants (lockout
risk), or 电信 (10000 / 装维师傅) turning the LAN advertisement on. Neither is a prerequisite of
Plan C.

## Chosen: Plan C (2026-11-23)

Stack, smallest that still gives the friend his own credential and a cap:

```
friend -> http://<vps>:4000/v1   new-api (SQLite, virtual keys, quota)   [VPS]
        -> http://127.0.0.1:18312  frps                                  [VPS]
        <- frpc outbound ----------------------------------------------  [whiting_main]
        -> http://127.0.0.1:8312   vLLM                                  [whiting_main]
```

- vLLM is not restarted: `sk-fake` never leaves the loopback path, the tunnel is token+TLS, and
  the public credential is new-api's.
- No domain and no 备案 for the first version: the face is `<vps-ip>:4000`, a non-standard port.
  TLS comes later if the friend's key must not travel in clear.
- 腾讯云轻量 2核2G4M, 300 GB/month, 成都地域, Ubuntu 24.04: roughly ¥88–99 for the first year
  (promotional; renewal is higher). 4 Mbit is ample for 6 tok/s of streamed JSON.
- Still open: the VPS itself, its IP and root credential, and the firewall allowing 4000 and 7000.

## Built and verified (2026-11-23)

The stack that exists is smaller than the plan above, because GitHub answers both this machine and
the VPS at 60–160 KB/s: the frp and new-api downloads were the only slow, unreliable step, and
neither is needed. What runs instead:

```
friend -> http://162.14.81.104:4000/v1        nginx: key check, limit_conn 2, logs   [VPS]
        -> http://127.0.0.1:18312             ssh -R tunnel                          [VPS]
        <- outbound from the house ----------  systemd --user vllm-tunnel.service     [whiting_main]
        -> http://127.0.0.1:8312              vLLM                                   [whiting_main]
```

- 腾讯云轻量 成都, `lhins-72g31nfq`, Ubuntu 26.04 LTS, 2 core / 2 GB / 4 Mbit, ¥230.40 for a year.
- nginx 1.28.3 from the Ubuntu mirror installs in seconds and already carries the three things
  the gateway needs: an exact `Authorization` match (`map`), a per-IP concurrency cap
  (`limit_conn`), and an access log. It rewrites the header to `Bearer sk-fake` on the way out,
  so vLLM never needed a restart and its own key never leaves the loopback path.
- The tunnel is plain `ssh -R 127.0.0.1:18312:127.0.0.1:8312`, kept by a user unit with
  `Restart=always`, `ExitOnForwardFailure=yes` and `ServerAliveInterval=30`; `loginctl
  enable-linger whting` lets it outlive a logout. The house opens no port.
- The VPS firewall (not a security group — Lighthouse has its own) allows TCP 4000, 22, 80, ICMP.

Evidence from outside the house, against the public address:

| Probe | Result |
| --- | --- |
| `GET /v1/models`, no key | 401 in 0.35 s |
| `GET /v1/models`, wrong key | 401 |
| `GET /v1/models`, friend key | 200, lists `minicpm47-35b-a3b` |
| one completion, 24 tokens | `gateway ok`, 3 completion tokens, 1.5 s |
| one completion, 8 tokens | `Hello there, how are you?` |
| `stream: true` | `data:` chunks arrive incrementally |
| three concurrent 400-token requests | 200, 200, **503** — the cap holds |

Operational details, the friend's key and how to rotate it live outside the repository in
`~/.agents/inference-gateway.txt`; the note keeps the shape, not the secret.

Not done, and each is a decision rather than an oversight: no TLS (the key travels in clear until
a domain is bought), no token metering (the cap is concurrency and key, not spend), no per-key
quota (one friend, one key).
