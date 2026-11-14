//! What a schema leaf's value is, read from the type the schema states.
//!
//! See `docs/notes/client-complete.md`.
//!
//! # Invariant
//!
//! One reading for both writers of a value into a partly typed object: the
//! completion's token and the sticky fill's value. So a string is quoted and a
//! switch is `true` wherever either writes.

use protocol::StructField;

/// What a leaf's value is, in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    /// A string, always quoted.
    Text,
    /// A switch: `true` or `false`, never quoted.
    Switch,
    /// A number, an object, a list or a type the schema does not state: the text
    /// stands as it reads.
    Other,
}

impl Leaf {
    /// The leaf `field` describes.
    ///
    /// # Invariant
    ///
    /// Read from the stated type through `protocol::argtype`, the one table that
    /// maps SPEC's spelling to the JSON image; a spelling that table refuses is a
    /// leaf nothing is known about, never a silent string.
    pub fn of(field: &StructField) -> Self {
        let Some(spelling) = field.type_of.as_deref() else {
            return Leaf::Other;
        };
        let Ok(types) = protocol::argtype::schema_types(spelling) else {
            return Leaf::Other;
        };
        // A union that admits a string is typed as one: the operator may write it.
        if types.contains(&"string") {
            Leaf::Text
        } else if types.contains(&"boolean") {
            Leaf::Switch
        } else {
            Leaf::Other
        }
    }

    /// `text` as the JSON token this leaf takes, for a partly typed object.
    pub fn token(self, text: &str) -> String {
        if self.literal(text) {
            return text.to_string();
        }
        serde_json::Value::String(text.to_string()).to_string()
    }

    /// `text` as the value a structured argument carries.
    pub fn value(self, text: &str) -> serde_json::Value {
        match (self, self.literal(text)) {
            (Leaf::Switch, true) => serde_json::Value::Bool(text == "true"),
            (Leaf::Other, true) => serde_json::from_str(text)
                .unwrap_or_else(|_| serde_json::Value::String(text.to_string())),
            _ => serde_json::Value::String(text.to_string()),
        }
    }

    /// Whether `text` stands as itself, without quotes.
    ///
    /// # Invariant
    ///
    /// The type decides, never the spelling of one candidate: a string leaf whose
    /// value reads as a number is still a string (`ctx_limit` takes `"70%"`, and
    /// the same list's `"0"` is not a number).
    fn literal(self, text: &str) -> bool {
        match self {
            Leaf::Text => false,
            Leaf::Switch => matches!(text, "true" | "false"),
            Leaf::Other => matches!(
                serde_json::from_str::<serde_json::Value>(text),
                Ok(serde_json::Value::Bool(_)
                    | serde_json::Value::Null
                    | serde_json::Value::Number(_))
            ),
        }
    }
}
