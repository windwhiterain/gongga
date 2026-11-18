# 2026-11-18 — Avalonia is deleted; the browser is the second front-end

**Superior's verdict (direct instruction):** 删除 Avalonia 前端，换 Web 前端。

## What was decided while implementing

- **`crates/webui` / `gongga-webui`** is a Cargo member, not a `frontends/` tree:
  DESIGN §1 already names `[webui] binary` and SPEC allows more crates with the
  same naming. It is **not** in the bundle (verdict 2 of §1: only the TUI ships,
  other UIs distribute themselves).
- The host is Rust and the window is a page. The browser cannot spawn a process nor
  speak the server's UDP, so the host serves the page on loopback and relays the
  client's JSON Lines verbatim over one WebSocket. It parses no frame: the wire
  stays `render_wire.rs`'s alone.
- **One WebSocket attach = one window = one `gongga-client` child.** The pipe has
  no replay, so a long-lived child would leave a reloaded page with a partial
  window; a fresh child reconnects and refills from its journal. A second tab is
  refused with a close reason the page shows. Two windows are two processes with
  `--instance`.
- **Loopback only, and an origin rule.** The page can submit lines and delete
  sessions; an unauthenticated control surface must not answer another machine,
  and another site's page must not reach a local host. Host must be a loopback name
  with our port; Origin, when a browser sends one, must be that same name.
- `crates/client/src/child.rs` now starts the child for both front-ends (spawn
  flags, stderr to `<client-home>/client.log`); `tui/src/pipe.rs` and
  `webui/src/window.rs` no longer duplicate that.
- The page follows the **current** wire, not the C# port: `Item`'s
  `label/detail/new_text/snippet/range` (not the old insert/detail/space) and
  `RenderMessage::Newer` / `UiIntent::{Newer, Held, Columns, Room}` which the
  C# front-end predated.
- Deliberate limit: the page holds a window's whole history and never trims, so it
  never sends `Held`; `keep_rows` is a terminal memory budget. recorded in
  `crates/webui/web/model.js`'s header and `docs/notes/webui-web.md`.

## Found while porting (recorded, then fixed)

- `render_wire.rs`, `runtime.rs`, `tui-run.md`, `tui-cover.md` said
  `UiIntent::Quit` is "Alt+Q"; the TUI has always bound **Ctrl+Q**. The page binds
  Ctrl+Q too, and the words are fixed at the source rather than in a note.
- `docs/notes/client-render-wire.md` stated "12 drawing + 5 control", "5 UiIntent
  gestures", "candidates" and a "bounded queue / try_send" writer: all stale against
  the code. The note now points at the code for the frame list and names the new
  frames/intents.
- The Avalonia section in `DESIGN.md` and `docs/notes/avalonia-frontend.md` are
  deleted with the front-end they described (the decisions that still hold --
  `client.lock`, `--instance`, `client.toml` in the client home -- are in the new
  section and their own notes).

## Verification

Real processes, not a mock. On 2026-11-18: a temporary home seeded from the local
`ship` checkout; `scripts/fake_llm.py` answering as
`deepseek-official/deepseek-flash` (the `exec` tool on the first request);
`gongga-server` on `127.0.0.1:19700`; `gongga-webui` on `127.0.0.1:19701`; and
the debugging Edge driven over CDP (the browser skill's playwright could not be
fetched -- the package index is unreachable from this session).

Observed, with screenshots under `.dev/weblive/shots/` and a copy in the main
checkout's `.dev/verify/web-frontend-2026-11-18/`:

- HTTP: `/` 200 `text/html` 2189 bytes, `/main.js` 200 `text/javascript`;
- attach: `Layout`, a sticky `main` pane, restored history, no console errors;
- a turn: user row, inject rows, a folded `exec·bash` call, `✓ 1.4s` output
  (`hi-from-fake\n`, exit 0), assistant text, status running -> idle;
- clicking the call opens it (two head fields: `bin`, `args`);
- typing `/` opens the client's completion (37 rows), Escape closes it;
- shift-click hides the pane, a plain click shows it; PageUp scrolled 296 of 480 px;
- a reload detaches (the child ends) and a new attach works again;
- a second tab is refused with `another page is attached; close it, or start
  gongga-webui --instance <name>`.

Gates: `mbx build` exit 0; `mbx fmt --check` exit 0; `test-layout-check` 0
violations over 12 packages; `comments-check` finds no violation in the new files
(the repository's pre-existing client/server violations are unchanged, only line
numbers moved); `scripts/skill_contract.sh` PASS; `mbx test -p gongga-client -p
gongga-tui` 17 passed. `scripts/ship.sh --check` cannot answer in this worktree
(its `ship/` submodule is not initialised); run from the main checkout it reports
a pre-existing drift -- content speaks protocol 14, this build reads 13 -- which
this branch did not touch.
