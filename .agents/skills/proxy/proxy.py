# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""A routing proxy in one file: per-host rules, pluggable upstreams, measured proof.

Why this exists rather than wrapping an existing router: the question that wastes real
time is not "is a proxy running" but "did THIS request go through it". A router that
decides by rule but never says which rule fired leaves that to inference, and inference
is wrong often enough to matter -- an echo host that is not itself routed reports the
direct address, which reads as a broken tunnel when nothing is broken.

Here the proxy is the decision maker, so the route is a fact it can report. `check` then
proves the effect by comparing the exit address it sees through the proxy with the one it
sees without it.

Design notes:
  * Every upstream is used the same way: open a tunnel to host:port, then speak plain
    HTTP inside it. One code path for direct, HTTP and SOCKS5 means one place to be
    wrong, and it keeps CONNECT and ordinary requests identical.
  * The rule list must end in a catch-all. A host matching nothing has no defined
    route, and inventing one is how "probably direct" gets mistaken for a decision.
  * Nothing here needs a third-party package: stdlib only, so `uv run` starts instantly.
"""

import argparse
import base64
import json
import os
import select
import socket
import struct
import sys
import threading
import time
import urllib.parse

CONFIG_DIR = os.path.join(os.path.expanduser("~"), ".proxy")
CONFIG_PATH = os.path.join(CONFIG_DIR, "config.json")
DECISIONS_PATH = os.path.join(CONFIG_DIR, "decisions.jsonl")

DEFAULT_CONFIG = {
    "listen": {"host": "127.0.0.1", "port": 7899},
    "upstreams": {
        "direct": {"kind": "direct"},
    },
    "rules": [
        {"type": "all", "via": "direct"},
    ],
}

ECHO_HOSTS = ["https://api.ipify.org", "https://icanhazip.com", "https://ifconfig.me/ip"]


# --- configuration ------------------------------------------------------------


def load_config(path=CONFIG_PATH):
    """Read the config, or return the default when there is none yet."""
    if not os.path.exists(path):
        return json.loads(json.dumps(DEFAULT_CONFIG))
    with open(path, "r", encoding="utf-8") as fh:
        config = json.load(fh)
    validate_config(config)
    return config


def save_config(config, path=CONFIG_PATH):
    validate_config(config)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(config, fh, indent=2, ensure_ascii=False)
        fh.write("\n")


def validate_config(config):
    """Refuse a config whose routes are not fully defined."""
    rules = config.get("rules") or []
    if not rules:
        raise ValueError("no rules: every request would have no route")
    if rules[-1].get("type") != "all":
        raise ValueError("the last rule must be {\"type\": \"all\"}, or a host matching "
                         "nothing has no defined route")
    upstreams = config.get("upstreams") or {}
    if not upstreams:
        raise ValueError("no upstreams defined")
    for rule in rules:
        via = rule.get("via")
        if via not in upstreams:
            raise ValueError("rule %r points at upstream %r, which is not defined"
                             % (rule.get("value", "all"), via))
    for name, spec in upstreams.items():
        kind = spec.get("kind")
        if kind not in ("direct", "http", "socks5"):
            raise ValueError("upstream %r has kind %r; expected direct, http or socks5" % (name, kind))
        if kind != "direct" and not spec.get("address"):
            raise ValueError("upstream %r needs an address such as 127.0.0.1:1080" % name)


# --- rules --------------------------------------------------------------------


def rule_matches(rule, host, ip=None):
    """Does this rule apply to the destination? Host matching is case-insensitive."""
    kind = rule.get("type")
    value = (rule.get("value") or "").lower().strip(".")
    host = (host or "").lower().strip(".")
    if kind == "all":
        return True
    if kind == "exact":
        return host == value
    if kind == "suffix":
        return host == value or host.endswith("." + value)
    if kind == "prefix":
        return host.startswith(value)
    if kind == "cidr":
        return ip is not None and in_cidr(ip, value)
    raise ValueError("unknown rule type %r" % kind)


def in_cidr(ip, cidr):
    """IPv4 CIDR containment, using integers so no library is needed."""
    try:
        network, _, bits = cidr.partition("/")
        width = int(bits or 32)
        mask = (0xFFFFFFFF << (32 - width)) & 0xFFFFFFFF
        return (ip_to_int(ip) & mask) == (ip_to_int(network) & mask)
    except (ValueError, TypeError):
        return False


def ip_to_int(ip):
    parts = [int(p) for p in ip.split(".")]
    if len(parts) != 4 or any(p < 0 or p > 255 for p in parts):
        raise ValueError("not an IPv4 address: %r" % ip)
    return (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]


def looks_like_ip(host):
    return all(c.isdigit() or c == "." for c in host or "") and host.count(".") == 3


def pick(config, host, ip=None):
    """The upstream name for this destination: first matching rule wins."""
    for rule in config["rules"]:
        if rule_matches(rule, host, ip):
            return rule["via"], rule
    # validate_config guarantees a catch-all, so this is unreachable for a valid config.
    raise RuntimeError("no rule matched %r; the config is not valid" % host)


# --- upstreams ----------------------------------------------------------------


def connect_direct(host, port, timeout):
    return socket.create_connection((host, port), timeout=timeout)


def connect_http(address, host, port, timeout, auth=None):
    """Tunnel through an HTTP proxy with CONNECT."""
    host_part, _, port_part = address.rpartition(":")
    sock = socket.create_connection((host_part, int(port_part)), timeout=timeout)
    target = "%s:%d" % (host, port)
    request = "CONNECT %s HTTP/1.1\r\nHost: %s\r\n" % (target, target)
    if auth:
        user, _, password = auth.partition(":")
        token = base64.b64encode(("%s:%s" % (user, password)).encode()).decode()
        request += "Proxy-Authorization: Basic %s\r\n" % token
    request += "\r\n"
    sock.sendall(request.encode())
    header = read_until(sock, b"\r\n\r\n")
    status = header.split(b"\r\n")[0]
    if b" 200" not in status:
        sock.close()
        raise OSError("upstream refused CONNECT: %s" % status.decode("latin-1", "replace"))
    return sock


def connect_socks5(address, host, port, timeout, auth=None):
    """Tunnel through a SOCKS5 proxy, no-auth or username/password."""
    host_part, _, port_part = address.rpartition(":")
    sock = socket.create_connection((host_part, int(port_part)), timeout=timeout)
    want_userpass = auth is not None
    sock.sendall(b"\x05" + (b"\x02" if want_userpass else b"\x01") + (b"\x02" if want_userpass else b"\x00"))
    reply = recv_exact(sock, 2)
    if reply[0:1] != b"\x05":
        sock.close()
        raise OSError("upstream is not SOCKS5")
    method = reply[1:2]
    if method == b"\x02":
        if not want_userpass:
            sock.close()
            raise OSError("upstream wants a username and password")
        user, _, password = auth.partition(":")
        payload = b"\x01" + bytes([len(user)]) + user.encode() + bytes([len(password)]) + password.encode()
        sock.sendall(b"\x05" + payload)
        if recv_exact(sock, 2)[1:2] != b"\x00":
            sock.close()
            raise OSError("upstream rejected the credentials")
    elif method != b"\x00":
        sock.close()
        raise OSError("upstream wants an authentication method this client lacks")

    if looks_like_ip(host):
        target = b"\x01" + socket.inet_aton(host)
    else:
        encoded = host.encode()
        target = b"\x03" + bytes([len(encoded)]) + encoded
    sock.sendall(b"\x05\x01\x00" + target + struct.pack(">H", port))
    head = recv_exact(sock, 4)
    if head[1:2] != b"\x00":
        sock.close()
        raise OSError("SOCKS5 connect failed with code %d" % head[1])
    # Consume the bound address, whose length depends on its type.
    kind = head[3:4]
    if kind == b"\x01":
        recv_exact(sock, 4)
    elif kind == b"\x03":
        recv_exact(sock, recv_exact(sock, 1)[0])
    elif kind == b"\x04":
        recv_exact(sock, 16)
    recv_exact(sock, 2)
    return sock


def tunnel(config, upstream_name, host, port, timeout=15):
    """Open a socket to host:port along the named upstream, whatever kind it is."""
    spec = config["upstreams"][upstream_name]
    kind = spec["kind"]
    if kind == "direct":
        return connect_direct(host, port, timeout)
    if kind == "http":
        return connect_http(spec["address"], host, port, timeout, spec.get("auth"))
    if kind == "socks5":
        return connect_socks5(spec["address"], host, port, timeout, spec.get("auth"))
    raise ValueError("unknown upstream kind %r" % kind)


def read_until(sock, marker, limit=65536):
    data = b""
    while marker not in data:
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
        if len(data) > limit:
            break
    return data


def recv_exact(sock, count):
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise OSError("upstream closed during the handshake")
        data += chunk
    return data


# --- the decision log ---------------------------------------------------------


class Decisions:
    """Records which upstream each destination was sent to, and why.

    This is what makes the route a fact instead of an inference: the same code that
    chooses the upstream writes the record, so `check` cannot disagree with the server.
    """

    def __init__(self, path=DECISIONS_PATH):
        self.path = path
        self.lock = threading.Lock()

    def record(self, host, port, upstream, rule, phase="open", error=None):
        entry = {
            "at": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "host": host, "port": port, "via": upstream,
            "rule": {k: v for k, v in (rule or {}).items() if k != "via"},
            "phase": phase,
        }
        if error:
            entry["error"] = error
        line = json.dumps(entry, ensure_ascii=False)
        with self.lock:
            try:
                os.makedirs(os.path.dirname(self.path), exist_ok=True)
                with open(self.path, "a", encoding="utf-8") as fh:
                    fh.write(line + "\n")
            except OSError:
                pass  # a log that cannot be written must not break the connection
        return entry

    def recent(self, count=50):
        if not os.path.exists(self.path):
            return []
        with self.lock:
            with open(self.path, "r", encoding="utf-8", errors="replace") as fh:
                lines = fh.readlines()[-count:]
        entries = []
        for line in lines:
            try:
                entries.append(json.loads(line))
            except ValueError:
                continue
        return entries


# --- relaying -----------------------------------------------------------------


def relay(left, right, idle_timeout=300):
    """Copy bytes both ways until both sides are done.

    select() rather than two threads: a tunnel that leaks one thread per connection
    eventually runs out of them, and the failure then looks like a network problem.
    """
    sockets = [left, right]
    try:
        while True:
            readable, _, errored = select.select(sockets, [], sockets, idle_timeout)
            if errored or not readable:
                return
            for sock in readable:
                other = right if sock is left else left
                try:
                    data = sock.recv(65536)
                except OSError:
                    return
                if not data:
                    try:
                        other.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    sockets.remove(sock)
                    if not sockets:
                        return
                    continue
                try:
                    other.sendall(data)
                except OSError:
                    return
    finally:
        for sock in (left, right):
            try:
                sock.close()
            except OSError:
                pass


def read_request_head(sock, limit=65536):
    """Read one HTTP request head. Returns (head, leftover) or (None, b'')."""
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(4096)
        if not chunk:
            return None, b""
        data += chunk
        if len(data) > limit:
            return None, b""
    head, _, leftover = data.partition(b"\r\n\r\n")
    return head, leftover


def parse_head(head):
    """First line of a request: (method, target, version)."""
    line = head.split(b"\r\n", 1)[0].decode("latin-1")
    parts = line.split()
    if len(parts) < 3:
        return None, None, None
    return parts[0], parts[1], parts[2]


class ProxyServer:
    """A small HTTP proxy: CONNECT for TLS, absolute-form for plain HTTP."""

    def __init__(self, config, decisions=None):
        self.config = config
        self.decisions = decisions or Decisions()
        listen = config["listen"]
        self.address = (listen["host"], listen["port"])
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        # No SO_REUSEADDR: on Windows it lets a second instance bind a port that is
        # already serving, and connections then go to either one. A stale instance
        # holding old rules looked exactly like "the new rule had no effect".
        if os.name != "nt":
            self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            self.sock.bind(self.address)
        except OSError as exc:
            raise SystemExit("cannot listen on %s:%d (%s); another proxy is probably "
                             "already using it" % (self.address[0], self.address[1], exc))
        self.sock.listen(128)
        self.running = True

    def serve_forever(self):
        while self.running:
            try:
                client, _ = self.sock.accept()
            except OSError:
                if self.running:
                    continue
                return
            thread = threading.Thread(target=self.handle, args=(client,), daemon=True)
            thread.start()

    def stop(self):
        self.running = False
        try:
            self.sock.close()
        except OSError:
            pass

    def handle(self, client):
        try:
            client.settimeout(30)
            head, leftover = read_request_head(client)
            if head is None:
                return
            method, target, version = parse_head(head)
            if method is None:
                return
            if method == "CONNECT":
                self.do_connect(client, target, head)
            else:
                self.do_forward(client, method, target, version, head, leftover)
        except Exception:  # noqa: BLE001 - one bad connection must not stop the server
            pass
        finally:
            try:
                client.close()
            except OSError:
                pass

    def do_connect(self, client, target, head):
        host, _, port_text = target.rpartition(":")
        port = int(port_text or 443)
        upstream, rule = pick(self.config, host)
        try:
            upstream_sock = tunnel(self.config, upstream, host, port)
        except OSError as exc:
            self.decisions.record(host, port, upstream, rule, phase="failed", error=str(exc))
            client.sendall(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
            return
        self.decisions.record(host, port, upstream, rule)
        client.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        relay(client, upstream_sock)

    def do_forward(self, client, method, target, version, head, leftover):
        """Plain HTTP: open a tunnel, then send the request in origin form.

        Origin form travels inside the tunnel, so this is the same path as CONNECT and
        the upstream only ever has to understand TCP.
        """
        parsed = urllib.parse.urlsplit(target)
        if not parsed.hostname:
            client.sendall(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            return
        host = parsed.hostname
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
        upstream, rule = pick(self.config, host)
        try:
            upstream_sock = tunnel(self.config, upstream, host, port)
        except OSError as exc:
            self.decisions.record(host, port, upstream, rule, phase="failed", error=str(exc))
            client.sendall(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
            return
        self.decisions.record(host, port, upstream, rule)
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        lines = head.split(b"\r\n")
        lines[0] = ("%s %s %s" % (method, path, version)).encode()
        outgoing = b"\r\n".join(lines) + b"\r\n\r\n"
        try:
            upstream_sock.sendall(outgoing + leftover)
        except OSError:
            upstream_sock.close()
            return
        relay(client, upstream_sock)


# --- measuring ----------------------------------------------------------------


def run(cmd, timeout=60):
    import subprocess
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return done.returncode, done.stdout, done.stderr
    except FileNotFoundError:
        return 127, "", "not found: %s" % cmd[0]
    except subprocess.TimeoutExpired:
        return 124, "", "timed out"


def curl(url, proxy=None, timeout=20):
    """One request, forced through `proxy` or forced around it. Returns (status, body, err)."""
    cmd = ["curl", "-sS", "--max-time", str(timeout)]
    if proxy:
        cmd += ["--proxy", proxy]
    else:
        cmd += ["--noproxy", "*"]
    code, help_out, _ = run(["curl", "--help", "all"], timeout=20)
    if "--ssl-no-revoke" in help_out:
        cmd.append("--ssl-no-revoke")  # Schannel only; other curls reject the flag
    cmd += ["-w", "\n%{http_code}", url]
    code, out, err = run(cmd, timeout=timeout + 10)
    if code != 0:
        return None, None, (err or out).strip().splitlines()[-1] if (err or out).strip() else "curl failed"
    body, _, status = out.rpartition("\n")
    if not status.strip().isdigit():
        return None, body.strip(), "no status line"
    return int(status.strip()), body.strip(), None


def exit_address(proxy=None, timeout=20):
    """How this machine appears from outside, seen through `proxy` or around it."""
    tried = []
    for url in ECHO_HOSTS:
        status, body, err = curl(url, proxy=proxy, timeout=timeout)
        if err or status != 200 or not body:
            tried.append("%s: %s" % (urllib.parse.urlsplit(url).hostname, err or "HTTP %s" % status))
            continue
        return body.split()[0].strip(), url
    return None, "; ".join(tried) or "no echo service answered"


def proxy_url_for(config):
    """The address to point a client at.

    Routing happens inside this proxy, so a client always talks to the same local port
    whatever upstream the rules choose. That is the reason a per-upstream port would be
    wrong: it would move the routing decision into the client's configuration.
    """
    listen = config.get("listen", {})
    return "http://%s:%d" % (listen.get("host", "127.0.0.1"), listen.get("port", 7899))


def server_is_listening(config, timeout=1.0):
    listen = config.get("listen", {})
    try:
        sock = socket.create_connection((listen.get("host", "127.0.0.1"), listen.get("port", 7899)),
                                        timeout=timeout)
        sock.close()
        return True
    except OSError:
        return False


# --- commands -----------------------------------------------------------------


def out(line=""):
    print(line, flush=True)


def show_rules(config):
    upstreams = config["upstreams"]
    for spec in upstreams.values():
        kind = spec["kind"]
        where = spec.get("address", "") if kind != "direct" else ""
        out("upstream  %-10s %-7s %s" % (spec.get("_name", ""), kind, where))
    out()
    for index, rule in enumerate(config["rules"], 1):
        what = rule.get("value", "everything else") if rule["type"] != "all" else "everything else"
        out("%2d  %-7s %-32s -> %s" % (index, rule["type"], what, rule["via"]))


def list_rules(config):
    for index, rule in enumerate(config["rules"], 1):
        what = "everything else" if rule["type"] == "all" else rule.get("value", "")
        out("%2d  %-7s %-34s -> %s" % (index, rule["type"], what, rule["via"]))
    out()
    out("upstreams: %s" % ", ".join("%s(%s)" % (k, v["kind"]) for k, v in config["upstreams"].items()))


def add_rule(config, rule_type, value, via):
    if via not in config["upstreams"]:
        out("no upstream named %r; defined: %s" % (via, ", ".join(config["upstreams"])))
        return 1
    if rule_type not in ("exact", "suffix", "prefix", "cidr"):
        out("rule type must be exact, suffix, prefix or cidr")
        return 1
    # Before the catch-all, so the new rule can actually take effect.
    config["rules"].insert(max(0, len(config["rules"]) - 1),
                           {"type": rule_type, "value": value, "via": via})
    save_config(config)
    out("added   %-7s %-30s -> %s" % (rule_type, value, via))
    out("        the running proxy reads rules at startup; restart it to apply")
    return 0


def remove_rule(config, value):
    kept = [r for r in config["rules"]
            if r.get("type") == "all" or (r.get("value") or "").lower() != value.lower()]
    if len(kept) == len(config["rules"]):
        out("no rule matches %r" % value)
        return 1
    config["rules"] = kept
    save_config(config)
    out("removed %s (%d rule(s) left)" % (value, len(kept)))
    return 0


def do_check(config, host, timeout=20):
    """Prove whether a host is routed, from a rule evaluation and a measurement."""
    target = urllib.parse.urlsplit(host if "://" in host else "https://" + host)
    name = target.hostname
    if not name:
        out("cannot read a host out of %r" % host)
        return 2

    ip = None
    if looks_like_ip(name):
        ip = name
    upstream, rule = pick(config, name, ip)
    url = proxy_url_for(config)

    out("host              %s" % name)
    out("rule              %s -> %s" % (
        "everything else" if rule["type"] == "all" else "%s %s" % (rule["type"], rule["value"]),
        upstream))
    out("upstream          %s (%s)" % (upstream, config["upstreams"][upstream]["kind"]))
    out("client points at  %s" % url)

    listening = server_is_listening(config)
    out("proxy listening   %s" % ("yes" if listening else "no (start it with: proxy serve)"))
    if not listening:
        out()
        out("verdict           NOT RUNNING")
        out("                  next: proxy serve")
        return 1

    # Through the proxy the routing rule applies; around it the environment still does,
    # so NO_PROXY is what makes the second measurement a real comparison.
    through_ip, through_via = exit_address(proxy=url, timeout=timeout)
    direct_ip, direct_err = exit_address(proxy=None, timeout=timeout)
    out("exit via proxy    %s" % (through_ip or "unknown (%s)" % through_via))
    out("exit without      %s" % (direct_ip or "unknown (%s)" % direct_err))

    status, _, request_err = curl("https://" + name, proxy=url, timeout=timeout)
    out("request           %s" % (("HTTP %s" % status) if status else "FAILED: %s" % request_err))

    # The exit address belongs to the echo host, not to this target. It only says
    # something about the target when both are routed by the same rule -- otherwise it
    # measures a different route and reports it as if it were this one.
    echo_host = urllib.parse.urlsplit(through_via).hostname if through_via else None
    echo_rule = pick(config, echo_host)[1] if echo_host else None
    same_route = echo_rule == rule
    if echo_host and not same_route:
        out("exit note         %s follows a different rule, so that address says nothing"
            % echo_host)
        out("                  about %s; the decision log below is what settles it" % name)

    # What the proxy actually did with this host, from the code that chose the route.
    entries = [e for e in Decisions().recent(200) if e.get("host") == name]
    last = entries[-1] if entries else None
    if last:
        out("proxy decision    %s via %s%s" % (last["host"], last["via"],
                                                "" if last["phase"] == "open" else " [%s]" % last.get("error")))

    out()
    if not status:
        out("verdict           REQUEST FAILED")
        out("                  %s through the proxy: %s" % (name, request_err))
        out("                  next: proxy doctor")
        return 1
    if last is None:
        out("verdict           ROUTED (no decision recorded; the log may be elsewhere)")
        return 0
    if last["phase"] != "open":
        out("verdict           ROUTE FAILED")
        out("                  rule sent %s to %s, which did not connect: %s"
            % (name, last["via"], last.get("error")))
        out("                  next: proxy doctor, or proxy rm %s" % name)
        return 1
    if last["via"] == "direct":
        out("verdict           DIRECT")
        out("                  %s was connected without any upstream" % name)
        return 0
    if same_route and direct_ip and through_ip and through_ip != direct_ip:
        out("verdict           ROUTED VIA %s" % last["via"])
        out("                  the proxy exits as %s, without it as %s" % (through_ip, direct_ip))
    else:
        out("verdict           ROUTED VIA %s" % last["via"])
        out("                  the proxy opened %s through that upstream and the request"
            % name)
        out("                  succeeded; the exit address belongs to a different rule")
    return 0


def do_doctor(config):
    problems = 0

    def line(ok, what, detail=""):
        nonlocal problems
        if not ok:
            problems += 1
        out("%s %-32s %s" % ("ok  " if ok else "FAIL", what, detail))

    code, version, _ = run(["curl", "--version"], timeout=20)
    line(code == 0, "curl present", version.splitlines()[0] if code == 0 else "missing")
    line(os.path.exists(CONFIG_PATH), "config present", CONFIG_PATH)
    try:
        validate_config(config)
        line(True, "config valid", "%d rule(s), %d upstream(s)"
             % (len(config["rules"]), len(config["upstreams"])))
    except ValueError as exc:
        line(False, "config valid", str(exc))
    line(config["rules"][-1]["type"] == "all", "catch-all rule last", "every host has a route")

    url = proxy_url_for(config)
    listening = server_is_listening(config)
    line(listening, "proxy listening", url if listening else "not started (proxy serve)")

    # An upstream is working when it carries a tunnel, not when its port answers:
    # a proxy that accepts connections and refuses CONNECT looks alive and is useless.
    for name, spec in config["upstreams"].items():
        if spec["kind"] == "direct":
            continue
        try:
            probe = tunnel(config, name, "example.com", 443, timeout=10)
            probe.close()
            line(True, "upstream %s carries a tunnel" % name,
                 "%s %s" % (spec["kind"], spec["address"]))
        except OSError as exc:
            line(False, "upstream %s carries a tunnel" % name, "%s: %s" % (spec["address"], exc))

    if listening:
        through_ip, echoed = exit_address(proxy=url)
        line(through_ip is not None, "traffic exits through proxy", through_ip or echoed)
        # The exit address describes the echo host's route. Comparing it with the direct
        # address only means something when some rule sends that host through an upstream;
        # otherwise the same address on both sides is correct, not a failure.
        echo_host = urllib.parse.urlsplit(echoed).hostname if echoed else None
        routed_echo = echo_host and pick(config, echo_host)[0] != "direct"
        if routed_echo:
            direct_ip, _ = exit_address(proxy=None)
            if through_ip and direct_ip:
                line(through_ip != direct_ip, "exit differs from direct",
                     "%s vs %s" % (through_ip, direct_ip))
        else:
            line(True, "exit comparison",
                 "not applicable: no rule routes %s, so both measurements are direct"
                 % (echo_host or "an echo host"))

    out()
    out("verdict: %s" % ("usable" if problems == 0 else "NOT usable, see above"))
    return 0 if problems == 0 else 1


def run_core(argv):
    """Hand `proxy core ...` to the core-API client.

    The client lives in its own module because it talks to a different thing -- a
    running routing core -- but it is reached through the same entry point, so there is
    one name to remember rather than one per layer.
    """
    import importlib.util
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location("mihomo", os.path.join(here, "mihomo.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.run(argv)


def main(argv):
    if argv and argv[0] == "core":
        return run_core(argv[1:])
    parser = argparse.ArgumentParser(
        prog="proxy",
        description="per-host routing proxy; `proxy core ...` drives a running mihomo core",
        epilog="core subcommands: doctor, groups, nodes GROUP, use GROUP NODE, "
               "bench [GROUP] [--throughput], mode [rule|global|direct], reload [PATH], "
               "connect HOST:PORT --save",
    )
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("serve", help="run the proxy in the foreground")
    sub.add_parser("rules", help="show the rule table")
    sub.add_parser("status", help="config, listener and last decisions")
    sub.add_parser("doctor", help="check every link in the chain")
    sub.add_parser("env", help="shell exports pointing clients at this proxy")
    p = sub.add_parser("add", help="route a host through an upstream")
    p.add_argument("value")
    p.add_argument("--via", required=True, help="upstream name from the config")
    p.add_argument("--type", default="suffix", choices=["exact", "suffix", "prefix", "cidr"])
    p = sub.add_parser("rm", help="remove a rule by its value")
    p.add_argument("value")
    p = sub.add_parser("check", help="is this host actually routed? measured")
    p.add_argument("host")
    p = sub.add_parser("upstream", help="add or list upstreams")
    p.add_argument("action", nargs="?", default="list", choices=["list", "add"])
    p.add_argument("name", nargs="?")
    p.add_argument("address", nargs="?")
    p.add_argument("--kind", default="http", choices=["direct", "http", "socks5"])
    p.add_argument("--auth", help="user:password for the upstream")

    args = parser.parse_args(argv)
    if not args.cmd:
        parser.print_help()
        return 0

    config = load_config()

    if args.cmd == "serve":
        server = ProxyServer(config)
        out("listening on %s" % proxy_url_for(config))
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            server.stop()
        return 0

    if args.cmd == "rules":
        list_rules(config)
        return 0

    if args.cmd == "status":
        out("config            %s" % CONFIG_PATH)
        out("listen            %s" % proxy_url_for(config))
        out("listening         %s" % ("yes" if server_is_listening(config) else "no"))
        out("rules             %d" % len(config["rules"]))
        out("upstreams         %s" % ", ".join(config["upstreams"]))
        recent = Decisions().recent(10)
        if recent:
            out()
            out("recent decisions:")
            for entry in recent:
                out("  %s %-40s -> %s%s" % (entry["at"], "%s:%s" % (entry["host"], entry["port"]),
                                            entry["via"], (" [%s]" % entry.get("error")) if entry.get("error") else ""))
        return 0

    if args.cmd == "doctor":
        return do_doctor(config)

    if args.cmd == "env":
        url = proxy_url_for(config)
        out('export HTTPS_PROXY="%s"' % url)
        out('export HTTP_PROXY="%s"' % url)
        out('export ALL_PROXY="%s"' % url)
        out('export NO_PROXY="localhost,127.0.0.1,::1"')
        out()
        out("# uv and pip read the environment; git does not")
        out("git config --global http.proxy %s" % url)
        return 0

    if args.cmd == "add":
        return add_rule(config, args.type, args.value, args.via)
    if args.cmd == "rm":
        return remove_rule(config, args.value)
    if args.cmd == "check":
        return do_check(config, args.host)

    if args.cmd == "upstream":
        if args.action == "list":
            for name, spec in config["upstreams"].items():
                out("%-12s %-7s %s" % (name, spec["kind"], spec.get("address", "")))
            return 0
        if not args.name:
            out("usage: proxy upstream add NAME ADDRESS --kind http|socks5 [--auth user:pass]")
            return 2
        spec = {"kind": args.kind}
        if args.kind != "direct":
            spec["address"] = args.address
        if args.auth:
            spec["auth"] = args.auth
        config["upstreams"][args.name] = spec
        save_config(config)
        out("upstream %s = %s %s" % (args.name, args.kind, args.address or ""))
        return 0

    parser.print_help()
    return 2


def reader_went_away(error):
    """True when the reader closed the pipe -- `proxy rules | head`.

    Windows reports this as OSError(22), not BrokenPipeError. mihomo.py carries the
    same guard because it is also runnable on its own.
    """
    if isinstance(error, BrokenPipeError):
        return True
    return isinstance(error, OSError) and getattr(error, "errno", None) == 22


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except OSError as error:
        if not reader_went_away(error):
            raise
        # Sink stdout so the flush during shutdown does not hit the closed pipe again.
        try:
            sink = os.open(os.devnull, os.O_WRONLY)
            os.dup2(sink, sys.stdout.fileno())
        except OSError:
            pass
        sys.exit(0)
