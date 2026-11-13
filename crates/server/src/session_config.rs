//! SPEC's `SessionConfig`, the shape `new_session` and `set_session` take: flattened
//! into the flat settings.

use protocol::Args;
use serde::Deserialize;

/// SPEC `Model := {provider, model}`.
#[derive(Debug, Default, Deserialize)]
pub struct ModelRef {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// One model, as the two spellings the roster publishes it in.
///
/// # Invariant
///
/// SPEC's `Model := {provider, model}` and the flat `provider/model` name a
/// session's row and a sticky entry already hold are the same value; both are read
/// here so a client can hand over whichever it keeps. See `docs/notes/server-session-config.md`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ModelValue {
    Spelled(String),
    Ref(ModelRef),
}

/// SPEC `ModelConfig := {model, thinking, ctx_limit, cache_tier}`. Fields are optional:
/// a delta keeps what it omits.
#[derive(Debug, Default, Deserialize)]
pub struct ModelConfig {
    #[serde(default)]
    pub model: Option<ModelValue>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub ctx_limit: Option<serde_json::Value>,
    #[serde(default)]
    pub cache_tier: Option<String>,
}

/// SPEC `Workspace := {remote, cwd}`.
#[derive(Debug, Default, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// SPEC `SessionConfig := {model_config, workspace, keep_alive}`.
#[derive(Debug, Default, Deserialize)]
pub struct SessionConfig {
    #[serde(default)]
    pub model_config: Option<ModelConfig>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub keep_alive: Option<bool>,
}

/// Flatten a nested `SessionConfig` into the flat settings the handler reads.
///
/// # Invariant
///
/// Only a named field is written: a delta keeps every setting it does not name,
/// at every level, because an absent key never reaches the handler. A model is
/// spelled `<provider>/<model>` for the roster; `none` passes through for the
/// fields that take it (`remote`, `cache_tier`).
pub fn flatten(raw: &serde_json::Value, args: &mut Args) -> Result<(), String> {
    let config: SessionConfig = serde_json::from_value(raw.clone())
        .map_err(|e| format!("--config is not a SessionConfig object: {e}"))?;
    if let Some(model_config) = config.model_config {
        if let Some(model) = model_config.model {
            let spelled = match model {
                ModelValue::Spelled(text) => text,
                ModelValue::Ref(model) => match (model.provider, model.model) {
                    (Some(provider), Some(name)) => format!("{provider}/{name}"),
                    (None, Some(name)) => name,
                    (Some(provider), None) => {
                        return Err(format!(
                            "--config model names provider {provider:?} but no model"
                        ));
                    }
                    (None, None) => {
                        return Err("--config model is empty: name a model".to_string());
                    }
                },
            };
            args.insert("model".to_string(), spelled);
        }
        if let Some(thinking) = model_config.thinking {
            args.insert("thinking".to_string(), thinking);
        }
        if let Some(ctx_limit) = model_config.ctx_limit {
            let percent = match ctx_limit {
                serde_json::Value::String(text) => text,
                serde_json::Value::Number(number) => number.to_string(),
                other => {
                    return Err(format!(
                        "--config ctx_limit takes a percentage like \"70%\", got {other}"
                    ));
                }
            };
            args.insert("ctx_limit".to_string(), percent);
        }
        if let Some(cache_tier) = model_config.cache_tier {
            args.insert("cache_tier".to_string(), cache_tier);
        }
    }
    if let Some(workspace) = config.workspace {
        if let Some(remote) = workspace.remote {
            args.insert("remote".to_string(), remote);
        }
        if let Some(cwd) = workspace.cwd {
            args.insert("cwd".to_string(), cwd);
        }
    }
    if let Some(keep_alive) = config.keep_alive {
        args.insert(
            "keep_alive".to_string(),
            if keep_alive { "true" } else { "false" }.to_string(),
        );
    }
    Ok(())
}

/// One field SPEC gives a default, spelled as SPEC spells it.
///
/// # Invariant
///
/// Every default SPEC states is marked here, and a marked field is exactly what a
/// sticky entry may not fill: a value the type already decides is not a missing
/// argument. See `docs/notes/server-session-config.md`.
fn defaulted(field: protocol::StructField, spelling: &str) -> protocol::StructField {
    protocol::StructField {
        default: Some(spelling.to_string()),
        ..field
    }
}

/// The `SessionConfig` shape a client completes inside `--config`.
///
/// # Invariant
///
/// Field names, the values each leaf offers, and the default each one has: what a
/// fill may not stand in for. See `docs/notes/server-session-config.md`.
pub fn schema(remotes: &[String]) -> protocol::StructSchema {
    use protocol::{CompletionSource, StructField, StructSchema};
    let leaf = |name: &str, description: &str, source: CompletionSource, hint: &str| StructField {
        name: name.to_string(),
        description: description.to_string(),
        object: None,
        source,
        accepts: None,
        value_hint: Some(hint.to_string()),
        model_ref: false,
        default: None,
    };
    let object = |name: &str, description: &str, fields: Vec<StructField>| StructField {
        name: name.to_string(),
        description: description.to_string(),
        object: Some(Box::new(StructSchema { fields })),
        source: CompletionSource::None,
        accepts: None,
        value_hint: None,
        model_ref: false,
        default: None,
    };
    let mut remote_values = vec![protocol::NONE_VALUE.to_string()];
    remote_values.extend(remotes.iter().cloned());
    // SPEC: `model: Model` -- no default, so an entry may fill it.
    let model = StructField {
        model_ref: true,
        ..leaf(
            "model",
            "The model this session runs on.",
            CompletionSource::Models,
            "provider/model",
        )
    };
    // SPEC: `Thinking = provider_default()`, re-defaulted when the model changes.
    let thinking = defaulted(
        StructField {
            accepts: crate::providers::level_accepts(),
            ..leaf(
                "thinking",
                "How hard the model thinks.",
                CompletionSource::None,
                "level",
            )
        },
        "provider_default()",
    );
    // SPEC: `ctx_limit = 70%`.
    let ctx_limit = defaulted(
        leaf(
            "ctx_limit",
            "The context level for a Context notice.",
            CompletionSource::Static(
                [
                    "0", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ),
            "n%",
        ),
        "70%",
    );
    // SPEC: `cache_tier = provider_default()`.
    let cache_tier = defaulted(
        StructField {
            accepts: crate::providers::tier_accepts(),
            ..leaf(
                "cache_tier",
                "The cache tier every request sends.",
                CompletionSource::None,
                "name",
            )
        },
        "provider_default()",
    );
    // SPEC: `remote: optional[Remote] = none`.
    let remote = defaulted(
        leaf(
            "remote",
            "The machine this session's shells run on; none is this machine.",
            CompletionSource::Static(remote_values),
            "name",
        ),
        "none",
    );
    // SPEC: `cwd: path = "."`.
    let cwd = defaulted(
        leaf(
            "cwd",
            "The session's directory.",
            CompletionSource::Paths,
            "path",
        ),
        ".",
    );
    // SPEC: `keep_alive: bool` -- no default, so an entry may fill it.
    let keep_alive = leaf(
        "keep_alive",
        "Whether a periodic notice keeps the prefix warm.",
        CompletionSource::Static(vec!["true".to_string(), "false".to_string()]),
        "true|false",
    );
    StructSchema {
        fields: vec![
            object(
                "model_config",
                "The model and how it is asked.",
                vec![model, thinking, ctx_limit, cache_tier],
            ),
            object("workspace", "Where the session works.", vec![remote, cwd]),
            keep_alive,
        ],
    }
}
