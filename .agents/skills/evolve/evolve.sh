#!/usr/bin/env bash
# Self-evolution: replace the server that is running you.
#
# The pieces already exist, they were just never wired together:
#
#   * `cancel` stops the server but *keeps* the running turn as a pending task
#     (`session.rs::request_stop`), finishing any command already in flight so its
#     output is stored rather than reported as stopped, and the next start replays it;
#   * a detached job outlives the server that started it, which is what lets a
#     successor wait outside the process it is about to replace;
#   * `server.lock` is a pid file, so the home is handed over rather than
#     stolen, and a stale pid is recognised as litter.
#
# What was missing is one command that does the swap in the safe order: stage
# the new binary, start the successor *before* stopping the old server (so a
# bad build costs nothing), and only then cancel.
#
#   $PWD/.agents/skills/evolve/evolve.sh stage            # test, build, stage, write launcher
#   $PWD/.agents/skills/evolve/evolve.sh launch <stamp>   # start it outside this turn's job
#   $PWD/.agents/skills/evolve/evolve.sh swap <stamp>     # verify the successor, then cancel
#   $PWD/.agents/skills/evolve/evolve.sh status           # what is staged, who is waiting
#   $PWD/.agents/skills/evolve/evolve.sh clean            # kill successors nobody is going to swap
#
# Only the newest stage may take the home. `stage` writes `.dev/evolve-current`,
# a launcher whose stamp is not in there leaves the waiting loop instead of
# racing for the port, and `swap` refuses a stamp that is not current. Without
# that pointer a waiter from an abandoned attempt keeps polling `tasklist` and
# `netstat` five times a second for as long as the old server lives -- forever,
# if nobody was going to swap it -- and when the port finally frees, the *older*
# binary can be the one that wins the race.
#
# After `swap` this turn is cancelled and preserved; the successor binds the
# port, picks the pending turn up and asks the model to continue. That is the
# whole loop: the harness replaces the process that is running it.
#
# See DESIGN.md §10 for why the order matters and what is still missing.
set -euo pipefail

# The repo root, found by walking up rather than by counting `../`: this script
# lives inside a skill directory, and a depth written into the code breaks the
# first time it moves.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
while [ "$ROOT" != "/" ] && [ ! -f "$ROOT/Cargo.toml" ]; do
  ROOT="$(dirname "$ROOT")"
done
[ -f "$ROOT/Cargo.toml" ] || { echo "evolve: no Cargo.toml above $0" >&2; exit 1; }
# The handover's own scratch: staged bundles, launchers, the pointer at who may take
# the home. `GONGGA_EVOLVE_RUN` exists for the self-test, which has to drive `clean`
# against a directory of its own -- and for two worktrees on one machine, which must
# not share a pointer (`AGENTS.md`: each worktree keeps its own home).
RUN="${GONGGA_EVOLVE_RUN:-$ROOT/.dev}"
# Which stage is allowed to take the home. Written by `stage`, read by every
# launcher and by `swap`.
CURRENT="$RUN/evolve-current"
# Who is evolving at all. `$CURRENT` decides which stage may take the home, but it
# cannot keep two agents apart: it takes whoever writes it last, and the earlier
# launcher then leaves the queue, so one agent's handover is silently replaced by
# another's. AGENTS says agents must not evolve at once -- so the claim lives in a
# directory, made by an atomic `mkdir`, and it lasts as long as the handover does:
# stage, launch, and the successor that binds.
LEASE="$RUN/evolve-lease"
HOME_DIR="${GONGGA_HOME:-$HOME/.gongga}"
BIND="${GONGGA_SERVER:-127.0.0.1:9700}"
PORT="${BIND##*:}"
mkdir -p "$RUN"

# The CLI has to be reachable from a plain bash call, exactly as the model
# would call it: the name on PATH first, then the build in this repo. Both
# spellings are asked for: `gongga` is what a home installed before the command
# was renamed, and a handover has to work across that rename.
CLI="$(command -v gongga-cli || command -v gongga-cli.exe || command -v gongga || command -v gongga.exe || true)"
if [ -z "$CLI" ] && [ -x "$ROOT/target/debug/gongga-cli.exe" ]; then
  CLI="$ROOT/target/debug/gongga-cli.exe"
elif [ -z "$CLI" ] && [ -x "$ROOT/target/debug/gongga.exe" ]; then
  CLI="$ROOT/target/debug/gongga.exe"
fi

die() { echo "evolve: $*" >&2; exit 1; }

# Free space on the volume holding this repo, and how full it is.
#
# Every swap says it: iterating on a Rust tree fills a disk, and the moment to
# know is before the build that needs the room, not after it fails. `df` is in
# Git Bash; a platform without it prints nothing rather than failing a handover.
disk_line() {
  local out
  out="$(df -h "$ROOT" 2>/dev/null | tail -n 1)" || return 0
  [ -n "$out" ] || return 0
  printf '%s  (free %s of %s, %s used)\n' "$1" \
    "$(printf '%s' "$out" | awk '{print $4}')" \
    "$(printf '%s' "$out" | awk '{print $2}')" \
    "$(printf '%s' "$out" | awk '{print $5}')"
}

# Reclaims the build tree. Only safe once the old server is gone: on Windows it
# is `target/debug/gongga-server.exe` that holds the lock, so this runs in the
# launcher, after the wait, not in `stage`.
clean_build_tree() {
  local before after
  before="$(df -h "$ROOT" 2>/dev/null | tail -n 1 | awk '{print $4}')"
  # CARGO_TARGET_DIR would send this somewhere else; the tree that grows is the
  # default one at $ROOT/target.
  if ! ( cd "$ROOT" && CARGO_TARGET_DIR= cargo clean ) >/dev/null 2>&1; then
    echo "evolve: cargo clean failed; keeping the tree and carrying on"
    return 0
  fi
  after="$(df -h "$ROOT" 2>/dev/null | tail -n 1 | awk '{print $4}')"
  echo "evolve: cargo clean done (free ${before:-?} -> ${after:-?})"
}

