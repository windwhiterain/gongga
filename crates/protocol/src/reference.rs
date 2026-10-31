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

    pub fn prefix(self) -> &'static str {
        match self {
            Reference::Image => "@image:",
            Reference::Skill => "@skill:",
            Reference::File => "@file:",
        }
    }

    /// The kind `word` opens, when it opens one.
    pub fn of_prefix(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| word.starts_with(kind.prefix()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prefix_names_one_kind() {
        for kind in Reference::ALL {
            assert_eq!(Reference::of_prefix(kind.prefix()), Some(kind));
            assert_eq!(
                Reference::of_prefix(&format!("{}x", kind.prefix())),
                Some(kind)
            );
        }
        assert_eq!(Reference::of_prefix("@image"), None);
        assert_eq!(Reference::of_prefix("user@host"), None);
    }
}
