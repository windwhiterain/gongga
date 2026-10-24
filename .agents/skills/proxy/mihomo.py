# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""Drive a running mihomo core through its own API: nodes, selection, modes, benches.

A core like this is two things at once -- a router and a catalog of routes -- and the
catalog is reachable over HTTP while it runs. That API is what makes the difference
between "the proxy is on" and "this group routes through that node", and it is the part
worth not losing the ability to use.

Discovery deliberately ranks the core's own configuration above any third-party client's
bookkeeping: the core can be started by hand from its config file, and reading where it
actually listens means this works with no client installed at all.

Three things here are easy to get wrong, so they are handled explicitly:

  * A "node" list can contain entries that are not routes -- a vendor notice, or a
    reference to another policy group. Selecting one silently breaks every group that
    inherits from it, so these are refused rather than resolved.
  * A latency probe measures the core's own round trip through a node, which is
    independent of this machine's rules. A throughput transfer is not: in `rule` mode the
    test URL may match a direct rule, and the number then describes the local line while
    the label says the node's name. The transfer switches the core to `global` mode so
    the bytes provably traverse the selected node, and restores the previous mode and
    selection even when a transfer fails.
  * A transfer stopped by its own time window still delivered bytes, and those bytes are
    the measurement. The exit code is not consulted; the delivered size is.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import urllib.error
import urllib.parse
import unicodedata
import urllib.request

DEFAULT_PORT = 9090
DEFAULT_TEST_URL = "http://cp.cloudflare.com/generate_204"
DEFAULT_SPEED_URL = "https://speed.cloudflare.com/__down?bytes=20000000"
CACHE_PATH = os.path.join(os.path.expanduser("~"), ".proxy", "mihomo.json")
PROBE_PORTS = (9090, 9091, 9097, 6170)
GROUP_TYPES = ("Selector", "URLTest", "Fallback", "LoadBalance")
NOTICE_PATTERN = re.compile(r"未读|公告|notice", re.I)
NULL_DEVICE = "NUL" if os.name == "nt" else "/dev/null"


class CoreError(Exception):
    pass


# --- discovery -----------------------------------------------------------------


def parse_core_config(text):
    """Scrape `external-controller` and `secret` out of a core configuration.

    These two keys are the whole of what the API needs, so scanning for them beats
    parsing the document: a real YAML parser would be a dependency for two lines, and
    this stays correct on the enormous vendor-generated files these cores ship with.
    """
    controller = None
    secret = None
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or ":" not in stripped:
            continue
        key, _, value = stripped.partition(":")
        key = key.strip()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key == "external-controller" and value:
            controller = value
        elif key == "secret" and value:
            secret = value
    return controller, secret


def origin_from_controller(value, default_port=DEFAULT_PORT):
    """Turn a controller address into an origin, filling in the parts people omit."""
    text = (value or "").strip()
    if not text:
        return None
    if "://" not in text:
        text = "http://" + text
    parts = urllib.parse.urlsplit(text)
    host = parts.hostname or "127.0.0.1"
    port = parts.port or default_port
    return "http://%s:%d" % (host, port)


def core_config_paths():
    home = os.path.expanduser("~")
    return [
        os.path.join(home, ".config", "mihomo", "config.yaml"),
        os.path.join(home, ".config", "mihomo", "config.yml"),
        os.path.join(home, ".kualemon", "mihomo", "config.yaml"),
        os.path.join(home, "mihomo", "config.yaml"),
        "/etc/mihomo/config.yaml",
    ]


def read_core_config(path):
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return parse_core_config(handle.read())
    except OSError:
        return None, None


