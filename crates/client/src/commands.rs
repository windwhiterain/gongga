//! Sticky arguments and shown windows: the client's own state in `client.toml`.
//!
//! See `docs/notes/cli-commands.md`.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use protocol::{ArgSpec, Args, CommandSpec, StructSchema};

/// The skills a session can open: the project's own, the home's, then the machine's.
///
/// # Invariant
///
/// One walk, shared with the server's `skill` and with `skill <name>` -- so a
/// name read here is a name that can be opened. `project` is the directory the session
/// works in, and `None` when that directory is another machine's: its checkout is not
/// on this disk. See `docs/notes/cli-commands.md`.
pub fn installed_skills(home: &Path, project: Option<&Path>) -> Vec<String> {
    roots(home, project)
        .all()
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

/// The three roots one session reads a skill from, as this machine can see them.
pub fn roots(home: &Path, project: Option<&Path>) -> skill::Roots {
    skill::Roots::new(
        project.map(|dir| dir.join(".agents").join("skills")),
        home.join("skills"),
        dirs::home_dir().map(|home| home.join(".agents").join("skills")),
    )
}

/// Every sticky type a command can actually take.
///
/// # Invariant
///
/// SPEC's four, and nothing else: a required argument's `kind` (`Session`), a
/// structured value's own type when it is not an update (`SessionConfig`), and a
/// field's type when it has no default (`ModelConfig`, `Model`). One list for
/// `set_sticky`, `--type` and the write-back. See `docs/notes/client-commands.md`.
pub fn sticky_kinds(commands: &[CommandSpec]) -> Vec<String> {
    let mut kinds = BTreeSet::new();
    for command in commands {
        for arg in &command.args {
            let fillable = arg.required || (arg.structure.is_some() && !arg.update);
            if let Some(kind) = &arg.kind
                && fillable
            {
                kinds.insert(kind.clone());
            }
            if let Some(structure) = &arg.structure {
                field_kinds(structure, &mut kinds);
            }
        }
    }
    kinds.into_iter().collect()
}

fn field_kinds(schema: &StructSchema, kinds: &mut BTreeSet<String>) {
    for field in &schema.fields {
        if field.default.is_none()
            && let Some(kind) = &field.kind
        {
            kinds.insert(kind.clone());
        }
        if let Some(nested) = &field.object {
            field_kinds(nested, kinds);
        }
    }
}

/// Binds a line's tokens to a published spec. See `next_unfilled`.
///
/// # Invariant
///
/// The spec is the only syntax the caller knows: an unknown command is not
/// parsed, and a token naming nothing in it is a typo said as such. A value may
/// start with `--` only in the `--flag=--value` spelling.
pub fn parse_args(tokens: &[String], spec: Option<&CommandSpec>) -> anyhow::Result<Args> {
    let flags: Vec<&ArgSpec> = spec.map(|s| s.args.iter().collect()).unwrap_or_default();
    let takes = |name: &str| flags.iter().any(|a| a.name == name);
    let mut args = Args::new();
    // One past what the last token bound, a flag as much as a bare word.
    let mut cursor = 0;
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token.starts_with("--") {
            let flag = token.trim_start_matches('-');
            if let Some((name, value)) = flag.split_once('=') {
                if !takes(name) {
                    anyhow::bail!(
                        "{} has no argument --{name}; it takes {}",
                        spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                        signature(spec),
                    );
                }
                insert_once(&mut args, spec, name, value)?;
                cursor = after_arg(spec, name).unwrap_or(cursor);
                i += 1;
            } else {
                if !takes(flag) {
                    anyhow::bail!(
                        "{} has no argument --{flag}; it takes {}",
                        spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                        signature(spec),
                    );
                }
                let value = tokens
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--{flag} needs a value"))?;
                insert_once(&mut args, spec, flag, value)?;
                cursor = after_arg(spec, flag).unwrap_or(cursor);
                i += 2;
            }
        } else {
            let Some(at) = spec.and_then(|s| next_unfilled(s, &args, cursor)) else {
                anyhow::bail!(
                    "unexpected argument {token:?}; {} takes {}",
                    spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                    signature(spec),
                );
            };
            let name = spec.map(|s| s.args[at].name.clone()).unwrap_or_default();
            insert_once(&mut args, spec, &name, token)?;
            cursor = at + 1;
            i += 1;
        }
    }
    Ok(args)
}

