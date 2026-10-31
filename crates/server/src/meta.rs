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

/// Resolves the references one message carries, before it is stored.
///
/// # Invariant
///
/// An unresolvable reference stays in the text verbatim: an unreadable picture or
/// an unknown skill must still be a message. `llm` is `None` when no client
/// resolved, and then only skill references are expanded.
pub async fn expand(cwd: &Path, text: &str, llm: Option<&LlmClient>) -> Expanded {
    let mut out = Expanded::default();
    let mut cursor = 0;
    for mention in mentions(text) {
        if mention.start < cursor {
            continue;
        }
        let (replacement, image) = match mention.kind {
            Reference::File => continue,
            Reference::Skill => match skill_metadata(&mention.value) {
                Some(metadata) => (metadata, None),
                None => continue,
            },
            Reference::Image => match picture(cwd, &mention.value, llm).await {
                Some(image) => (String::new(), Some(image)),
                None => continue,
            },
        };
        // A reference that stood on its own takes one space with it, so the
        // sentence does not close up around a hole.
        let from = if mention.start > cursor && text[cursor..mention.start].ends_with(' ') {
            mention.start - 1
        } else {
            mention.start
        };
        out.text.push_str(&text[cursor..from]);
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

/// What `@skill: <name>` stands for: one row of `index_skill`'s table.
fn skill_metadata(name: &str) -> Option<String> {
    let found = crate::tools::index_skill::installed()
        .into_iter()
        .find(|skill| skill.name == name)?;
    Some(format!(
        "skill `{}` ({}) {}; SKILL.md: {}",
        found.name,
        found.bucket,
        found.description,
        found.path.display()
    ))
}

/// The picture `@image: <path>` names, uploaded when the model can see one.
///
/// # Invariant
///
/// An upload that fails still attaches the picture: `image::content_part` inlines
/// the bytes when there is no file id, so an unreachable File API costs a bigger
/// request and never the message.
async fn picture(cwd: &Path, given: &str, llm: Option<&LlmClient>) -> Option<ImageRef> {
    let path = image::resolve(cwd, given);
    let bytes = std::fs::read(&path).ok()?;
    let media = image::media_type(&path, &bytes)?;
    let mut reference = ImageRef::new(path.to_string_lossy().to_string(), media);
    if let Some(llm) = llm {
        if !llm.accepts_image() {
            tracing::warn!(path = %reference.path, "a picture was named for a model that cannot see one");
            return None;
        }
        match llm
            .upload_image(&reference.path, &reference.media_type)
            .await
        {
            Ok(id) => reference.file_id = Some(id),
            Err(e) => tracing::warn!(
                path = %reference.path,
                error = %e,
                "could not upload a named picture; the request carries its bytes"
            ),
        }
    }
    Some(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(Reference, String)> {
        mentions(text)
            .into_iter()
            .map(|m| (m.kind, m.value))
            .collect()
    }

    /// The same list, spelled as the assertions read it.
    fn pairs(text: &str) -> Vec<(Reference, String)> {
        found(text)
    }

    #[test]
    fn a_reference_is_its_prefix_and_one_word() {
        assert_eq!(
            found("@image: shot.png"),
            vec![(Reference::Image, "shot.png".to_string())]
        );
        assert_eq!(
            found("see @image: a/b.png now"),
            vec![(Reference::Image, "a/b.png".to_string())]
        );
        assert_eq!(
            found("@image:a/b.png"),
            vec![(Reference::Image, "a/b.png".to_string())]
        );
        assert_eq!(
            found("@skill: heir"),
            vec![(Reference::Skill, "heir".to_string())]
        );
        assert_eq!(
            found("@file: notes.txt"),
            vec![(Reference::File, "notes.txt".to_string())]
        );
    }

    #[test]
    fn a_prefix_matches_anywhere_and_punctuation_is_the_sentence() {
        assert_eq!(
            found("(@image: shot.png)."),
            vec![(Reference::Image, "shot.png".to_string())]
        );
        assert_eq!(
            found("@image: shot.png."),
            vec![(Reference::Image, "shot.png".to_string())]
        );
        // A prefix with nothing after it is prose.
        assert!(found("write @image:").is_empty());
        // An email is not a reference.
        assert!(found("mail me at user@host.png").is_empty());
    }

    #[test]
    fn a_file_reference_is_left_exactly_as_written() {
        let expanded = block_on(expand(Path::new("."), "@file: notes.txt", None));
        assert_eq!(expanded.text, "@file: notes.txt");
        assert!(expanded.images.is_empty());
    }

    #[test]
    fn an_unresolvable_reference_stays_text() {
        let expanded = block_on(expand(Path::new("."), "see @image: gone.png", None));
        assert_eq!(expanded.text, "see @image: gone.png");
        assert!(expanded.images.is_empty());
    }

    /// This module has no async test harness; one runtime serves each case.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
}