def read_cache():
    try:
        with open(CACHE_PATH, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    origin = origin_from_controller(data.get("controller"))
    if not origin:
        return None
    return {
        "origin": origin,
        "secret": data.get("secret") or "",
        "source": "saved cache %s" % CACHE_PATH,
        "config": data.get("config"),
    }


def discover(explicit_controller=None, explicit_secret=None):
    """Find a controller, best source first, and say which source was used.

    Order: an explicit argument, then the environment, then a saved cache, then the
    core's own config file, then a vendor client's state file, then a port probe. The
    core's config outranks the client's bookkeeping on purpose -- the core is the thing
    being talked to, and it may well have been started without any client.
    """
    if explicit_controller:
        return {
            "origin": origin_from_controller(explicit_controller),
            "secret": explicit_secret or "",
            "source": "explicit --controller",
            "config": None,
        }

    env_controller = os.environ.get("MIHOMO_CONTROLLER")
    if env_controller:
        return {
            "origin": origin_from_controller(env_controller),
            "secret": os.environ.get("MIHOMO_SECRET", ""),
            "source": "MIHOMO_CONTROLLER",
            "config": None,
        }

    cached = read_cache()
    if cached:
        return cached

    for path in core_config_paths():
        controller, secret = read_core_config(path)
        if controller:
            return {
                "origin": origin_from_controller(controller),
                "secret": secret or "",
                "source": path,
                "config": path,
            }

    # A vendor client stores the secret it generated; reading it is a convenience, and
    # everything above already works when that client is not installed.
    state_path = os.path.join(os.path.expanduser("~"), ".kualemon", "state.json")
    try:
        with open(state_path, "r", encoding="utf-8") as handle:
            state = json.load(handle)
    except (OSError, ValueError):
        state = {}
    core = state.get("core") or {}
    if core.get("controllerPort"):
        return {
            "origin": origin_from_controller("127.0.0.1:%s" % core["controllerPort"]),
            "secret": state.get("controllerSecret") or "",
            "source": "client state %s" % state_path,
            "config": core.get("configPath"),
        }

    for port in PROBE_PORTS:
        origin = "http://127.0.0.1:%d" % port
        try:
            Core(origin, "", timeout=1.5).version()
        except CoreError:
            continue
        return {"origin": origin, "secret": "", "source": "port probe", "config": None}

    return None


def save_cache(discovered):
    os.makedirs(os.path.dirname(CACHE_PATH), exist_ok=True)
    with open(CACHE_PATH, "w", encoding="utf-8") as handle:
        json.dump({"controller": discovered["origin"], "secret": discovered.get("secret", ""), "config": discovered.get("config")}, handle, indent=2)
        handle.write("\n")


# --- the core ------------------------------------------------------------------


class Core(object):
    def __init__(self, origin, secret="", timeout=10.0):
        self.origin = origin.rstrip("/")
        self.secret = secret or ""
        self.timeout = timeout

    def request(self, path, method="GET", body=None, timeout=None):
        data = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(self.origin + path, data=data, method=method)
        request.add_header("Content-Type", "application/json")
        if self.secret:
            request.add_header("Authorization", "Bearer " + self.secret)
        try:
            with urllib.request.urlopen(request, timeout=timeout or self.timeout) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            detail = error.read()[:200].decode("utf-8", "replace")
            if error.code in (401, 403):
                raise CoreError("the core rejected the secret (HTTP %d)" % error.code)
            raise CoreError("%s %s -> HTTP %d %s" % (method, path, error.code, detail))
        except OSError as error:
            raise CoreError("no answer from %s: %s" % (self.origin, error))
        if not raw:
            return None
        try:
            return json.loads(raw.decode("utf-8"))
        except ValueError:
            raise CoreError("%s %s returned something that is not JSON" % (method, path))

    def version(self):
        data = self.request("/version", timeout=3.0)
        if isinstance(data, dict):
            return data.get("version") or "?"
        return data or "?"

    def configs(self):
        return self.request("/configs")

    def proxies(self):
        return (self.request("/proxies") or {}).get("proxies") or {}

    def select(self, group, node):
        self.request("/proxies/" + urllib.parse.quote(group, safe=""), method="PUT", body={"name": node})

    def delay(self, node, timeout_ms=5000, url=DEFAULT_TEST_URL):
        query = "?timeout=%d&url=%s" % (timeout_ms, urllib.parse.quote(url, safe=""))
        data = self.request("/proxies/" + urllib.parse.quote(node, safe="") + "/delay" + query, timeout=(timeout_ms / 1000.0) + 5)
        if not data or "delay" not in data:
            raise CoreError("no delay reported")
        return data["delay"]

    def set_mode(self, mode):
        self.request("/configs", method="PATCH", body={"mode": mode})

    def reload(self, path):
        self.request("/configs?force=true", method="PUT", body={"path": path})


# --- pure decisions ------------------------------------------------------------


def is_notice(name):
    """A vendor sometimes ships a notice as a selectable entry. It is not a route."""
    return bool(NOTICE_PATTERN.search(name or ""))


def groups_of(proxies):
    return {name: proxy for name, proxy in proxies.items() if proxy.get("type") in GROUP_TYPES}


def is_group_reference(proxies, name):
    """A group's choice list may name another group. Selecting one is legal but it is
    not a node, so anything measuring a node has to tell them apart."""
    entry = proxies.get(name) or {}
    return entry.get("type") in GROUP_TYPES


def resolve_group(proxies, query):
    groups = groups_of(proxies)
    if query in groups:
        return query, groups[query]
    matches = [name for name in groups if query.lower() in name.lower()]
    if not matches:
        raise CoreError('no policy group matches "%s"; try `mihomo groups`' % query)
    if len(matches) > 1:
        raise CoreError('"%s" matches %d groups: %s' % (query, len(matches), ", ".join(sorted(matches))))
    return matches[0], groups[matches[0]]


def resolve_node(group, query):
    choices = group.get("all") or []
    if query in choices:
        return query
    matches = [choice for choice in choices if query.lower() in choice.lower()]
    if not matches:
        raise CoreError('"%s" is not one of the %d choices in this group' % (query, len(choices)))
    if len(matches) > 1:
        shown = "\n  ".join(matches[:10])
        raise CoreError('"%s" matches %d choices:\n  %s' % (query, len(matches), shown))
    return matches[0]


def sort_bench(rows):
    """Reachable nodes first, fastest first; failures last so a timeout never sorts
    ahead of a working node by accident of being zero."""
    return sorted(rows, key=lambda row: (not row["ok"], row["delay"] if row["ok"] else 0, row["node"]))


def throughput_of(byte_count, seconds):
    if not seconds or seconds <= 0:
        return 0.0
    return byte_count / seconds / 1024.0


def node_choices(proxies, group):
    """The real nodes in a group: no notices, no references to other groups."""
    return [
        choice
        for choice in (group.get("all") or [])
        if not is_notice(choice) and not is_group_reference(proxies, choice)
    ]


# --- benches -------------------------------------------------------------------


def bench_latency(core, group_query, concurrency=4, timeout_ms=5000, url=DEFAULT_TEST_URL, limit=None):
    proxies = core.proxies()
    name, group = resolve_group(proxies, group_query)
    choices = node_choices(proxies, group)[:limit] if limit else node_choices(proxies, group)
    if not choices:
        raise CoreError('group "%s" has no measurable nodes' % name)

    rows = []
    pending = list(choices)
    lock = threading.Lock()

    def drain():
        while True:
            with lock:
                if not pending:
                    return
                node = pending.pop(0)
            try:
                rows.append({"node": node, "delay": core.delay(node, timeout_ms, url), "ok": True})
            except CoreError as error:
                rows.append({"node": node, "delay": None, "ok": False, "error": str(error)})

    threads = [threading.Thread(target=drain) for _ in range(min(concurrency, len(choices)))]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    return name, sort_bench(rows)


def measure_transfer(mixed_port, seconds, url):
    """Download through the core's mixed port for a fixed window and report the rate.

    The stop is the window, not the end of the file, so a non-zero exit code is normal
    and the delivered size is still the measurement.
    """
    curl = shutil.which("curl")
    if not curl:
        raise CoreError("curl is not on PATH, and it is what makes a real transfer")
    command = [
        curl, "--silent", "--no-progress-meter", "--location",
        "--max-time", str(seconds),
        "--proxy", "http://127.0.0.1:%d" % mixed_port,
        "--output", NULL_DEVICE,
        "--write-out", "%{size_download}",
        url,
    ]
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=seconds + 20)
    except subprocess.TimeoutExpired:
        raise CoreError("the transfer did not stop after %ds" % (seconds + 20))
    try:
        delivered = int(float((completed.stdout or "0").strip() or 0))
    except ValueError:
        delivered = 0
    return delivered, throughput_of(delivered, seconds)


