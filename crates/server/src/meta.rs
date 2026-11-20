//! The `@` references a message may carry. See `docs/notes/server-meta.md`.

use std::path::Path;

use protocol::Reference;

use crate::image::{self, ImageRef};
use crate::llm::LlmClient;

/// One reference: where it sits in the text, what it is, what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// Byte offset of the opening `@`.
    pub start: usize,
    /// Byte offset just past the closing `)`.
    pub end: usize,
    pub kind: Reference,
    /// The value as the reference means it, the machine prefix taken off.
    pub value: String,
    /// The machine this reference names, when it names one; `none` is the session's own.
    pub remote: Option<String>,
}

/// The prefix that names a machine inside a reference's own group.
///
/// # Invariant
///
/// One group and one value: `@file(remote://p/notes.md)` is a peer's file, and
/// `@file(notes.md)` is the session's. The group is what a caller has to close, so a
/// second group would be a second thing to complete and a second thing to forget. See
/// `docs/notes/server-meta.md`.
pub use protocol::meta_prefix;

/// Every reference in `text`, oldest first.
///
/// # Invariant
///
/// A reference is `@kind(value)`, the parentheses doing the delimiting: the value ends
/// where its group closes, so no prose around it is guessed at -- `@` need not open a
/// word, and a name need not stop at a space. A value that opens `remote://<name>/`
/// names the machine to read it on. See `docs/notes/server-meta.md`.
pub fn mentions(text: &str) -> Vec<Mention> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(index) = text[cursor..].find('@') {
        let start = cursor + index;
        // Past the `@`: a `@` that opens nothing does not hold the walk, and the
        // next reference may open inside the same word.
        cursor = start + 1;
        let Some(kind) = Reference::of_prefix(&text[start..]) else {
            continue;
        };
        let Some((value, end)) = group(text, start + kind.prefix().len()) else {
            continue;
        };
        let value = unquote(value.trim());
        if value.is_empty() {
            continue;
        }
        cursor = end;
        let (remote, value) = split_remote(value);
        out.push(Mention {
            start,
            end,
            kind,
            value: value.to_string(),
            remote,
        });
    }
    out
}

/// A value split at its machine prefix: `remote://p/x` is peer `p` and `x`.
///
/// # Invariant
///
/// Read only when the prefix is complete -- a name and a `/` after it. A value that
/// merely starts with those letters is a path that starts with those letters.
fn split_remote(value: &str) -> (Option<String>, &str) {
    let Some(rest) = value.strip_prefix(meta_prefix()) else {
        return (None, value);
    };
    let Some((name, path)) = rest.split_once('/') else {
        return (None, value);
    };
    let name = name.trim();
    if name.is_empty() {
        return (None, value);
    }
    (Some(name.to_string()), path)
}

/// The value of the group whose `(` is the byte before `open`, and the byte just
/// past its `)`.
///
/// # Invariant
///
/// A group is read, not searched for: `(` opens one and `)` closes it, nested ones
/// count, and a quoted run carries its own -- so a name holding either character is
/// written as it is. A group with no closing `)` is not a reference at all: the
/// operator is still typing, and prose is not a hole.
fn group(text: &str, open: usize) -> Option<(&str, usize)> {
    // The `(` is behind `open`, so this group is already one deep.
    let mut depth = 1usize;
    let mut quote: Option<char> = None;
    for (at, c) in text[open..].char_indices() {
        if let Some(opened) = quote {
            if c == opened {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[open..open + at], open + at + c.len_utf8()));
                }
            }
            _ => {}
        }
    }
    None
}

/// A value written in quotes, without them: the way to name something that itself
/// starts or ends with a space.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// One reference's text: the spelling that names it, and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// `@file(notes.txt)`, the canonical spelling a mention is matched against.
    pub spelling: String,
    pub content: String,
}

/// The skill that teaches how a session on a peer is put together.
///
/// # Invariant
///
/// Seeded into the index of every session whose directory is on a peer, because the
/// tool instructions deliberately say nothing about the two machines: a model that is
/// never told reads `remote_bash` as a second `bash`. See `docs/notes/server-meta.md`.
pub const REMOTE_SKILL: &str = "remote";

/// The file a directory keeps its standing rules in.
///
/// # Invariant
///
/// One name in the directory the session stands in: a prompt is the rules of where it
/// stands, and no ancestor's copy is inherited.
pub const RULES: &str = "AGENTS.md";

/// The spelling the directory's rules are indexed under.
pub fn rules_spelling() -> String {
    format!("@{}({})", Reference::File.name(), RULES)
}

