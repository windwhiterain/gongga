#!/usr/bin/env python3
"""Rule-engine tests: the part that decides routes, exercised without a network.

Every case here is a way the matcher has been or could be got wrong; four of them were
confirmed by putting the bug back and watching the test fail. Run from anywhere:

    python3 test_rules.py        # exits non-zero on failure
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("proxy", os.path.join(HERE, "proxy.py"))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)

CFG = {
  "listen": {"host": "127.0.0.1", "port": 7899},
  "upstreams": {"direct": {"kind": "direct"}, "a": {"kind": "http", "address": "127.0.0.1:1"}},
  "rules": [
    {"type": "suffix", "value": "github.com", "via": "a"},
    {"type": "cidr", "value": "10.0.0.0/8", "via": "a"},
    {"type": "all", "via": "direct"},
  ],
}

def check(name, cond, detail=""):
    print(("ok   " if cond else "FAIL ") + name + ("" if cond else "  " + detail))
    return 0 if cond else 1

bad = 0
bad += check("suffix matches the bare domain", p.pick(CFG, "github.com")[0] == "a")
bad += check("suffix matches a subdomain", p.pick(CFG, "api.github.com")[0] == "a")
bad += check("suffix does not match a longer label",
             p.pick(CFG, "notgithub.com")[0] == "direct",
             "notgithub.com must not match github.com")
bad += check("suffix does not match a prefix-only domain",
             p.pick(CFG, "github.company.com")[0] == "direct")
bad += check("case is ignored", p.pick(CFG, "API.GitHub.COM")[0] == "a")
bad += check("catch-all covers a stranger", p.pick(CFG, "example.org")[0] == "direct")
bad += check("cidr matches an address in range", p.pick(CFG, "x", "10.1.2.3")[0] == "a")
bad += check("cidr rejects an address outside", p.pick(CFG, "x", "11.1.2.3")[0] == "direct")
bad += check("cidr /8 boundary low", p.pick(CFG, "x", "10.0.0.0")[0] == "a")
bad += check("cidr /8 boundary high", p.pick(CFG, "x", "10.255.255.255")[0] == "a")
bad += check("first matching rule wins",
             p.pick(CFG, "github.com")[1]["value"] == "github.com")

# A config whose routes are not fully defined must be refused, not silently defaulted.
for name, cfg in [
    ("no rules", {**CFG, "rules": []}),
    ("no catch-all", {**CFG, "rules": [{"type": "suffix", "value": "x.com", "via": "a"}]}),
    ("rule points at unknown upstream", {**CFG, "rules": [{"type": "all", "via": "ghost"}]}),
    ("upstream without address", {**CFG, "upstreams": {"s": {"kind": "http"}}}),
    ("unknown upstream kind", {**CFG, "upstreams": {"s": {"kind": "quic", "address": "x:1"}}}),
]:
    try:
        p.validate_config(cfg); bad += check("refuses " + name, False, "it was accepted")
    except ValueError:
        bad += check("refuses " + name, True)

print(f"\n{'PASS' if not bad else str(bad) + ' FAILED'}")
sys.exit(1 if bad else 0)