def bench_throughput(core, group_query, seconds=8, url=DEFAULT_SPEED_URL, limit=None):
    """Measure sustained bandwidth per node by putting the bytes through the node.

    Pinning a policy group is not sufficient evidence: under `rule` or `whitelist`
    routing the test URL may match a direct rule, and the transfer then measures this
    machine's own line while the label claims a node. `global` mode takes the rules out
    of the decision, so every byte provably traverses the selected node.

    The previous mode and selection come back in `finally` -- a bench that leaves the
    core in global mode has changed routing for everything else on the machine.
    """
    configs = core.configs() or {}
    mixed_port = configs.get("mixed-port") or configs.get("port")
    if not mixed_port:
        raise CoreError("the core reports no mixed port to send transfers through")

    proxies = core.proxies()
    name, group = resolve_group(proxies, group_query)
    choices = node_choices(proxies, group)
    if limit:
        choices = choices[:limit]
    if not choices:
        raise CoreError('group "%s" has no measurable nodes' % name)

    mode_before = configs.get("mode") or "rule"
    global_group = proxies.get("GLOBAL") or {}
    selection_before = global_group.get("now")

    rows = []
    try:
        core.set_mode("global")
        for node in choices:
            try:
                core.select("GLOBAL", node)
                delivered, rate = measure_transfer(mixed_port, seconds, url)
                rows.append({"node": node, "bytes": delivered, "kbps": rate, "ok": delivered > 0})
            except CoreError as error:
                rows.append({"node": node, "bytes": 0, "kbps": 0.0, "ok": False, "error": str(error)})
    finally:
        if selection_before:
            try:
                core.select("GLOBAL", selection_before)
            except CoreError:
                pass
        try:
            core.set_mode(mode_before)
        except CoreError:
            pass
    rows.sort(key=lambda row: (not row["ok"], -row["kbps"], row["node"]))
    return name, rows


