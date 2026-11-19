//! The `@` references a message may carry. See `docs/notes/protocol-reference.md`.

/// What a reference names, and the one spelling that names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reference {
    /// A picture, attached rather than described.
    Image,
    /// A skill, replaced by its metadata.
    Skill,
    /// A file, left exactly as written.
    File,
}

impl Reference {
    /// The three spellings, in the order a reader meets them.
    ///
    /// # Invariant
    ///
    /// The server parses these and the client completes them, so one list is what
    /// keeps a completed reference parseable.
    pub const ALL: [Reference; 3] = [Reference::Image, Reference::Skill, Reference::File];

    /// What one reference is called, between its `@` and its `(`.
    pub fn name(self) -> &'static str {
        match self {
            Reference::Image => "image",
            Reference::Skill => "skill",
            Reference::File => "file",
        }
    }

    /// The spelling that opens one: `@file(`, the value and its own `)` to follow.
    ///
    /// # Invariant
    ///
    /// The opening parenthesis is part of the spelling rather than punctuation after
    /// it: `@file` with no `(` names nothing, so a sentence that says `@file` is a
    /// sentence and not a reference to a file called nothing.
    pub fn prefix(self) -> &'static str {
        match self {
            Reference::Image => "@image(",
            Reference::Skill => "@skill(",
            Reference::File => "@file(",
        }
    }

    /// The kind `word` opens, when it opens one.
    pub fn of_prefix(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| word.starts_with(kind.prefix()))
    }
}

/// The prefix that names a machine inside a reference's own group.
///
/// # Invariant
///
/// One group and one value: `@file(remote://p/notes.md)` is a peer's file, and
/// `@file(notes.md)` is the session's. The server parses this and the client completes
/// it, so one spelling is what keeps a machine a caller picked parseable. See
/// `docs/notes/protocol-reference.md`.
pub fn meta_prefix() -> &'static str {
    "remote://"
}

/// The name that stands for the machine a reference is read on, when it must be said.
///
/// # Invariant
///
/// A bare value is the session's machine, so a caller holding a file of *this* machine's
/// has to say so: `@image(remote://local/C:\x.png)` is the clipboard's own picture. One
/// name for it, shared by the parser that reads it and the window that writes it.
pub fn local_machine() -> &'static str {
    "local"
}
