# `check` waited out its `wait`, and the sticky util is why it should not have

A session reading a `bash` channel always paid the full `wait`:

```
new   {"name":"bash","input":"cd ~ && scp ...","id":"R37"}      -> 0.002s, output ""
check {"id":"R37","wait":150000}                                -> 150s later, output "..."
check {"id":"R36","wait":120000}                                -> 120s later, output ""
```

SPEC says the marker is a **sticky value**, not a per-call one:

```
- util: use corresponding sticky value according to name, e.g. "python"/"bash".
```

`[channel.bash]` declares `until = "{{sentinel}}"`, and the sentinel is the text the write
puts in the command line (`{{input}}; echo {{sentinel}}$?`). `talk` generated one per write
and threw it away:

```rust
let sentinel = if input.is_some() && grammar.until... { Some(new_sentinel()) } else { None };
```

A `check` writes nothing, so it had no sentinel, so `marker` was `None`, so `look` could
never set `matched`. The only exits left were "the program ended" and the ceiling — hence
every read taking the full `wait`.

## The fix

The row remembers the marker its last write left (`channels.until_sentinel`), and a call
that writes nothing reads it:

- write with no stored sentinel: make one, write it, remember it;
- write with one stored: make a fresh one (each reply still has its own end);
- read with one stored: use it as the marker.

`python` was never affected: its `until` is the literal `">>> "`, so the marker needs no
machine state. `bash` is the one whose marker is generated, which is exactly the case the
word "sticky" is about.

## Measured, one server, one channel, two turns

```
turn 1  new   {"name":"bash","input":"sleep 4; echo done","id":"W1","wait":300}
        -> {"exit":null,"id":"W1","matched":false,"output":"","time":"0.307s"}   424ms
turn 2  check {"id":"W1","wait":9000}           (no input, so no write)
        -> {"exit":0,"id":"W1","matched":true,"output":"done","time":"4.179s"}   657ms
```

The read answered at the sentinel the write left behind -- 657ms into a 9s `wait`, with
the program's exit status. Before the column it could only answer at 9000ms.

## The default `new` wait

Separately: `new`'s default wait is the row's `timeout_ms`, and it was `0`, so a call that
named no `wait` returned as soon as the channel existed ("a call that states no `wait`
returns as soon as the channel exists"). That is the `0.002s` in the first transcript above,
and the reason every `new` needed a `check` after it. The row now says `4000`; the home
that was already running keeps its own `config.toml`, which is the file that decides this
until it is evolved to the shipped one.