# --- commands ------------------------------------------------------------------


def out(line=""):
    print(line, flush=True)


def display_width(text):
    """How many terminal cells a string occupies.

    `%-*s` counts code points, so a Chinese node name or an emoji -- both of which this
    catalog is full of -- shifts every column after it. Widths come from the character
    database instead of from len().
    """
    width = 0
    for character in text:
        if unicodedata.combining(character):
            continue
        width += 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1
    return width


def pad(text, width):
    return text + " " * max(0, width - display_width(text))


def column(width, header, value):
    return pad(header, width) + "  " + pad(str(value), width)


def table(width, header, rows):
    return [column(width, header, row) for row in rows]


def connect_from(discovered):
    if not discovered:
        raise CoreError("no controller found; pass --controller HOST:PORT, or set MIHOMO_CONTROLLER")
    return Core(discovered["origin"], discovered.get("secret", ""))


def cmd_doctor(discovered, core):
    out("controller        %s" % discovered["origin"])
    out("found via         %s" % discovered["source"])
    out("secret            %s" % ("set" if discovered.get("secret") else "none"))
    if discovered.get("config"):
        out("core config       %s" % discovered["config"])
    out("version           %s" % core.version())
    configs = core.configs() or {}
    mixed = configs.get("mixed-port") or 0
    out("mode              %s" % (configs.get("mode") or "?"))
    out("mixed port        %s" % (mixed or "not set"))
    groups = groups_of(core.proxies())
    out("policy groups     %d" % len(groups))
    if mixed:
        try:
            measure_transfer(mixed, 1, "http://cp.cloudflare.com/generate_204")
            out("mixed port routes yes")
        except CoreError as error:
            out("mixed port routes no (%s)" % error)
    out("")
    out("verdict           CONTROLLER REACHABLE")


def cmd_groups(core):
    groups = groups_of(core.proxies())
    rows = []
    for name, group in sorted(groups.items()):
        selected = group.get("now") or "-"
        note = " [notice, not a route]" if is_notice(selected) else ""
        rows.append([name, group.get("type"), selected + note, str(len(group.get("all") or []))])
    header = ["GROUP", "TYPE", "ROUTES THROUGH", "CHOICES"]
    widths = [max(display_width(row[index]) for row in rows + [header]) for index in range(4)]
    line = lambda row: "  ".join(pad(cell, widths[index]) for index, cell in enumerate(row)).rstrip()
    out(line(header))
    for row in rows:
        out(line(row))
    return 0


