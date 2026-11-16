//! A command's arguments as one JSON Schema document.
//!
//! See `docs/notes/client-complete.md`.

use serde_json::{Map, Value, json};

use crate::{Accepts, ArgSpec, CompletionSource};

/// The one keyword this document uses that JSON Schema does not define.
///
/// # Invariant
///
/// Only what the standard cannot state lives here: the live source a value is
/// completed from, the sets keyed by model, the value placeholder, the sticky kind,
/// the model-reference flag and the struct-update flag. The standard says an unknown
/// keyword is an annotation, so a reader that knows no `x-` keyword reads a plain
/// schema.
pub const EXTENSION: &str = "x-completion";

/// Everything one property is written from, whichever row states it.
pub struct Row<'a> {
    /// SPEC's type, e.g. `optional[path]`.
    pub type_of: Option<&'a str>,
    pub description: &'a str,
    /// The object this property holds, when it holds one.
    pub object: Option<Value>,
    pub accepts: Option<&'a Accepts>,
    pub source: &'a CompletionSource,
    /// The sticky kind: two arguments of one kind share one entry.
    pub kind: Option<&'a str>,
    pub hint: Option<&'a str>,
    /// SPEC's default, spelled as SPEC spells it.
    pub default: Option<&'a str>,
    pub model_ref: bool,
    /// SPEC's `T{..}`: an update says what changes, not what the value is.
    pub update: bool,
    /// The sibling whose value chooses the schema, and the documents its values
    /// choose from, by value.
    pub schema_of: Option<&'a str>,
    pub schemas: Option<&'a std::collections::BTreeMap<String, Value>>,
}

/// The arguments of one command, as the schema of the object they are written as.
pub fn of(args: &[ArgSpec]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in args {
        properties.insert(
            arg.name.clone(),
            property(&Row {
                type_of: Some(&arg.type_of),
                description: &arg.description,
                object: arg.schema.clone(),
                accepts: arg.accepts.as_ref(),
                source: &arg.completions,
                kind: arg.kind.as_deref(),
                hint: arg.value_hint.as_deref(),
                default: None,
                model_ref: false,
                update: arg.update,
                schema_of: None,
                schemas: None,
            }),
        );
        if arg.required {
            required.push(Value::String(arg.name.clone()));
        }
    }
    let mut document = object(properties);
    if !required.is_empty() {
        document["required"] = Value::Array(required);
    }
    document
}

/// One property: the standard's keywords first, then what they cannot say.
pub fn property(row: &Row<'_>) -> Value {
    let mut out = Map::new();
    match &row.object {
        // A structured value is an object of its own fields.
        Some(inner) => {
            out.insert("type".into(), json!("object"));
            if let Some(properties) = inner.get("properties") {
                out.insert("properties".into(), properties.clone());
            }
            if let Some(required) = inner.get("required") {
                out.insert("required".into(), required.clone());
            }
        }
        None => match row.type_of.map(crate::argtype::schema_types) {
            Some(Ok(types)) if types.len() == 1 => {
                out.insert("type".into(), json!(types[0].to_string()));
            }
            Some(Ok(types)) => {
                out.insert(
                    "type".into(),
                    json!(types.iter().map(|t| t.to_string()).collect::<Vec<_>>()),
                );
            }
            // A spelling the table refuses states nothing: an empty schema accepts
            // any value, which is what the reader then reads.
            Some(Err(_)) | None => {}
        },
    }
    if !row.description.is_empty() {
        out.insert("description".into(), json!(row.description));
    }
    if let Some(value) = row.default {
        out.insert("default".into(), json!(value));
    }
    // A closed set of values is an `enum`, wherever it was stated.
    match (row.accepts, row.source) {
        (Some(Accepts::Same(values)), _) => {
            out.insert("enum".into(), json!(values));
        }
        (None, CompletionSource::Static(values)) => {
            out.insert("enum".into(), json!(values));
        }
        _ => {}
    }
    let mut extension = Map::new();
    // A document may choose its own fields on a sibling's value.
    if let Some(chosen) = row.object.as_ref().and_then(|inner| inner.get(EXTENSION)) {
        for key in ["schema_of", "schemas"] {
            if let Some(value) = chosen.get(key) {
                extension.insert(key.into(), value.clone());
            }
        }
    }
    if let Some(accepts @ Accepts::PerModel(_)) = row.accepts {
        extension.insert("accepts".into(), json!(accepts));
    }
    // Carried as the enum states it, so reading it back is the same serde pair.
    if !matches!(
        row.source,
        CompletionSource::None | CompletionSource::Static(_)
    ) {
        extension.insert("source".into(), json!(row.source));
    }
    if let Some(kind) = row.kind {
        extension.insert("kind".into(), json!(kind));
    }
    if let Some(hint) = row.hint {
        extension.insert("hint".into(), json!(hint));
    }
    if row.model_ref {
        extension.insert("model_ref".into(), json!(true));
    }
    if row.update {
        extension.insert("update".into(), json!(true));
    }
    // A schema chosen by a sibling's value: what `oneOf` cannot say without
    // spelling every choice twice.
    if let Some(sibling) = row.schema_of {
        extension.insert("schema_of".into(), json!(sibling));
    }
    if let Some(schemas) = row.schemas {
        extension.insert("schemas".into(), json!(schemas));
    }
    if !extension.is_empty() {
        out.insert(EXTENSION.into(), Value::Object(extension));
    }
    Value::Object(out)
}

