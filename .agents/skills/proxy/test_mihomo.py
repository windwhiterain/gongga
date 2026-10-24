#!/usr/bin/env python3
"""Core-API tests: discovery, name resolution, ordering and bench restoration.

These exercise the decisions that are wrong in ways the network cannot reveal. A bench
that leaves the core in `global` mode, a resolver that silently picks the first of
twenty matches, or a latency sort that ranks a timeout above a working node all look
like success from the outside.

    python3 test_mihomo.py        # exits non-zero on failure
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("mihomo", os.path.join(HERE, "mihomo.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

bad = 0


def check(name, condition, detail=""):
    global bad
    if condition:
        print("  ok   %s" % name)
    else:
        bad += 1
        print("  FAIL %s%s" % (name, ("  -- " + detail) if detail else ""))


def raises(name, function, fragment=""):
    global bad
    try:
        function()
    except m.CoreError as error:
        if fragment and fragment not in str(error):
            bad += 1
            print("  FAIL %s  -- message was %r" % (name, str(error)))
        else:
            print("  ok   %s" % name)
        return
    bad += 1
    print("  FAIL %s  -- nothing was raised" % name)


PROXIES = {
    "GLOBAL": {"type": "Selector", "now": "DIRECT", "all": ["DIRECT", "HK 1", "节点选择", "未读消息"]},
    "节点选择": {"type": "Selector", "now": "HK 1", "all": ["HK 1", "HK 2", "JP 1"]},
    "漏网之鱼": {"type": "Selector", "now": "JP 1", "all": ["HK 1", "JP 1", "未读消息"]},
    "HK 1": {"type": "Shadowsocks"},
    "DIRECT": {"type": "Direct"},
    "REJECT": {"type": "Reject"},
}


print("config scraping")
controller, secret = m.parse_core_config(
    "# comment\nexternal-controller: 127.0.0.1:9090\nsecret: 'abc123'\nmixed-port: 7897\n"
)
check("finds the controller", controller == "127.0.0.1:9090", controller)
check("finds a quoted secret", secret == "abc123", secret)
check("ignores commented keys", m.parse_core_config("# external-controller: 1.2.3.4:9\n") == (None, None))
check("empty secret is absent", m.parse_core_config("secret:\n")[1] is None)
check("quoted controller", m.parse_core_config('external-controller: "0.0.0.0:9090"\n')[0] == "0.0.0.0:9090")

print("origins")
check("bare host:port", m.origin_from_controller("127.0.0.1:9090") == "http://127.0.0.1:9090")
check("scheme kept", m.origin_from_controller("http://10.0.0.2:6170") == "http://10.0.0.2:6170")
check("port defaults", m.origin_from_controller("127.0.0.1") == "http://127.0.0.1:9090")
check("empty gives nothing", m.origin_from_controller("  ") is None)

print("classifying entries")
check("notice is not a route", m.is_notice("未读消息"))
check("notice keyword", m.is_notice("vendor NOTICE"))
check("a node is a node", not m.is_notice("HK 1"))
check("group reference detected", m.is_group_reference(PROXIES, "节点选择"))
check("node is not a reference", not m.is_group_reference(PROXIES, "HK 2"))
check(
    "measured choices drop notices and group references",
    m.node_choices(PROXIES, PROXIES["GLOBAL"]) == ["DIRECT", "HK 1"],
    str(m.node_choices(PROXIES, PROXIES["GLOBAL"])),
)

print("resolving names")
check("exact group wins", m.resolve_group(PROXIES, "节点选择")[0] == "节点选择")
check("containment finds a unique group", m.resolve_group(PROXIES, "漏网")[0] == "漏网之鱼")
raises("ambiguous group refused", lambda: m.resolve_group({**PROXIES, "节点选择2": PROXIES["节点选择"]}, "节点"), "matches")
raises("unknown group refused", lambda: m.resolve_group(PROXIES, "不存在"))
check("exact node wins", m.resolve_node(PROXIES["节点选择"], "JP 1") == "JP 1")
check("containment finds one node", m.resolve_node(PROXIES["节点选择"], "JP") == "JP 1")
raises("ambiguous node refused", lambda: m.resolve_node(PROXIES["节点选择"], "HK"), "matches")
raises("unknown node refused", lambda: m.resolve_node(PROXIES["节点选择"], "US"))

print("ordering")
rows = [
    {"node": "slow", "delay": 900, "ok": True},
    {"node": "dead", "delay": None, "ok": False},
    {"node": "fast", "delay": 40, "ok": True},
]
ordered = [row["node"] for row in m.sort_bench(rows)]
check("fastest first", ordered[0] == "fast", str(ordered))
check("failure sorts last even though its delay is None", ordered[-1] == "dead", str(ordered))
throughput_rows = [
    {"node": "a", "kbps": 10.0, "ok": True},
    {"node": "b", "kbps": 0.0, "ok": False},
    {"node": "c", "kbps": 300.0, "ok": True},
]
check("a failed transfer never sorts ahead", [r["node"] for r in sorted(throughput_rows, key=lambda r: (not r["ok"], -r["kbps"]))][0] == "c")
check("rate arithmetic", m.throughput_of(2048 * 1024, 2) == 1024.0)
check("zero window is guarded", m.throughput_of(100, 0) == 0.0)

print("display width")
check("ascii is one wide", m.display_width("abc") == 3)
check("chinese is twice as wide", m.display_width("节点") == 4)
check("padding accounts for width", m.display_width(m.pad("节点", 7)) == 7)
check("emoji does not break padding", m.display_width(m.pad("🐟 x", 6)) == 6)


print("bench restores the core even when a transfer fails")


class FakeCore(object):
    """Records what the bench did to the core, so a missing restore is visible."""

    def __init__(self, fail_on=None):
        self.mode = "rule"
        self.selection = "DIRECT"
        self.selected = []
        self.fail_on = fail_on

    def configs(self):
        return {"mode": self.mode, "mixed-port": 7897}

    def proxies(self):
        return {
            **PROXIES,
            "GLOBAL": {"type": "Selector", "now": self.selection, "all": ["DIRECT", "HK 1", "JP 1"]},
            "漏网之鱼": {"type": "Selector", "now": "JP 1", "all": ["HK 1", "JP 1"]},
        }

    def set_mode(self, mode):
        self.mode = mode

    def select(self, group, node):
        self.selected.append((group, node))
        if group == "GLOBAL":
            self.selection = node
        if node == self.fail_on:
            raise m.CoreError("node refused the connection")


def run_bench(fail_on=None, interrupt=False, node_failure=False):
    core = FakeCore(fail_on=fail_on)
    original = m.measure_transfer
    if interrupt:
        def boom(*_args, **_kwargs):
            raise KeyboardInterrupt()
        m.measure_transfer = boom
    elif node_failure:
        def dead(*_args, **_kwargs):
            raise m.CoreError("transfer died")
        m.measure_transfer = dead
    else:
        m.measure_transfer = lambda port, seconds, url: (1024 * 1024, 512.0)
    try:
        name, rows = m.bench_throughput(core, "漏网之鱼", seconds=1, url="http://x")
    except KeyboardInterrupt:
        name, rows = None, []
    finally:
        m.measure_transfer = original
    return core, name, rows


core, name, rows = run_bench()
check("measured the real nodes of the group", [row["node"] for row in rows] == ["HK 1", "JP 1"], str(rows))
check("mode is restored to rule", core.mode == "rule", core.mode)
check("GLOBAL selection is restored", core.selection == "DIRECT", core.selection)
check("transfers ran in global mode", any(node == "HK 1" for _, node in core.selected))

core, _, rows = run_bench(node_failure=True)
check("a dead node is recorded as a failure, not a crash", all(not row["ok"] for row in rows), str(rows))
check("mode restored after every node fails", core.mode == "rule", core.mode)
check("selection restored after every node fails", core.selection == "DIRECT", core.selection)

# A per-node failure is caught inside the loop, so the restore is only proved by
# something that escapes the loop entirely -- an interrupt during the transfer.
core, _, _ = run_bench(interrupt=True)
check("mode restored when the bench is interrupted", core.mode == "rule", core.mode)
check("selection restored when the bench is interrupted", core.selection == "DIRECT", core.selection)
core, _, _ = run_bench(fail_on="JP 1")
check("selection restored after a node refuses", core.selection == "DIRECT", core.selection)

print("discovery order")
import unittest.mock as mock

config_calls = []


def fake_read_config(path):
    config_calls.append(path)
    return ("127.0.0.1:9999", "from-config")


with mock.patch.dict(os.environ, {}, clear=False):
    os.environ.pop("MIHOMO_CONTROLLER", None)
    with mock.patch.object(m, "read_cache", return_value=None), \
         mock.patch.object(m, "core_config_paths", return_value=["/x/config.yaml"]), \
         mock.patch.object(m, "read_core_config", side_effect=fake_read_config):
        found = m.discover()
    check("config file outranks a client state file", found["origin"] == "http://127.0.0.1:9999", str(found))
    check("secret comes from the same file", found["secret"] == "from-config", str(found))

    with mock.patch.object(m, "read_cache", return_value={"origin": "http://127.0.0.1:1", "secret": "c", "source": "cache", "config": None}), \
         mock.patch.object(m, "core_config_paths", return_value=["/x/config.yaml"]):
        check("cache outranks the config file", m.discover()["origin"] == "http://127.0.0.1:1")

    os.environ["MIHOMO_CONTROLLER"] = "127.0.0.1:7777"
    os.environ["MIHOMO_SECRET"] = "env-secret"
    found = m.discover()
    check("environment outranks everything", found["origin"] == "http://127.0.0.1:7777", str(found))
    check("environment carries the secret", found["secret"] == "env-secret")
    check("explicit outranks the environment", m.discover("127.0.0.1:5555", "s")["origin"] == "http://127.0.0.1:5555")

print("\n%s" % ("PASS" if not bad else "%d FAILED" % bad))
sys.exit(1 if bad else 0)