def cmd_nodes(core, query):
    proxies = core.proxies()
    name, group = resolve_group(proxies, query)
    selected = group.get("now")
    out("%s (%s) routes through %s" % (name, group.get("type"), selected))
    out("")
    for choice in group.get("all") or []:
        marks = []
        if choice == selected:
            marks.append("<- current")
        if is_notice(choice):
            marks.append("[notice, not a route]")
        if is_group_reference(proxies, choice):
            marks.append("[group reference]")
        out("  %s%s" % (choice, ("  " + " ".join(marks)) if marks else ""))
    return 0


def cmd_use(core, group_query, node_query):
    proxies = core.proxies()
    name, group = resolve_group(proxies, group_query)
    node = resolve_node(group, node_query)
    if is_notice(node):
        raise CoreError('"%s" is a notice entry, not a route; pick a real node' % node)
    previous = group.get("now")
    core.select(name, node)
    after = (core.proxies().get(name) or {}).get("now")
    if after != node:
        raise CoreError("the core did not take the change: still on %s" % after)
    out("%s -> %s" % (name, node))
    out("was               %s" % previous)
    out("verified          reading the group back reports %s" % after)
    return 0


def cmd_bench(core, group_query, throughput, seconds, url, concurrency, limit, chosen_by_default=False):
    if throughput:
        name, rows = bench_throughput(core, group_query, seconds, url, limit)
        out("group             %s%s" % (name, " (default: most real nodes)" if chosen_by_default else ""))
        out("measured          %ds per node, in global mode" % seconds)
        out("")
        body = []
        for row in rows:
            delivered = "%.1f MB" % (row["bytes"] / 1e6) if row["ok"] else "failed"
            rate = ("%d KB/s" % row["kbps"]) if row["ok"] else (row.get("error") or "")[:48]
            body.append([row["node"], delivered, rate])
        width = max(display_width(row[0]) for row in body)
        out("  " + pad("NODE", width) + "  " + pad("DELIVERED", 10) + "  RATE")
        for node, delivered, rate in body:
            out("  " + pad(node, width) + "  " + pad(delivered, 10) + "  " + rate)
        working = sum(1 for row in rows if row["ok"])
        out("")
        out("%d of %d delivered traffic" % (working, len(rows)))
        return 0

    name, rows = bench_latency(core, group_query, concurrency, limit=limit)
    out("group             %s%s" % (name, " (default: the group with the most real nodes)" if chosen_by_default else ""))
    out("")
    width = max(display_width(row["node"]) for row in rows)
    out("  " + pad("NODE", width) + "  DELAY")
    for row in rows:
        out("  " + pad(row["node"], width) + "  " + (("%d ms" % row["delay"]) if row["ok"] else "timeout"))
    reachable = sum(1 for row in rows if row["ok"])
    out("")
    out("%d of %d reachable" % (reachable, len(rows)))
    out("")
    out("note              latency says nothing about bandwidth; add --throughput to see it")
    return 0


def cmd_mode(core, mode):
    configs = core.configs() or {}
    before = configs.get("mode")
    if not mode:
        out("mode              %s" % before)
        return 0
    if mode not in ("rule", "global", "direct"):
        raise CoreError("mode must be rule, global or direct")
    core.set_mode(mode)
    after = (core.configs() or {}).get("mode")
    if after != mode:
        raise CoreError("the core did not take the mode: still %s" % after)
    out("mode %s -> %s" % (before, after))
    return 0