/// An object schema over `properties`, closed.
pub fn object(properties: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), Value::Object(properties));
    out.insert("additionalProperties".into(), json!(false));
    Value::Object(out)
}

/// The properties of an object schema.
pub fn properties(schema: &Value) -> Option<&Map<String, Value>> {
    schema.get("properties")?.as_object()
}

/// The names an object schema insists on.
pub fn required(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether a property is a structured value: it states fields of its own.
///
/// # Invariant
///
/// `properties`, not `type`. A model reference is an object whose members the
/// document does not spell out, and reading it as a struct dropped it.
pub fn is_struct(property: &Value) -> bool {
    property.get("properties").is_some()
}

/// What a property says about itself, in the popup's right-hand column.
pub fn description(property: &Value) -> String {
    property
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The default the type decides, when it states one.
pub fn default(property: &Value) -> Option<&str> {
    property.get("default").and_then(Value::as_str)
}

/// What this document added to the standard's keywords, when it added anything.
pub fn extension<'a>(property: &'a Value, key: &str) -> Option<&'a Value> {
    property.get(EXTENSION)?.get(key)
}

/// The sticky kind: two properties of one kind share one entry.
pub fn kind(property: &Value) -> Option<&str> {
    extension(property, "kind").and_then(Value::as_str)
}

/// The live source a value is completed from, as the catalog stated it.
pub fn source(property: &Value) -> Option<CompletionSource> {
    serde_json::from_value(extension(property, "source")?.clone()).ok()
}

/// The value sets keyed by model, when the property states them that way.
pub fn accepts(property: &Value) -> Option<Accepts> {
    serde_json::from_value(extension(property, "accepts")?.clone()).ok()
}

/// The value placeholder the popup shows.
pub fn hint(property: &Value) -> String {
    extension(property, "hint")
        .and_then(Value::as_str)
        .unwrap_or("value")
        .to_string()
}

/// Whether the operator enters this property's value as an object.
pub fn enters(property: &Value) -> bool {
    is_struct(property) || schema_of(property).is_some()
}

/// The sibling whose value chooses this property's schema.
pub fn schema_of(property: &Value) -> Option<&str> {
    extension(property, "schema_of").and_then(Value::as_str)
}

/// The schemas a sibling's values choose from, by value.
pub fn schemas(property: &Value) -> Option<&Map<String, Value>> {
    extension(property, "schemas")?.as_object()
}

/// Whether this property's value is the object SPEC spells `Model`.
pub fn model_ref(property: &Value) -> bool {
    extension(property, "model_ref")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