/// The rules of `home`, as the file says them now, or nothing when it keeps none.
///
/// # Invariant
///
/// Not a memory module: what this returns opens the index on every request, verbatim,
/// so no plan can reorder it, patch it, or let it expire, and the model reads the rules
/// that are on disk rather than the ones a provider was holding.
pub async fn rules(machines: &Machines<'_>, home: &Path) -> Option<String> {
    content(machines, home, &rules_spelling(), None, RULES).await
}

/// Every reference the rules name, every reference `texts` name, and their own.
///
/// # Invariant
///
/// Everything is read in `home`, the session's own directory, never in the directory
/// its shell happens to stand in: a request already sent is never rewritten, so its map
/// must not follow a `cd`. The rules file is no entry, only what it names; breadth-first
/// by first-seen order, and what cannot be read is left out.
/// See `docs/notes/server-meta.md`.
pub async fn roots(machines: &Machines<'_>, home: &Path, texts: &[&str]) -> Vec<Root> {
    // Named by a message or met by the walk, the rules file is never an entry.
    let mut seen: Vec<String> = vec![rules_spelling()];
    let mut queue: Vec<(String, Option<String>, String)> = Vec::new();
    if let Some(rules) = rules(machines, home).await {
        for mention in mentions(&rules) {
            enqueue(&mut queue, &mut seen, &mention);
        }
    }
    // The shape of a session on a peer, told to it without being asked.
    if !machines.session.is_local() {
        let skill = Mention {
            start: 0,
            end: 0,
            kind: Reference::Skill,
            value: REMOTE_SKILL.to_string(),
            remote: None,
        };
        enqueue(&mut queue, &mut seen, &skill);
    }
    for text in texts {
        for mention in mentions(text) {
            enqueue(&mut queue, &mut seen, &mention);
        }
    }
    let mut out: Vec<Root> = Vec::new();
    let mut index = 0;
    while index < queue.len() {
        let (named, remote, value) = queue[index].clone();
        index += 1;
        let Some(body) = content(machines, home, &named, remote.as_deref(), &value).await else {
            continue;
        };
        for mention in mentions(&body) {
            enqueue(&mut queue, &mut seen, &mention);
        }
        out.push(Root {
            spelling: named,
            content: body,
        });
    }
    out
}

/// `mention` at the end of the walk's queue, unless it is already there.
fn enqueue(
    queue: &mut Vec<(String, Option<String>, String)>,
    seen: &mut Vec<String>,
    mention: &Mention,
) {
    let named = spelling(mention);
    if seen.contains(&named) {
        return;
    }
    seen.push(named.clone());
    queue.push((named, mention.remote.clone(), mention.value.clone()));
}

/// The spelling a reference is indexed under: its prefix, its value, and its machine.
///
/// # Invariant
///
/// One spelling for one reference, whoever asks: the index carries these and
/// `expand` writes one back into every `@skill(...)` mention, so a second way of
/// writing it would make the same file two entries. The machine is part of the
/// spelling because `@file(x)` on this machine and `@file(x)` on a peer are two
/// different texts. See `docs/notes/server-meta.md`.
pub fn spelling(mention: &Mention) -> String {
    format!(
        "@{}({})",
        mention.kind.name(),
        spelled(mention.remote.as_deref(), &mention.value)
    )
}

/// A value as a group writes it: the machine prefix when one is named.
///
/// # Invariant
///
/// The inverse of `split_remote`, so a spelling that came from a mention parses back to
/// the same mention. See `docs/notes/server-meta.md`.
pub fn spelled(remote: Option<&str>, value: &str) -> String {
    match remote.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("{}{name}/{value}", meta_prefix()),
        None => value.to_string(),
    }
}

/// The machine a reference stands on: a skill is the server's own, a file the peer's.
///
/// # Invariant
///
/// The machine is the one the reference named, not the one the message was written on:
/// `@file(remote://p/x)` on a local session says `on remote p`, and a skill is always
/// this server's own because it runs here. See `docs/notes/server-meta.md`.
pub fn machine_note(spelling: &str, space: &crate::space::Space, remote: Option<&str>) -> String {
    let server =
        matches!(Reference::of_prefix(spelling), Some(Reference::Skill)) || space.is_local();
    if server {
        "on the server".to_string()
    } else {
        format!("on remote {}", remote.unwrap_or("the peer"))
    }
}

