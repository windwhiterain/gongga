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
    /// Byte offset just past the value.
    pub end: usize,
    pub kind: Reference,
    pub value: String,
}

/// Every reference in `text`, oldest first.
///
/// # Invariant
///
/// The value is one word: `@kind:` and the word after it, or `@kind:value` written
/// together, which is what the completion popup inserts. Trailing sentence
/// punctuation belongs to the sentence.
pub fn mentions(text: &str) -> Vec<Mention> {
    let words: Vec<(usize, usize)> = tokens(text);
    let mut out = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let (start, end) = words[index];
        index += 1;
        let word = &text[start..end];
        // Bracket and quote before the `@` belong to the sentence around it.
        let bare = word.trim_start_matches(['(', '[', '{', '"', '\'', '`']);
        let start = start + (word.len() - bare.len());
        let Some(kind) = Reference::of_prefix(bare) else {
            continue;
        };
        // `@image:<path>` carries its value; a bare `@image:` takes the next word.
        let (value, span_end) = match bare.strip_prefix(kind.prefix()) {
            Some(rest) if !rest.is_empty() => (rest.to_string(), end),
            _ => match words.get(index) {
                Some((next_start, next_end)) => {
                    index += 1;
                    (text[*next_start..*next_end].to_string(), *next_end)
                }
                // Nothing to name: not a reference, and the text keeps it.
                None => continue,
            },
        };
        let value = value
            .trim_end_matches([',', ';', ':', '.', '!', '?', ')', ']', '}', '"', '\'', '`'])
            .to_string();
        if value.is_empty() {
            continue;
        }
        out.push(Mention {
            start,
            end: span_end,
            kind,
            value,
        });
    }
    out
}

/// The byte spans of `text`'s whitespace-separated words.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(from) = start.take() {
                out.push((from, at));
            }
        } else if start.is_none() {
            start = Some(at);
        }
    }
    if let Some(from) = start {
        out.push((from, text.len()));
    }
    out
}

/// One reference's text: the spelling that names it, and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// `@file:notes.txt`, the canonical spelling a mention is matched against.
    pub spelling: String,
    pub content: String,
}

/// Where a message's references are read: this machine, or a session's remote.
///
/// # Invariant
///
/// One answer for one message: a reference is read on the machine that produced the
/// text it is written in, and never on whichever machine has a file of that name.
#[derive(Clone, Copy)]
pub enum Source<'a> {
    Local,
    Remote(&'a crate::remote::Reader),
    /// A remote session with no connection: every read fails.
    Unreachable,
}

impl Source<'_> {
    pub fn machine(&self) -> crate::session::Machine {
        match self {
            Source::Local => crate::session::Machine::Local,
            Source::Remote(_) | Source::Unreachable => crate::session::Machine::Remote,
        }
    }

    /// The bytes of `named`, resolved against `home` as that machine spells paths.
    pub async fn read(&self, home: &Path, named: &str) -> Option<Vec<u8>> {
        let path = self.spelling(home, named);
        match self {
            Source::Local => std::fs::read(path).ok(),
            Source::Remote(reader) => reader.read(&path).await.ok(),
            Source::Unreachable => None,
        }
    }

    /// The one path a name means on this machine.
    ///
    /// # Invariant
    ///
    /// A peer's path is joined the way its own shell joins it, not this platform's.
    pub fn spelling(&self, home: &Path, named: &str) -> String {
        match self {
            Source::Local => crate::image::resolve(home, named)
                .to_string_lossy()
                .into_owned(),
            Source::Remote(_) | Source::Unreachable => peer_path(&home.to_string_lossy(), named),
        }
    }
}

/// `named` as a peer spells it, resolved against the peer's `home`.
fn peer_path(home: &str, named: &str) -> String {
    let named = named.replace('\\', "/");
    let absolute = named.starts_with('/')
        || named.char_indices().nth(1).is_some_and(|(at, c)| {
            c == ':' && named.as_bytes()[0].is_ascii_alphabetic() && at == 1
        });
    if absolute || home.is_empty() {
        return named;
    }
    format!("{}/{}", home.trim_end_matches('/'), named)
}

/// The file a directory keeps its standing rules in.
///
/// # Invariant
///
/// One name in the directory the session stands in: a prompt is the rules of where it
/// stands, and no ancestor's copy is inherited.
pub const RULES: &str = "AGENTS.md";