# The successor must not touch the home while the old server still owns it:
# `InstanceLock` refuses, and the UDP port is still bound. So it waits for both
# to go, then `exec`s.
#
# Measured on Windows: `exec` of a native .exe does NOT keep the pid. The bash
# launcher stays alive as the server's parent and the exe gets a fresh pid, so
# `server.lock` and this launcher's pid file disagree by design. `status` uses
# the parent link to recognise that case, and `adopt()`'s ancestor guard is what
# keeps `del_bash` from killing the server through its own parent.
# The template is filled in afterwards, not expanded while it is written.
#
# This used to be an unquoted heredoc, which meant every backtick, `$(...)` and
# `$var` in it ran *at generation time*: a comment that mentioned `tasklist`
# ran tasklist and pasted the whole process table into the launcher, which then
# died on the first line of it. Quoting the template removes the class, and the
# values are spliced in by `fill`, which escapes everything `sed` and the shell
# would otherwise read as syntax.
fill() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//&/\\&}"
  value="${value//|/\\|}"
  printf '%s' "$value"
}

write_launcher() {
  local stamp="$1" exe="$2" old_pid="$3" clean="${4:-0}"
  local argsfile="${5:-}" bundle="${6:-0}" old_dir="${7:-}"
  local path="$RUN/evolve-$stamp-launch.sh"
  local pidfile="$RUN/evolve-$stamp-launch.pid"
  local beatfile="$RUN/evolve-$stamp-launch.beat"
  sed \
    -e "s|@@STAMP@@|$(fill "$stamp")|g" \
    -e "s|@@EXE@@|$(fill "$exe")|g" \
    -e "s|@@OLD_PID@@|$(fill "$old_pid")|g" \
    -e "s|@@ARGSFILE@@|$(fill "$argsfile")|g" \
    -e "s|@@OLD_CWD@@|$(fill "$old_dir")|g" \
    -e "s|@@HOME_DIR@@|$(fill "$HOME_DIR")|g" \
    -e "s|@@PIDFILE@@|$(fill "$pidfile")|g" \
    -e "s|@@BEATFILE@@|$(fill "$beatfile")|g" \
    -e "s|@@ROOT@@|$(fill "$ROOT")|g" \
    -e "s|@@BIND@@|$(fill "$BIND")|g" \
    -e "s|@@CURRENT@@|$(fill "$CURRENT")|g" \
    -e "s|@@CLEAN@@|$(fill "$clean")|g" \
    -e "s|@@LEASE@@|$(fill "$LEASE")|g" \
    -e "s|@@BUNDLE@@|$(fill "$bundle")|g" \
    > "$path" <<'LAUNCH'
#!/usr/bin/env bash
# Successor for @@STAMP@@: waits outside the old server, then takes the home.
set -u
# The pid alone is not proof of life: a stale pid file plus a recycled pid
# would look exactly like a waiting successor. The heartbeat is written by the
# loop below, so only a process actually sitting in that loop keeps it fresh.
# The pid file has to hold the *Windows* pid: every check downstream is a
# tasklist query, and under Git Bash $$ is an MSYS pid that tasklist never
# reports -- so a waiting successor would look dead and swap would refuse to
# cancel, which is the whole handover failing. /proc/$$/winpid is the same
# process in the other numbering; $$ is only a fallback for a bash that has no
# /proc (where pids already are what tasklist/ps see).
if [ -r /proc/$$/winpid ]; then
  cat /proc/$$/winpid > '@@PIDFILE@@'
else
  echo $$ > '@@PIDFILE@@'
fi
# The lease names whoever is alive for this handover, and the shell that staged it
# has exited by now: from here on it is this loop.
if [ -d '@@LEASE@@' ]; then
  cat '@@PIDFILE@@' > '@@LEASE@@/pid' 2>/dev/null
fi
CLEAN='@@CLEAN@@'
date +%s > '@@BEATFILE@@'
NEW='@@EXE@@'
OLD_PID='@@OLD_PID@@'
ARGS_FILE='@@ARGSFILE@@'
OLD_DIR='@@OLD_CWD@@'
HOME_DIR='@@HOME_DIR@@'
FALLBACK_BIND='@@BIND@@'

# The old server's own argv, one token per line, read while it is still alive.
# The successor is the same server in a newer binary: same home, same address,
# same owner, same everything a flag said. `exec "$NEW"` with no argv used to
# produce a server nobody owned -- SPEC, Skill: evolve, is that a restart keeps
# the lifetime it had, "e.g. tui spawned server's lifetime bound to tui".
ARGS=()
if [ -s "$ARGS_FILE" ]; then
  while IFS= read -r line; do ARGS+=("$line"); done < "$ARGS_FILE"
fi
flag() { # the value after this flag, empty when it was not named
  local want="$1" i
  for ((i = 0; i < ${#ARGS[@]} - 1; i++)); do
    if [ "${ARGS[i]}" = "$want" ]; then
      printf '%s' "${ARGS[i + 1]}"
      return
    fi
  done
}
NEW_HOME="$(flag --home)"; NEW_HOME="${NEW_HOME:-$HOME_DIR}"
NEW_BIND="$(flag --bind)"; NEW_BIND="${NEW_BIND:-$FALLBACK_BIND}"
NEW_PARENT="$(flag --parent)"
PORT="${NEW_BIND##*:}"

# Nobody owns a home whose window is gone, and a server started with a dead
# --parent stops itself at once -- after resuming the turn it just picked up.
# Taking the home anyway would be a handover into a server that cannot live.
if [ -n "$NEW_PARENT" ] && ! tasklist //FI "PID eq $NEW_PARENT" //NH 2>/dev/null | grep -q "$NEW_PARENT"; then
  echo "evolve: the window that owned the old server (pid $NEW_PARENT) is gone; not taking the home"
  echo "evolve: start a window to get a server again, then stage a handover for it"
  exit 0
fi
# Wait for as long as it takes. A cap here would mean "the handover failed
# because the old server was slow", and AGENTS.md forbids turning a wait into a
# budget; the successor is detached, so waiting costs nobody anything. It says
# what it is waiting for instead, so a stuck handover is diagnosable.
tick=0
while :; do
  # Only the newest stage may take the home. A waiter whose attempt was
  # abandoned has to leave: left in the loop it polls tasklist and netstat five
  # times a second for as long as the old server lives, and when the port
  # finally frees it can beat the real successor to it -- with an older binary.
  # No pointer at all (an older stage, a hand-run script) means nothing
  # supersedes this one, so it waits as before.
  superseded=$(cat '@@CURRENT@@' 2>/dev/null || true)
  if [ -n "$superseded" ] && [ "$superseded" != '@@STAMP@@' ]; then
    echo "evolve: stage @@STAMP@@ was superseded by $superseded; leaving the queue"
    exit 0
  fi
  alive=0
  if [ -n "$OLD_PID" ] && tasklist //FI "PID eq $OLD_PID" //NH 2>/dev/null | grep -q "$OLD_PID"; then
    alive=1
  fi
  listening=$(netstat -ano 2>/dev/null | grep -c ":$PORT" || true)
  if [ "$alive" = 0 ] && [ "$listening" = 0 ]; then
    cd '@@ROOT@@'
    # `--clean` wipes the build tree, and the old server is still holding pieces
    # of it: a copy of its head is what runs, but the tree it was built from is
    # busy until it exits. This is the one moment when it is not.
    if [ "$CLEAN" = 1 ]; then
      echo "evolve: cargo clean before taking the home"
      CARGO_TARGET_DIR= cargo clean || echo "evolve: cargo clean failed; carrying on"
    fi
    # The successor runs in the directory the old server ran in, not in the home:
    # that inherited directory is what a new session with no `--cwd` opens in
    # (see cli::server::command), and the old process cannot be asked by now.
    if ! cd "$OLD_DIR"; then
      echo "evolve: the old server's directory $OLD_DIR is gone; taking the home from $NEW_HOME"
      cd "$NEW_HOME" || { echo "evolve: no home at $NEW_HOME; not taking it" >&2; exit 1; }
    fi
    echo "evolve: taking the home as $NEW_HOME on $NEW_BIND${NEW_PARENT:+ for window $NEW_PARENT} (${#ARGS[@]} argument(s) carried over, from $PWD)"
    # The staged file is the bundle: one head that installs every program it
    # carries into this home and then runs the one its own word names. That is
    # what makes a handover replace the whole distribution -- payload for all
    # three programs, and the `bin` pair the next shell will type (the server
    # refreshes it from its own build at startup).
    # The home is this process's now: a claim left behind would block the next
    # handover until somebody noticed it was stale.
    rm -rf '@@LEASE@@'
    if [ '@@BUNDLE@@' = "1" ]; then
      GONGGA_HOME="$NEW_HOME" exec "$NEW" server "${ARGS[@]}"
    fi
    exec "$NEW" "${ARGS[@]}"
  fi
  date +%s > '@@BEATFILE@@'
  if [ $((tick % 60)) = 0 ]; then
    echo "evolve: waiting for the home (old pid ${OLD_PID:-none} alive=$alive, port $PORT still bound=$listening)"
  fi
  tick=$((tick + 1))
  # Fast while a handover is expected, slow once it is not. Each tick spawns
  # `tasklist` and `netstat`, and a waiter left behind by an abandoned attempt
  # polls forever: three of those at 5Hz exhausted MSYS's fork on this machine,
  # which then failed inside unrelated shells. Waiting is not urgent enough to
  # cost a process spawn every 200ms for minutes.
  if [ "$tick" -le 25 ]; then sleep 0.2; else sleep 1; fi
done
LAUNCH
  chmod +x "$path"
  # A launcher that does not parse is worse than none: `swap` would cancel the
  # old server for a successor that cannot start.
  bash -n "$path" || die "generated launcher $path does not parse"
  echo "$path"
}

# The parent pid of a live process, or nothing. PowerShell because it is the one
# thing on this platform that answers without extra tooling.
parent_of() {
  powershell -NoProfile -Command \
    "(Get-CimInstance Win32_Process -Filter 'ProcessId=$1').ParentProcessId" \
    2>/dev/null | tr -d '\r' | grep -E '^[0-9]+$' || true
}

# Whether `candidate` is the server, or the process it hangs from.
#
# # Invariant
#
# A `taskkill /T` aimed at either of these takes the server with it, so no verb here
# may choose one.
#
# Measured on Windows 2026-11-02, and it shrinks what this can honestly be: `exec` of
# a native exe leaves the launcher bash **dead**, and the pids MSYS records as
# `ParentProcessId` are not the Windows pids of the visible bash processes (a
# three-level chain reported ppids belonging to processes already gone). So a walk up
# the tree is not answerable here, and a stale pid could even be reused by something
# unrelated. Two facts are answerable, and they cover the shapes that exist: the
# launcher kept the pid (unix `exec`), or the launcher survived as the server's
# parent. Everything else is a dead pid the loop never reaches.
same_as_server() { # candidate server_pid
  local candidate="$1" server_pid="$2"
  [ -n "$server_pid" ] || return 1
  [ "$candidate" = "$server_pid" ] && return 0
  [ "$(parent_of "$server_pid")" = "$candidate" ]
}

# The directory of a channel that answers, found by asking it.
#
# # Invariant
#
# Its program is the one hand on this machine that is in no job of the server's:
# relays are spawned detached, and `swap` cancels with `keep_alive`, which leaves
# the program running. A successor handed to it therefore outlives the server it
# replaces.
#
# A live pid is not enough to hand a handover to: a relay whose program lost its
# stdin keeps running and reads nothing, and its row looks exactly like a working
# one. So each candidate is asked to write a file, and one that does not answer
# in two seconds is passed over -- the cost of a wrong choice is the home.
live_channel() {
  local dir pid probe i
  for dir in "$HOME_DIR"/channels/*/; do
    [ -f "$dir/status" ] || continue
    grep -q '"exit":null' "$dir/status" 2>/dev/null || continue
    pid="$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$dir/status")"
    alive "$pid" || continue
    probe="$HOME_DIR/channel-probe-$$-${RANDOM}"
    rm -f "$probe"
    printf 'input %s\n' \
      "$(printf 'echo ok > %s\n' "$probe" | base64 -w0)" >>"$dir/in"
    for i in $(seq 1 20); do
      if [ -f "$probe" ]; then
        rm -f "$probe"
        printf '%s\n' "$dir"
        return 0
      fi
      sleep 0.1
    done
    rm -f "$probe"
    echo "note: ${dir%/} (pid $pid) answers nothing; trying the next channel" >&2
  done
  return 1
}

# The command line of a running process, as one string; failure when it cannot
# be read at all.
server_cmdline() { # pid
  local pid="$1"
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      # `wmic` is deprecated but present wherever Git Bash is; `//value` drops
      # the column header, and every line still carries the `CommandLine=` key.
      wmic process where "ProcessId=$pid" get CommandLine //value 2>/dev/null \
        | sed -n 's/^CommandLine=//p' | tr -d '\r' | sed '/^$/d'
      ;;
    *)
      [ -r "/proc/$pid/cmdline" ] || return 1
      tr '\0' '\n' < "/proc/$pid/cmdline" | sed '/^$/d'
      ;;
  esac
}

# The argv of a running process, one token per line, argv[0] included.
#
# Split the way a command line is split -- honouring the double quotes a value
# with a space arrives in -- so a path stays one token.
server_tokens() { # pid
  server_cmdline "$1" | awk '
    BEGIN { out = ""; inq = 0 }
    {
      s = $0
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\"") { inq = !inq; continue }
        if (c == " " && !inq) { if (out != "") { print out; out = "" } continue }
        out = out c
      }
      if (out != "") { print out; out = "" }
    }'
}

# The flags that decide how a server is reachable and who owns it, read from the
# server that is running now: one "flag<TAB>value" line each.
#
# SPEC, Skill: evolve: "restart server should still keep its lifetime, e.g. tui
# spawned server's lifetime bound to tui". The window that owns the old server is
# named in its `--parent`, so the successor has to name the same window: an
# `exec "$NEW"` with no argv produces a server nobody owns, which is a lifetime
# that silently changed hands to nobody.
running_flags() { # pid
  server_tokens "$1" | awk '
    tok { print flag "\t" $0; tok = 0; next }
    $0 == "--home" || $0 == "--bind" || $0 == "--parent" { flag = $0; tok = 1 }'
}

# One flag of a running server, empty when it was not named.
flag_of() { # pid, flag
  running_flags "$1" | awk -F '\t' -v want="$2" '$1 == want { print $2; exit }'
}

running_pid() {
  local pid
  pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || true)"
  [ -n "$pid" ] || return 1
  tasklist //FI "PID eq $pid" //NH 2>/dev/null | grep -q "$pid" || return 1
  echo "$pid"
}

# This shell's pid in the numbering every check downstream uses.
win_pid() {
  if [ -r "/proc/$$/winpid" ]; then cat "/proc/$$/winpid"; else echo $$; fi
}

alive() { # windows pid
  [ -n "${1:-}" ] || return 1
  tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"
}

lease_stamp() { cat "$LEASE/stamp" 2>/dev/null || true; }

# Whether a lease is still somebody's: a process alive for it, or -- before a
# successor exists -- a stage that finished and is waiting to be launched. That
# window is the one worth guarding: `stage` writes the claim and exits, and the
# operator launches a moment later. Reading only pids would call it abandoned and
# let the next agent take it, which is exactly the supersede this forbids.
lease_live() {
  local stamp pid launcher
  stamp="$(lease_stamp)"
  pid="$(cat "$LEASE/pid" 2>/dev/null || true)"
  launcher="$(cat "$RUN/evolve-$stamp-launch.pid" 2>/dev/null || true)"
  if alive "$pid" || alive "$launcher"; then
    return 0
  fi
  [ -n "$stamp" ] && [ "$stamp" = "$(cat "$CURRENT" 2>/dev/null || true)" ] \
    && [ -f "$RUN/evolve-$stamp.txt" ]
}

# Takes the lease for `stamp`, or stops and says who holds it.
#
# # Invariant
#
# One handover at a time. A holder is whoever is alive for it -- the staging shell,
# or the successor it left waiting -- and a lease whose holders are all gone is
# taken over with a word rather than blocking a later agent forever.
take_lease() { # stamp
  local stamp="$1"
  if mkdir "$LEASE" 2>/dev/null; then
    printf '%s\n' "$stamp" > "$LEASE/stamp"
    win_pid > "$LEASE/pid"
    LEASE_HELD=1
    return 0
  fi
  local holder pid who
  holder="$(lease_stamp)"
  pid="$(cat "$LEASE/pid" 2>/dev/null || true)"
  who="stage ${holder:-unknown}"
  if lease_live; then
    if alive "$pid"; then who="$who (pid $pid)"; else who="$who (staged, not launched yet)"; fi
    die "another handover is in flight: $who. \
Two agents must not evolve at once -- run '$0 status' to see it, '$0 swap ${holder:-<stamp>}' \
if it is staged and yours, or '$0 clean' if it is abandoned."
  fi
  echo "note: stage ${holder:-unknown} left its lease behind and is gone; taking it over"
  rm -rf "$LEASE"
  mkdir "$LEASE" || die "cannot take $LEASE"
  printf '%s\n' "$stamp" > "$LEASE/stamp"
  win_pid > "$LEASE/pid"
  LEASE_HELD=1
}

# Drops the lease unless the handover has been handed to a successor.
release_lease() {
  [ "${LEASE_HELD:-0}" = 1 ] && rm -rf "$LEASE"
  LEASE_HELD=0
}

cmd_stage() {
  local clean=0
  for flag in "$@"; do
    case "$flag" in
      --all) echo "note: --all is the only mode now; every program is staged" ;;
      --clean) clean=1 ;;
    esac
  done

  # Content is refreshed before any claim: a successor installs what the local
  # official ref names, so a stale ref can make an otherwise valid bundle unstartable.
  echo "== ship =="
  ( cd "$ROOT" && git -C ship fetch origin main && git -C ship rebase origin/main \
      && scripts/ship.sh --check ) \
    || die "the ship checkout is not current and published; refusing to stage a handover"
  # The successor reads the home's fork, not the source checkout checked above.
  local home_ship="$HOME_DIR/ship"
  if [ -d "$home_ship/.git" ]; then
    git -C "$home_ship" fetch origin main \
      || die "cannot refresh $home_ship; refusing to stage a handover"
    local home_tip=main
    if git -C "$home_ship" show-ref --verify --quiet refs/heads/local; then
      git -C "$home_ship" rebase origin/main local \
        || die "cannot rebase $home_ship/local; refusing to stage a handover"
      home_tip=local
    elif [ "$(git -C "$home_ship" symbolic-ref --short -q HEAD || true)" = main ]; then
      git -C "$home_ship" merge --ff-only origin/main \
        || die "cannot fast-forward $home_ship/main; refusing to stage a handover"
    else
      git -C "$home_ship" branch -f main origin/main \
        || die "cannot update $home_ship/main; refusing to stage a handover"
    fi
    git -C "$home_ship" merge-base --is-ancestor origin/main "$home_tip" \
      || die "$home_ship/$home_tip does not contain official main; refusing to stage"
  fi

  # Taken before the build: two agents waiting for a compile that only one of them
  # may finish is the cost this avoids, and the claim is what tells the second one
  # to stop rather than to supersede.
  local stamp
  stamp="$(date +%Y%m%d-%H%M%S)"
  take_lease "$stamp"
  # A stage that dies must not leave the claim behind. It survives this line only by
  # reaching the end, where the successor takes it over.
  trap 'release_lease' EXIT INT TERM

  disk_line "disk:"

  # Every target, test cfg included: a workspace that does not compile is the one
  # thing a handover must not install, and `--all-targets` answers exactly that
  # without linking a test binary per crate -- measured here at 7 s against 14 s, for
  # no tests to run. `set -e` is on, so the failure path has to be an `if`.
  echo "== check =="
  if ! cargo check --workspace --all-targets > "$RUN/.check.log" 2>&1; then
    tail -n 25 "$RUN/.check.log"; rm -f "$RUN/.check.log"
    die "the workspace does not compile; not staging a handover to it"
  fi
  tail -n 3 "$RUN/.check.log"; rm -f "$RUN/.check.log"
  # A workspace that does have tests still runs them: a handover is the wrong moment
  # to discover one is red, and one grep is cheaper than linking a test binary to
  # find out there is nothing to run.
  if grep -rqsE --include=*.rs '#\[(tokio::)?test\]' "$ROOT/crates"; then
    echo "== tests =="
    if ! cargo test --workspace > "$RUN/.test.log" 2>&1; then
      tail -n 25 "$RUN/.test.log"; rm -f "$RUN/.test.log"
      die "the workspace's tests fail; not staging a handover to it"
    fi
    tail -n 3 "$RUN/.test.log"; rm -f "$RUN/.test.log"
  fi

  echo "== build =="
  # The whole distribution, as one file: the bundle carries server, cli and tui,
  # so a handover replaces all three rather than only the process this machine
  # happens to be running. Its payload goes to `target/bundle-payload` -- the
  # crate asks cargo for those three programs by name, and cargo resolves that
  # against the tree it is told to use. The successor gets a copy of the head,
  # which is what keeps a running .exe from blocking the build (see SKILL.md).
  ( cd "$ROOT" && cargo build -p gongga-bundle ) || die "building the bundle failed"
  # The head is `gongga` (see crates/bundle/Cargo.toml), not the package name.
  local head="${CARGO_TARGET_DIR:-$ROOT/target}/debug/gongga"
  [ -f "$head" ] || head="$head.exe"
  [ -f "$head" ] || die "no bundle head at $head"

  local exe old_pid launcher
  exe="$RUN/evolve-$stamp-gongga.exe"
  cp "$head" "$exe" || die "copying the bundle failed"
  old_pid="$(running_pid || true)"
  [ -n "$old_pid" ] || echo "note: no live server holds $HOME_DIR/server.lock"
  # The successor has to be the old server over again: its argv is recorded now,
  # while it can still be read, and handed to the new binary verbatim.
  local argsfile="$RUN/evolve-$stamp-args"
  : > "$argsfile"
  local old_home="$HOME_DIR" old_bind="$BIND" old_parent="" old_dir=""
  if [ -n "$old_pid" ]; then
    # Read now, while it can still answer: after the cancel there is nothing left
    # to ask. Unreadable is a refusal rather than a guess, because the guess is
    # what produced an unowned server in the first place.
    if [ -z "$(server_cmdline "$old_pid" 2>/dev/null)" ]; then
      die "cannot read pid $old_pid's command line, so what the successor should be is \
unknown; refusing to stage a handover that would produce a server nobody owns"
    fi
    server_tokens "$old_pid" | tail -n +2 > "$argsfile"
    old_home="$(flag_of "$old_pid" --home)"; [ -n "$old_home" ] || old_home="$HOME_DIR"
    old_bind="$(flag_of "$old_pid" --bind)"; [ -n "$old_bind" ] || old_bind="$BIND"
    old_parent="$(flag_of "$old_pid" --parent)"
    echo "old server: home $old_home, bind $old_bind, owner ${old_parent:-none}"
  fi
  # The directory the old server runs in -- which is what a session created with
  # no `--cwd` opens in -- or the one the last server claimed this home from when
  # none is running. A server records it beside its claim (`server.cwd`), because
  # nothing can read another process's directory on Windows; one older than that
  # record leaves the successor in this repo, where a handover is staged from.
  old_dir="$(head -n 1 "$HOME_DIR/server.cwd" 2>/dev/null | tr -d '\r' || true)"
  if [ -z "$old_dir" ]; then
    old_dir="$ROOT"
    echo "note: no $HOME_DIR/server.cwd (a server older than the record); the successor runs in $old_dir"
  else
    echo "old_dir=$old_dir"
  fi
  launcher="$(write_launcher "$stamp" "$exe" "$old_pid" "$clean" "$argsfile" 1 "$old_dir")"
  # The previous attempt is abandoned from here on: its launcher sees itself
  # superseded and leaves the waiting loop by itself (see the loop below).
  echo "$stamp" > "$CURRENT"

  {
    echo "stamp=$stamp"
    echo "bundle=$exe"
    echo "old_pid=${old_pid:-none}"
    echo "old_home=$old_home"
    echo "old_bind=$old_bind"
    echo "old_owner=${old_parent:-none}"
    echo "old_dir=$old_dir"
    echo "old_args=$argsfile"
    echo "launcher=$launcher"
  } > "$RUN/evolve-$stamp.txt"
  cat "$RUN/evolve-$stamp.txt"
  echo
  echo "next: launch it outside this job, then swap"
  echo "  $PWD/.agents/skills/evolve/evolve.sh launch $stamp"
  echo "  $PWD/.agents/skills/evolve/evolve.sh swap $stamp"
  echo
  if [ "$clean" = 1 ]; then
    echo "armed: the successor runs 'cargo clean' after the old server is gone, before it binds"
  fi
  # From here the lease belongs to the successor: this shell is about to exit, and
  # the wait for the home may last minutes.
  LEASE_HELD=0
  echo "an earlier attempt still waiting will leave on its own; 'clean' says who is left"
}

# Starts a stage's launcher where the cancel cannot reach it.
#
# # Invariant
#
# The successor has to outlive the server it replaces, so it may not be a child
# of anything that server's job holds -- and a shell of the turn being cancelled
# is exactly that. A channel's program is not: relays are detached, and `swap`
# cancels with `keep_alive`, which leaves the program running. So the launcher is
# handed to a live channel instead of being started from this shell.
cmd_launch() {
  local stamp="${1:?usage: evolve.sh launch <stamp>}"
  local launcher="$RUN/evolve-$stamp-launch.sh"
  [ -f "$launcher" ] || die "no launcher for $stamp; run stage first"
  local ch
  ch="$(live_channel || true)"
  [ -n "$ch" ] || die "no live channel to start the successor from; \
open a window (its shell keeps one) and retry"

  local log="$RUN/evolve-$stamp-launch.log" cmd pid i
  # A pid or a beat left by an earlier attempt would be read as this one waiting.
  rm -f "$RUN/evolve-$stamp-launch.pid" "$RUN/evolve-$stamp-launch.beat"
  cmd="bash $(printf '%q' "$launcher") </dev/null >>$(printf '%q' "$log") 2>&1 &"
  printf 'input %s\n' "$(printf '%s\n' "$cmd" | base64 -w0)" >>"$ch/in"
  echo "successor handed to ${ch%/} (that program is in no job of this server)"

  for i in $(seq 1 100); do
    pid="$(cat "$RUN/evolve-$stamp-launch.pid" 2>/dev/null || true)"
    [ -n "$pid" ] && break
    sleep 0.1
  done
  [ -n "$pid" ] || die "the channel did not start it; see $log"
  for i in $(seq 1 100); do
    [ -s "$RUN/evolve-$stamp-launch.beat" ] && break
    sleep 0.1
  done
  [ -s "$RUN/evolve-$stamp-launch.beat" ] || die "pid $pid never beat; see $log"
  # The claim `swap` reads: this successor came from a program a cancel cannot
  # end. Its pid and heartbeat look the same either way -- only the one that
  # started it knows which process tree it is in, so it says so here.
  printf '%s\n' "${ch%/}" >"$RUN/evolve-$stamp-launch.outside"
  echo "successor pid $pid is waiting; swap $stamp hands the home to it"
}

cmd_swap() {
  local stamp="${1:?usage: evolve.sh swap <stamp>}"
  local exe="$RUN/evolve-$stamp-gongga.exe"
  [ -f "$exe" ] || die "no staged bundle for $stamp; run stage first"

  # Only the newest stage may take the home. An older stamp still has its
  # files, but its launcher has left the queue (it saw the pointer move), and
  # cancelling for it would hand the home to nothing.
  local current
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  if [ -z "$current" ]; then
    echo "note: no $CURRENT; not checking whether $stamp is the newest stage"
  elif [ "$stamp" != "$current" ]; then
    die "stage $stamp is not the current handover target (current: $current); \
its launcher has already left the queue -- run stage again if you meant the newest build"
  fi

  # Two agents racing is what the lease is for: `$CURRENT` has already moved to the
  # later stage by then, so this is the check that names the real holder.
  local holder
  holder="$(lease_stamp)"
  if [ -n "$holder" ] && [ "$holder" != "$stamp" ]; then
    die "stage $holder holds the handover lease, not $stamp; run '$0 status' \
(two agents evolving at once is what AGENTS forbids, and what this refuses)"
  fi

  # A successor that is not actually waiting must stop the handover: cancelling
  # without one would leave the home with no server and nothing to resume this
  # turn with. The launcher records its pid before it waits, so this is a real
  # check rather than a guess.
  local launch_pid
  launch_pid="$(cat "$RUN/evolve-$stamp-launch.pid" 2>/dev/null || true)"
  [ -n "$launch_pid" ] || die "successor was never launched; not cancelling"
  tasklist //FI "PID eq $launch_pid" //NH 2>/dev/null | grep -q "$launch_pid" \
    || die "successor pid $launch_pid is gone; not cancelling \
(if the pid looks like a small MSYS number, the launcher was written before \
/proc/\$\$/winpid was recorded: re-run stage and launch the new one)"
  # A live pid is not necessarily *our* launcher: pids get reused, and a pid
  # file left by an earlier attempt would then look like a successor waiting.
  # Only the waiting loop writes the heartbeat, so a fresh one means something
  # really is sitting there ready to take the home. One beat per 0.2s, so 10s
  # of slack is room for a loaded machine, not a budget: too tight only means
  # "refused, retried", too loose means cancelling after the waiter died.
  local beat now
  beat="$(cat "$RUN/evolve-$stamp-launch.beat" 2>/dev/null || echo 0)"
  now="$(date +%s)"
  [ $((now - beat)) -le 10 ] \
    || die "successor pid $launch_pid has no fresh heartbeat; not cancelling"

  local pid
  pid="$(running_pid || true)"
  [ -n "$pid" ] || die "no live server on $HOME_DIR; refusing to cancel"
  if [ -z "$CLI" ] || [ ! -x "$CLI" ]; then
    die "no gongga CLI to send cancel with; is one on PATH?"
  fi
  # A successor that hangs from the server is one the cancel ends with it: it
  # beats until the old process exits and then goes with the job, leaving the
  # home unserved and this turn with nothing to resume it. A process started
  # from a shell of this turn is exactly that, and nothing visible from here
  # tells the two kinds apart -- so the handover that knows writes it down, and
  # one started by hand has to be restarted through `launch`.
  [ -f "$RUN/evolve-$stamp-launch.outside" ] || die "successor pid $launch_pid was not \
started by '$0 launch'; a shell of this turn is in the server's job, and a cancel takes \
a successor started from one with it -- run '$0 launch $stamp' and swap again"
  disk_line "disk before:"
  echo "old server pid $pid; successor $exe (pid $launch_pid) is waiting"
  echo "cancelling: this turn is cancelled and preserved, the successor resumes it"
  # `keep_alive` because the successor was handed to a channel: without it the
  # cancel ends that channel's program and the successor with it. A server that
  # predates the flag has no channels to end, so it is asked plainly.
  "$CLI" cancel --keep_alive true || {
    echo "note: this server does not take keep_alive; cancelling plainly" >&2
    "$CLI" cancel
  }
}

cmd_status() {
  disk_line "disk:"
  echo "current: $(cat "$CURRENT" 2>/dev/null || echo none)"
  local armed_launcher
  armed_launcher="$RUN/evolve-$(cat "$CURRENT" 2>/dev/null)-launch.sh"
  if grep -q "^CLEAN='1'$" "$armed_launcher" 2>/dev/null; then
    echo "armed: the current successor runs 'cargo clean' before it binds"
  fi
  local holder holder_pid holder_launcher
  holder="$(lease_stamp)"
  if [ -d "$LEASE" ]; then
    holder_pid="$(cat "$LEASE/pid" 2>/dev/null || true)"
    if ! lease_live; then
      echo "lease: stage ${holder:-unknown} (holder gone; the next stage takes it over)"
    elif [ -f "$RUN/evolve-$holder-launch.pid" ]; then
      echo "lease: stage ${holder:-unknown} (pid ${holder_pid:-?}) is waiting to take the home"
    else
      echo "lease: stage ${holder:-unknown} is staged; launch it, then swap"
    fi
  fi
  local server_pid
  server_pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || echo none)"
  echo "server.lock: $server_pid"
  netstat -ano 2>/dev/null | grep ":$PORT" || echo "port $PORT: not bound"
  echo "staged:"
  ls -t "$RUN"/evolve-*.txt 2>/dev/null | head -n 3
  echo "successors:"
  local current f p stamp mark
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    p="$(cat "$f")"
    stamp="$(basename "$f")"
    stamp="${stamp#evolve-}"
    stamp="${stamp%-launch.pid}"
    tasklist //FI "PID eq $p" //NH 2>/dev/null | grep -q "$p" || continue
    # One rule for both verbs: neither the server nor the process it hangs from is a
    # waiter -- `taskkill /T` on either takes the server down.
    if same_as_server "$p" "$server_pid"; then
      echo "  $stamp: pid $p is the server or the process it hangs from"
      continue
    fi
    mark=""
    if [ "$stamp" = "$current" ]; then
      mark=" (current)"
    else
      mark=" (superseded; 'clean' ends it)"
    fi
    echo "  $stamp: pid $p waiting$mark"
  done
}

# Ends successors nobody is going to swap, and optionally the files they left.
#
# The reason this exists rather than being automatic on every `stage`: a waiter
# that predates the supersede check in the launcher loop has no way out, and a
# machine that has been evolving for a while has a pile of them. Each one is
# five `tasklist`/`netstat` rounds a second, forever, and every later server
# adopts it, so it also shows up in `list_bash` as a job that cannot be used.
cmd_clean() {
  local artifacts=0
  [ "${1:-}" = "--artifacts" ] && artifacts=1
  local current f p stamp server_pid live_stamp removed
  local killed=0 alive=0 kept=0
  local -a keep=()
  current="$(cat "$CURRENT" 2>/dev/null || true)"
  # The pid file of the stage that `exec`ed into the server holds the *server's*
  # pid, and that stage is usually not the current one any more -- a later
  # `stage` moves the pointer. Killing it would kill the very process this
  # command is running under, so the live server is never a candidate.
  server_pid="$(cat "$HOME_DIR/server.lock" 2>/dev/null || true)"
  live_stamp=""
  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    [ -n "$server_pid" ] || break
    [ "$(cat "$f" 2>/dev/null || true)" = "$server_pid" ] || continue
    live_stamp="${f##*/evolve-}"
    live_stamp="${live_stamp%-launch.pid}"
  done

  for f in "$RUN"/evolve-*-launch.pid; do
    [ -e "$f" ] || continue
    p="$(cat "$f" 2>/dev/null || true)"
    stamp="$(basename "$f")"
    stamp="${stamp#evolve-}"
    stamp="${stamp%-launch.pid}"
    [ -n "$p" ] || continue
    tasklist //FI "PID eq $p" //NH 2>/dev/null | grep -q "$p" || continue
    # `same_as_server` first, and not the stamp test: the stamp only says who may take
    # the home *next*, so a later `stage` moves it away from the launcher a server is
    # actually hanging from. See the 实测坑 in this skill's SKILL.md.
    if same_as_server "$p" "$server_pid"; then
      echo "  keep    $stamp (pid $p is the server or the process it hangs from)"
      kept=$((kept + 1))
      continue
    fi
    if [ "$stamp" = "$current" ]; then
      echo "  keep    $stamp (pid $p, current)"
      kept=$((kept + 1))
      continue
    fi
    if taskkill //PID "$p" //T //F >/dev/null 2>&1; then
      echo "  killed  $stamp (pid $p, superseded)"
      killed=$((killed + 1))
    else
      echo "  left    $stamp (pid $p); could not kill it"
      alive=$((alive + 1))
    fi
  done
  echo "waiters: $killed killed, $alive left running, $kept kept"

  local ls lp llauncher
  ls="$(lease_stamp)"
  if [ -d "$LEASE" ]; then
    lp="$(cat "$LEASE/pid" 2>/dev/null || true)"
    llauncher="$(cat "$RUN/evolve-$ls-launch.pid" 2>/dev/null || true)"
    # Not `lease_live`: a stage that is only staged is exactly what clean is for.
    if alive "$lp" || alive "$llauncher"; then
      echo "  lease   stage ${ls:-unknown} (pid ${lp:-?}) is still evolving; kept"
    else
      rm -rf "$LEASE"
      echo "  lease   stage ${ls:-unknown} was abandoned; released"
    fi
  fi

  if [ "$artifacts" = 0 ]; then
    echo "(pass --artifacts to delete the staged files of every stamp but the current one)"
    return 0
  fi

  # The staged bundle is ~40MB, nothing ever rewrites it, and a stamp
  # that is not current can no longer be swapped -- so the copy buys nothing.
  # The current stamp keeps everything: it is the one that may still be
  # launched. So does the stamp the running server came from, whose exe is the
  # image of a live process.
  #
  # One `find -delete` rather than a loop of `rm`: the shells this runs in can
  # have a delete wrapper on PATH that blocks per file, and a cleanup command is
  # the last place that should hang. One process, one pass.
  if [ -z "$current" ]; then
    echo "no evolve-current, so nothing says which stamp is live; not deleting anything"
    return 0
  fi
  # The stamp is followed by a dash in most artifacts and by a dot in the `.txt`
  # report, so both spellings have to be kept.
  keep=( -name "evolve-$current-*" -o -name "evolve-$current.*" )
  if [ -n "$live_stamp" ] && [ "$live_stamp" != "$current" ]; then
    keep+=( -o -name "evolve-$live_stamp-*" -o -name "evolve-$live_stamp.*" )
  fi
  removed=$(find "$RUN" -maxdepth 1 -type f \
    \( -name 'evolve-*-launch.sh' -o -name 'evolve-*-launch.pid' \
       -o -name 'evolve-*-launch.beat' -o -name 'evolve-*-launch.outside' \
       -o -name 'evolve-*-launch.log' \
       -o -name 'evolve-*-gongga.exe' -o -name 'evolve-*-server.exe' \
       -o -name 'evolve-*.txt' \) \
    ! \( "${keep[@]}" \) -print -delete 2>/dev/null | wc -l)
  echo "artifacts: $removed file(s) removed"
}

case "${1:-help}" in
  stage)  shift; cmd_stage "$@" ;;
  launch) shift; cmd_launch "$@" ;;
  swap)   shift; cmd_swap "$@" ;;
  status) cmd_status ;;
  clean)  shift; cmd_clean "$@" ;;
  *)      awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"
          exit 1 ;;
esac