/// What one spelling stands for, or nothing when this build cannot read it.
///
/// # Invariant
///
/// A picture is not text and an unknown skill has no body: neither is invented, and
/// both leave the mention literal in the prompt. The machine is the one the reference
/// named, and its own spelling of the path.
async fn content(
    machines: &Machines<'_>,
    home: &Path,
    spelling: &str,
    remote: Option<&str>,
    value: &str,
) -> Option<String> {
    let kind = Reference::of_prefix(spelling)?;
    let space = machines.in_space(remote);
    match kind {
        Reference::Image => None,
        Reference::File => {
            let path = space.spell(home, value);
            String::from_utf8(space.read_bytes(&path).await?).ok()
        }
        Reference::Skill => skill_body(&space, home, value).await,
    }
}

/// A skill's `SKILL.md` as written, with what its publisher generated beside it.
///
/// # Invariant
///
/// One body, both halves, and neither re-rendered: the prose is the file's, and the
/// arguments are the generated document's bytes, trusted rather than read out of the
/// package again. `SKILL.md` states no second copy of them. A skill found on a peer is
/// read through that peer's own space, so a project's skill there is a skill here.
async fn skill_body(space: &crate::space::Space, home: &Path, name: &str) -> Option<String> {
    let found = crate::tools::skill::find(space, home, name, None).await?;
    // The path says which machine holds it; that machine reads it.
    let here = crate::space::Space::local();
    let (path, space) = match crate::tools::skill::machine_of(&found.path) {
        Some((None, plain)) => (plain.to_string(), &here),
        _ => (found.path.clone(), space),
    };
    let mut body = String::from_utf8(space.read_bytes(&path).await?).ok()?;
    let prompts = crate::prompts::shared();
    if let Some(template) = prompts.text(crate::prompts::SKILL_ARGUMENTS) {
        let beside = format!("{}/schema.json", found.dir.trim_end_matches('/'));
        if let Some(arguments) = space.read_bytes(&beside).await {
            if let Ok(arguments) = String::from_utf8(arguments) {
                body.push_str("\n\n");
                body.push_str(&crate::prompts::substitute(
                    &template,
                    &[("arguments", arguments.trim())],
                ));
            }
        }
    }
    Some(body)
}

/// Which machine a reference is read on, resolved once per mention.
///
/// # Invariant
///
/// The session's own machine unless the reference names another, and one answer per
/// reference: a message may name a file here and a picture on a peer, and each is read
/// where it says. A name is resolved through the space's own catalog of peers, so the
/// one value a caller already holds also answers "and which other machine could this
/// be?". See `docs/notes/server-meta.md`.
#[derive(Clone, Copy)]
pub struct Machines<'a> {
    /// The space of the machine the text was written on, and what an unnamed reference
    /// is read in.
    pub session: &'a crate::space::Space,
}

impl<'a> Machines<'a> {
    /// The machines one session can read a reference on.
    pub fn of(session: &'a crate::space::Space) -> Machines<'a> {
        Machines { session }
    }

    /// The space one reference is read in.
    ///
    /// # Invariant
    ///
    /// A stated machine this server cannot name falls back to the session's own, so a
    /// typo in a message reads as it would have without one rather than as a hole.
    pub fn in_space(&self, remote: Option<&str>) -> crate::space::Space {
        match remote.map(str::trim).filter(|name| !name.is_empty()) {
            None | Some("none") => self.session.clone(),
            Some(name) => self.session.peer(name),
        }
    }
}

/// A message with its references resolved, and the pictures they named.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    pub images: Vec<ImageRef>,
}

impl Expanded {
    /// The json a pending message keeps: its text, and what it pointed at.
    pub fn to_json(&self) -> serde_json::Value {
        let mut row = serde_json::json!({"message": self.text});
        if !self.images.is_empty() {
            row["images"] = serde_json::json!(
                self.images
                    .iter()
                    .map(ImageRef::to_json)
                    .collect::<Vec<_>>()
            );
        }
        row
    }

    /// Reads back what [`Expanded::to_json`] wrote.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            text: value.get("message")?.as_str()?.to_string(),
            images: value
                .get("images")
                .and_then(|images| images.as_array())
                .map(|images| images.iter().filter_map(ImageRef::from_json).collect())
                .unwrap_or_default(),
        })
    }
}

/// Inlines a summary's text references so the artifact stands alone; the
/// unreadable and any picture stay literal.
pub async fn embed(machines: &Machines<'_>, home: &Path, text: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    for mention in mentions(text) {
        if mention.start < cursor {
            continue;
        }
        let Some(body) = content(
            machines,
            home,
            &spelling(&mention),
            mention.remote.as_deref(),
            &mention.value,
        )
        .await
        else {
            continue;
        };
        let from = if mention.start > cursor && text[cursor..mention.start].ends_with(' ') {
            mention.start - 1
        } else {
            mention.start
        };
        out.push_str(&text[cursor..from]);
        out.push_str(&body);
        cursor = mention.end;
    }
    out.push_str(&text[cursor..]);
    out.trim().to_string()
}