/// The cursor a parameter leaves behind: the index right after it.
///
/// # Invariant
///
/// One past the parameter whatever spelling bound it: a flag and a bare word
/// move one cursor, and naming a parameter behind it pulls it back. Binding one
/// twice is [`insert_once`]'s refusal.
pub fn after_arg(spec: Option<&CommandSpec>, name: &str) -> Option<usize> {
    let spec = spec?;
    spec.args
        .iter()
        .position(|a| a.name == name)
        .map(|at| at + 1)
}

/// Where a bare word at `cursor` lands.
///
/// # Invariant
///
/// The first unfilled parameter from the cursor, else the first unfilled one from
/// the start -- the rule a nameless value binds by, and the one both the parser
/// and the completion walk, so a popup offers what a word lands on. Nothing binds
/// twice, so position reaches each parameter where it is declared; the wrap keeps
/// a flag ahead of the line from stranding what is behind it.
pub fn next_unfilled(spec: &CommandSpec, bound: &Args, cursor: usize) -> Option<usize> {
    let unfilled = |(_, a): &(usize, &ArgSpec)| !bound.contains_key(&a.name);
    spec.args
        .iter()
        .enumerate()
        .skip(cursor)
        .find(unfilled)
        .or_else(|| spec.args.iter().enumerate().find(unfilled))
        .map(|(at, _)| at)
}

/// A repeated argument is a contradiction, whatever spellings it was given.
fn insert_once(
    args: &mut Args,
    spec: Option<&CommandSpec>,
    name: &str,
    value: &str,
) -> anyhow::Result<()> {
    if args.contains_key(name) {
        anyhow::bail!("argument --{name} given twice");
    }
    // The word a line spells, as the value the argument's own type declares: a line
    // is text, and the type is what it holds.
    let type_of = spec
        .and_then(|spec| spec.args.iter().find(|a| a.name == name))
        .map(|a| a.type_of.as_str())
        .unwrap_or("str");
    let value = protocol::argtype::value_of(type_of, value)
        .map_err(|why| anyhow::anyhow!("--{name}: {why}"))?;
    args.insert(name.to_string(), value);
    Ok(())
}

/// The one-line syntax of a command: `<hint>`, `[<hint>]`, `--name <hint>`.
pub fn signature(spec: Option<&CommandSpec>) -> String {
    let Some(spec) = spec else {
        return "nothing".to_string();
    };
    if spec.args.is_empty() {
        return "nothing".to_string();
    }
    spec.args
        .iter()
        .map(|a| {
            let hint = a.value_hint.as_deref().unwrap_or(&a.name);
            if a.positional {
                if a.required {
                    format!("<{hint}>")
                } else {
                    format!("[<{hint}>]")
                }
            } else if a.required {
                format!("--{} <{}>", a.name, hint)
            } else {
                format!("[--{} <{}>]", a.name, hint)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders bound arguments back to a command line, positionals bare in their
/// published order, for the echoed command row.
pub fn render_args(args: &Args, spec: Option<&CommandSpec>) -> String {
    let mut out = String::new();
    if let Some(spec) = spec {
        for arg in spec.args.iter().filter(|a| a.positional) {
            if let Some(value) = args.get(&arg.name) {
                out.push(' ');
                out.push_str(&render_value(value));
            }
        }
    }
    for (k, v) in args.iter() {
        if spec
            .map(|s| s.args.iter().any(|a| a.positional && a.name == *k))
            .unwrap_or(false)
        {
            continue;
        }
        out.push_str(&format!(" --{k} {}", render_value(v)));
    }
    out
}

/// One value as it reads on a command line: a string bare unless it has a space.
fn render_value(value: &Value) -> String {
    match value.as_str() {
        Some(word) if !word.contains(' ') => word.to_string(),
        Some(word) => format!("\"{word}\""),
        None => value.to_string(),
    }
}
