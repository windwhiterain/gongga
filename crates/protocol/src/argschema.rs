//! A command's arguments as one JSON Schema document.
//!
//! See `docs/notes/client-complete.md`.

use serde_json::{Map, Value, json};

use crate::{Accepts, ArgSpec, CompletionSource, StructField, StructSchema};

/// The one keyword this document uses that JSON Schema does not define.
///
/// # Invariant
///
/// Only what the standard cannot state lives here: the live source a value is
/// completed from, the sets keyed by model, the value placeholder and the
/// model-reference flag. The standard says an unknown keyword is an annotation, so
/// a reader that knows no `x-` keyword reads this document as a plain schema.
pub const EXTENSION: &str = "x-completion";

/// The arguments of one command, as the schema of the object they are written as.
pub fn of(args: &[ArgSpec]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in args {
        properties.insert(
            arg.name.clone(),
            property(Field {
                type_of: Some(&arg.type_of),
                description: &arg.description,
                object: arg.structure.as_ref(),
                accepts: arg.accepts.as_ref(),
                source: &arg.completions,
                hint: arg.value_hint.as_deref(),
                default: None,
                model_ref: false,
            }),
        );
        if arg.required {
            required.push(Value::String(arg.name.clone()));
        }
    }
    document(properties, required)
}

/// One field of a structured value, as a property of its object.
fn nested(schema: &StructSchema) -> Value {
    let mut properties = Map::new();
    for field in &schema.fields {
        properties.insert(field.name.clone(), of_field(field));
    }
    document(properties, Vec::new())
}

/// One [`StructField`] as a property.
fn of_field(field: &StructField) -> Value {
    property(Field {
        type_of: field.type_of.as_deref(),
        description: &field.description,
        object: field.object.as_deref(),
        accepts: field.accepts.as_ref(),
        source: &field.source,
        hint: field.value_hint.as_deref(),
        default: field.default.as_deref(),
        model_ref: field.model_ref,
    })
}

/// Everything one property is read from, whichever list states it.
struct Field<'a> {
    type_of: Option<&'a str>,
    description: &'a str,
    object: Option<&'a StructSchema>,
    accepts: Option<&'a Accepts>,
    source: &'a CompletionSource,
    hint: Option<&'a str>,
    default: Option<&'a str>,
    model_ref: bool,
}

/// One property: the standard keywords first, then what they cannot say.
fn property(field: Field<'_>) -> Value {
    let mut out = Map::new();
    match field.object {
        // A structured value is an object of its own fields.
        Some(schema) => {
            let inner = nested(schema);
            out.insert("type".into(), json!("object"));
            if let Some(properties) = inner.get("properties") {
                out.insert("properties".into(), properties.clone());
            }
        }
        None => match field.type_of.map(crate::argtype::schema_types) {
            Some(Ok(types)) if types.len() == 1 => {
                out.insert("type".into(), json!(types[0].to_string()));
            }
            Some(Ok(types)) => {
                out.insert(
                    "type".into(),
                    json!(types.iter().map(|t| t.to_string()).collect::<Vec<_>>()),
                );
            }
            // A spelling the table refuses states nothing; the property then
            // accepts any value, which is what an empty schema means.
            Some(Err(_)) | None => {}
        },
    }
    if !field.description.is_empty() {
        out.insert("description".into(), json!(field.description));
    }
    if let Some(value) = field.default {
        out.insert("default".into(), json!(value));
    }
    // A closed set of values is an `enum`, wherever it was stated.
    match (field.accepts, field.source) {
        (Some(Accepts::Same(values)), _) => {
            out.insert("enum".into(), json!(values));
        }
        (None, CompletionSource::Static(values)) => {
            out.insert("enum".into(), json!(values));
        }
        _ => {}
    }
    let mut extension = Map::new();
    if let Some(accepts @ Accepts::PerModel(_)) = field.accepts {
        extension.insert("accepts".into(), json!(accepts));
    }
    // Carried as the enum states it, so reading it back is the same serde pair.
    if !matches!(
        field.source,
        CompletionSource::None | CompletionSource::Static(_)
    ) {
        extension.insert("source".into(), json!(field.source));
    }
    if let Some(hint) = field.hint {
        extension.insert("hint".into(), json!(hint));
    }
    if field.model_ref {
        extension.insert("model_ref".into(), json!(true));
    }
    if !extension.is_empty() {
        out.insert(EXTENSION.into(), Value::Object(extension));
    }
    Value::Object(out)
}

/// An object schema over `properties`, closed.
fn document(properties: Map<String, Value>, required: Vec<Value>) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), Value::Object(properties));
    out.insert("additionalProperties".into(), json!(false));
    if !required.is_empty() {
        out.insert("required".into(), Value::Array(required));
    }
    Value::Object(out)
}