/// The rules of `home`, then every reference `texts` name, and their own.
///
/// # Invariant
///
/// Everything is read in `home`, the session's own directory, never in the directory
/// its shell happens to stand in: a request already sent is never rewritten, so its map
/// must not follow a `cd`. The rules file is a root whether or not a message names it.
/// Breadth-first by first-seen order; what cannot be read is left out.
/// See `docs/notes/server-meta.md`.
pub async fn roots(source: Source<'_>, home: &Path, texts: &[&str]) -> Vec<Root> {
    // Seeded here rather than named by a message: the index holds it behind the prose.
    let first = format!("{}{}", Reference::File.prefix(), RULES);
    let mut queue: Vec<String> = vec![first.clone()];
    let mut seen: Vec<String> = vec![first];
    let mut out: Vec<Root> = Vec::new();
    for text in texts {
        for mention in mentions(text) {
            let spelling = spelling(&mention);
            if !seen.contains(&spelling) {
                seen.push(spelling.clone());
                queue.push(spelling);
            }
        }
    }
    let mut index = 0;
    while index < queue.len() {
        let named = queue[index].clone();
        index += 1;
        let Some(content) = content(source, home, &named).await else {
            continue;
        };
        for mention in mentions(&content) {
            let spelling = spelling(&mention);
            if !seen.contains(&spelling) {
                seen.push(spelling.clone());
                queue.push(spelling);
            }
        }
        out.push(Root {
            spelling: named,
            content,
        });
    }
    out
}

/// The spelling a reference is indexed under: its prefix and its value, written
/// together.
///
/// # Invariant
///
/// One spelling for one reference, whoever asks: the index carries these and
/// `expand` matches mentions against them, so a second way of writing it would
/// leave a message expanded and its text in the index twice.
pub fn spelling(mention: &Mention) -> String {
    format!("{}{}", mention.kind.prefix(), mention.value)
}

/// What one spelling stands for, or nothing when this build cannot read it.
///
/// # Invariant
///
/// A picture is not text and an unknown skill has no body: neither is invented, and
/// both leave the mention literal in the prompt.
async fn content(source: Source<'_>, home: &Path, spelling: &str) -> Option<String> {
    let kind = Reference::of_prefix(spelling)?;
    let value = spelling.strip_prefix(kind.prefix())?;
    match kind {
        Reference::Image => None,
        Reference::File => String::from_utf8(source.read(home, value).await?).ok(),
        Reference::Skill => skill_body(home, source.machine(), value),
    }
}

/// A skill's `SKILL.md` as written, frontmatter included.
fn skill_body(home: &Path, machine: crate::session::Machine, name: &str) -> Option<String> {
    // Only a row this server holds: a peer's checkout skill is not.
    let found = crate::tools::index_skill::installed_for(home, machine)
        .into_iter()
        .find(|skill| skill.name == name)?;
    std::fs::read_to_string(found.path).ok()
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

/// The spellings `text` leaves literal. See `docs/notes/server-meta.md`.
pub async fn literal(
    source: Source<'_>,
    home: &Path,
    text: &str,
    rooted: Option<&[String]>,
) -> Vec<String> {
    match rooted {
        Some(rooted) => rooted.to_vec(),
        None => roots(source, home, &[text])
            .await
            .into_iter()
            .map(|root| root.spelling)
            .collect(),
    }
}

/// Resolves the references one message carries, before it is stored.
///
/// # Invariant
///
/// A spelling in `indexed` -- text the prompt's index already carries -- stays
/// literal and keys that index. See `docs/notes/server-meta.md`.
///
/// An unresolvable reference stays in the text verbatim: an unreadable picture or
/// an unknown skill must still be a message. `llm` is `None` when no client
/// resolved, and then only skill references are expanded.
pub async fn expand(
    store: &crate::store::Store,
    source: Source<'_>,
    home: &Path,
    text: &str,
    llm: Option<&LlmClient>,
    indexed: &[&str],
) -> Expanded {
    let mut out = Expanded::default();
    let mut cursor = 0;
    for mention in mentions(text) {
        if mention.start < cursor || indexed.contains(&spelling(&mention).as_str()) {
            continue;
        }
        let (replacement, image) = match mention.kind {
            Reference::File => continue,
            Reference::Skill => match skill_metadata(home, source.machine(), &mention.value) {
                Some(metadata) => (metadata, None),
                None => continue,
            },
            Reference::Image => match picture(store, source, home, &mention.value, llm).await {
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
            },
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

/// What `@skill: <name>` becomes: the spelling it keeps, and the `SKILL.md` it names.
///
/// # Invariant
///
/// The spelling survives, so the index keys the entry a message wrote: the body is
/// charged once in the prompt's index and the row keeps a pointer to it. See
/// `docs/notes/server-meta.md`.
fn skill_metadata(home: &Path, machine: crate::session::Machine, name: &str) -> Option<String> {
    let found = crate::tools::index_skill::installed_for(home, machine)
        .into_iter()
        .find(|skill| skill.name == name)?;
    Some(format!(
        "{} {} ({})",
        Reference::Skill.prefix(),
        found.name,
        found.path.display()
    ))
}

/// The picture `@image: <path>` names, copied into this home.
///
/// # Invariant
///
/// The copy is the whole record, and the upload happens later: a request resolves the
/// copy to whatever file the provider holds then, and inlines the bytes when it holds
/// none. A picture named here is therefore never lost to a Files API that is down.
async fn picture(
    store: &crate::store::Store,
    source: Source<'_>,
    home: &Path,
    given: &str,
    llm: Option<&LlmClient>,
) -> Option<ImageRef> {
    let path = source.spelling(home, given);
    let bytes = source.read(home, given).await?;
    let media = image::media_type(std::path::Path::new(&path), &bytes)?;
    let llm = llm?;
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