/// Resolves the references one message carries, before it is stored.
///
/// # Invariant
///
/// A `@skill(...)` mention keeps its spelling and gains the `SKILL.md` it names, even
/// when the index already carries that body: a body never says where it lives.
///
/// An unresolvable reference stays in the text verbatim: an unreadable picture or
/// an unknown skill must still be a message. `llm` is `None` when no client
/// resolved, and then only skill references are expanded.
pub async fn expand(
    store: &crate::store::Store,
    machines: &Machines<'_>,
    home: &Path,
    text: &str,
    llm: Option<&LlmClient>,
) -> Expanded {
    let mut out = Expanded::default();
    let mut cursor = 0;
    for mention in mentions(text) {
        if mention.start < cursor {
            continue;
        }
        let (replacement, image) = match mention.kind {
            Reference::File => continue,
            Reference::Skill => match crate::tools::skill::find(
                &machines.in_space(mention.remote.as_deref()),
                home,
                &mention.value,
                mention.remote.as_deref(),
            )
            .await
            {
                Some(skill) => {
                    // Idempotent: a mention already carrying its ` (path)`
                    // suffix is left alone, or a re-sent row grows another.
                    if text[mention.end..].starts_with(&format!(" ({})", skill.path)) {
                        continue;
                    }
                    (
                        format!(
                            "@{}({}) ({})",
                            Reference::Skill.name(),
                            spelled(mention.remote.as_deref(), &skill.name),
                            skill.path
                        ),
                        None,
                    )
                }
                None => continue,
            },
            Reference::Image => {
                // Idempotent, the same rule by the copy's id: text already
                // carrying ` (id)` is not copied to or appended again.
                let space = machines.in_space(mention.remote.as_deref());
                let named = space.spell(home, &mention.value);
                let kept_copy = store
                    .picture_of(&named)
                    .map(|(id, _)| format!(" ({id})"))
                    .filter(|suffix| text[mention.end..].starts_with(suffix));
                if kept_copy.is_some() {
                    continue;
                }
                match picture(store, &space, home, &mention.value, llm).await {
                    Some(image) => {
                        // SPEC keeps the mention and appends the copy it stands for, so
                        // the row still says which picture it was sent with.
                        let spelled = &text[mention.start..mention.end];
                        let kept = match image.copy {
                            Some(copy) => format!("{spelled} ({copy})"),
                            None => spelled.to_string(),
                        };
                        (kept, Some(image))
                    }
                    None => continue,
                }
            }
        };
        out.text.push_str(&text[cursor..mention.start]);
        out.text.push_str(&replacement);
        cursor = mention.end;
        if let Some(image) = image {
            if !out.images.contains(&image) {
                out.images.push(image);
            }
        }
    }
    out.text.push_str(&text[cursor..]);
    out.text = out.text.trim().to_string();
    out
}

/// The picture `@image(<path>)` names, copied into this home.
///
/// # Invariant
///
/// The copy is the whole record, and the upload happens later: a request resolves the
/// copy to whatever file the provider holds then, and inlines the bytes when it holds
/// none. A picture named here is therefore never lost to a Files API that is down.
async fn picture(
    store: &crate::store::Store,
    space: &crate::space::Space,
    home: &Path,
    given: &str,
    llm: Option<&LlmClient>,
) -> Option<ImageRef> {
    let path = space.spell(home, given);
    // Said, not swallowed: a silent `?` made one bug look like "nothing happened".
    let Some(bytes) = space.read_bytes(&path).await else {
        tracing::warn!(path = %path, space = %space.describe(), "a picture was named and nothing could be read there");
        return None;
    };
    let Some(media) = image::media_type(&bytes) else {
        tracing::warn!(path = %path, bytes = bytes.len(), "a picture was named and the bytes are not one it takes");
        return None;
    };
    let Some(llm) = llm else {
        tracing::warn!(path = %path, "a picture was named with no model client to send it to");
        return None;
    };
    if !llm.accepts_image() {
        tracing::warn!(path = %path, "a picture was named for a model that cannot see one");
        return None;
    }
    let mut reference = ImageRef::new(path, media);
    // The copy is the whole record: no upload happens here. The provider's file
    // is resolved when the request is written.
    reference.copy = crate::image::pictures::remember(store, &reference.path, media, &bytes).ok();
    Some(reference)
}