def cmd_reload(core, discovered, path):
    target = path or discovered.get("config")
    if not target:
        raise CoreError("no config path known; pass one explicitly")
    if not os.path.exists(target):
        raise CoreError("no such file: %s" % target)
    core.reload(target)
    out("reloaded %s" % target)
    out("verified          the core answered; a policy change now reaches live traffic")
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="proxy core", description="drive a mihomo core through its API")
    parser.add_argument("--controller", help="host:port of the running core (default: discover)")
    parser.add_argument("--secret", help="API secret, when one is set")
    sub = parser.add_subparsers(dest="cmd")

    sub.add_parser("doctor", help="where the controller is, and whether it answers")
    sub.add_parser("groups", help="policy groups and what each routes through")
    p = sub.add_parser("nodes", help="the choices in one group")
    p.add_argument("group")
    p = sub.add_parser("use", help="point a group at a node, then read it back")
    p.add_argument("group")
    p.add_argument("node")
    p = sub.add_parser("bench", help="latency, or --throughput for sustained bandwidth")
    p.add_argument("group", nargs="?", help="policy group (default: largest)")
    p.add_argument("--throughput", action="store_true")
    p.add_argument("--seconds", type=int, default=8)
    p.add_argument("--url", default=None)
    p.add_argument("--concurrency", type=int, default=4)
    p.add_argument("--limit", type=int, default=None, help="measure only the first N nodes")
    p = sub.add_parser("mode", help="rule, global, direct, or nothing to report it")
    p.add_argument("mode", nargs="?")
    p = sub.add_parser("reload", help="apply an edited config without restarting")
    p.add_argument("path", nargs="?")
    p = sub.add_parser("connect", help="remember a controller address and secret")
    p.add_argument("controller")
    p.add_argument("--secret", default=None)
    p.add_argument("--save", action="store_true")

    args = parser.parse_args(argv)
    if not args.cmd:
        parser.print_help()
        return 0

    discovered = discover(args.controller, args.secret)
    if args.cmd == "connect":
        discovered = {
            "origin": origin_from_controller(args.controller),
            "secret": args.secret or "",
            "source": "explicit --controller",
            "config": None,
        }
        Core(discovered["origin"], discovered["secret"]).version()
        out("controller        %s" % discovered["origin"])
        out("secret            %s" % ("set" if discovered["secret"] else "none"))
        out("reachable         yes")
        if args.save:
            save_cache(discovered)
            out("saved             %s" % CACHE_PATH)
        return 0

    if not discovered:
        raise CoreError("no controller found; pass --controller HOST:PORT, or set MIHOMO_CONTROLLER")
    core = connect_from(discovered)

    if args.cmd == "doctor":
        return cmd_doctor(discovered, core) or 0
    if args.cmd == "groups":
        return cmd_groups(core)
    if args.cmd == "nodes":
        return cmd_nodes(core, args.group)
    if args.cmd == "use":
        return cmd_use(core, args.group, args.node)
    if args.cmd == "bench":
        chosen_by_default = args.group is None
        group = args.group or largest_group(core)
        return cmd_bench(core, group, args.throughput, args.seconds, args.url or (DEFAULT_SPEED_URL if args.throughput else DEFAULT_TEST_URL), args.concurrency, args.limit, chosen_by_default)
    if args.cmd == "mode":
        return cmd_mode(core, args.mode)
    if args.cmd == "reload":
        return cmd_reload(core, discovered, args.path)
    parser.print_help()
    return 0


def largest_group(core):
    """Default bench target: the group with the most real nodes, which is where the
    useful candidates live."""
    proxies = core.proxies()
    best = None
    for name, group in groups_of(proxies).items():
        if name == "GLOBAL":
            continue
        count = len(node_choices(proxies, group))
        if best is None or count > best[1]:
            best = (name, count)
    if not best:
        raise CoreError("no policy group with nodes")
    return best[0]


def reader_went_away(error):
    """True when stdout was closed by the thing at the other end of the pipe.

    `proxy core groups | head` closes the pipe, and Windows reports that as
    OSError(22) rather than BrokenPipeError. It is not an error in what we did.
    """
    if isinstance(error, BrokenPipeError):
        return True
    return isinstance(error, OSError) and getattr(error, "errno", None) == 22


def run(argv):
    """The one place that turns a failure into an exit code.

    Both `python mihomo.py` and `proxy core …` come through here: an error handler that
    only exists in one of the two entry points is an error handler that does not run.
    """
    try:
        return main(argv)
    except CoreError as error:
        print("proxy core: %s" % error, file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    except OSError as error:
        if not reader_went_away(error):
            raise
        # The reader closed the pipe. Point stdout at a sink first, or the flush during
        # interpreter shutdown hits the same closed pipe and complains on the way out.
        try:
            sink = os.open(os.devnull, os.O_WRONLY)
            os.dup2(sink, sys.stdout.fileno())
        except OSError:
            pass
        return 0


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
