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

/// SPEC `Workspace := {cwd, worktree}`.
///
/// # Invariant
///
/// One path says both where a session works and which machine it works on: a
/// `remote://<name>/<path>` `cwd` is that peer, a bare one is the machine it already
/// records. There is no second field to disagree with the first.
#[derive(Debug, Default, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
}

/// SPEC `LiveCompact := {enabled, interval_tokens, mode, max_unmasked_blocks}`.
#[derive(Debug, Default, Deserialize)]
pub struct LiveCompact {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub interval_tokens: Option<u64>,
    #[serde(default)]
    pub mode: Option<crate::live_compact::CompactMode>,
    #[serde(default)]
    pub max_unmasked_blocks: Option<u64>,
}

/// SPEC `SessionConfig := {model_config, workspace, keep_alive, live_compact}`.
#[derive(Debug, Default, Deserialize)]
pub struct SessionConfig {
    #[serde(default)]
    pub model_config: Option<ModelConfig>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub keep_alive: Option<bool>,
    #[serde(default)]
    pub live_compact: Option<LiveCompact>,
}

/// Flatten a nested `SessionConfig` into the flat settings the handler reads.
///
/// # Invariant
///
/// Only a named field is written: a delta keeps every setting it does not name,
/// at every level, because an absent key never reaches the handler. A model is
/// spelled `<provider>/<model>` for the roster; `none` passes through for the
/// field that takes it (`cache_tier`).
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
        if let Some(cwd) = workspace.cwd {
            args.insert("cwd".to_string(), cwd);
        }
        if let Some(worktree) = workspace.worktree {
            args.insert("worktree".to_string(), worktree);
        }
    }
    if let Some(keep_alive) = config.keep_alive {
        args.insert(
            "keep_alive".to_string(),
            if keep_alive { "true" } else { "false" }.to_string(),
        );
    }
    if let Some(live_compact) = config.live_compact {
        if let Some(enabled) = live_compact.enabled {
            args.insert(
                "live_compact_enabled".to_string(),
                if enabled { "true" } else { "false" }.to_string(),
            );
        }
        if let Some(tokens) = live_compact.interval_tokens {
            args.insert(
                "live_compact_interval_tokens".to_string(),
                tokens.to_string(),
            );
        }
        if let Some(mode) = live_compact.mode {
            args.insert("live_compact_mode".to_string(), mode.as_str().to_string());
        }
        if let Some(maximum) = live_compact.max_unmasked_blocks {
            args.insert(
                "memory_max_unmasked_blocks".to_string(),
                maximum.to_string(),
            );
        }
    }
    Ok(())
}

/// One property of the `SessionConfig` document, as this file states it.
#[derive(Default)]
struct Field<'a> {
    description: &'a str,
    /// SPEC's type: from it a client writes `true` and quotes a string.
    spelling: &'a str,
    hint: Option<&'a str>,
    source: Option<protocol::CompletionSource>,
    accepts: Option<protocol::Accepts>,
    kind: Option<&'a str>,
    default: Option<&'a str>,
    model_ref: bool,
}

impl Field<'_> {
    fn build(&self) -> serde_json::Value {
        let source = self.source.clone().unwrap_or_default();
        protocol::argschema::property(&protocol::argschema::Row {
            type_of: Some(self.spelling),
            description: self.description,
            object: None,
            accepts: self.accepts.as_ref(),
            source: &source,
            kind: self.kind,
            hint: self.hint,
            default: self.default,
            model_ref: self.model_ref,
            update: false,
            schema_of: None,
            schemas: None,
        })
    }
}

/// One nested object of the document.
fn object(
    description: &str,
    spelling: &str,
    kind: Option<&str>,
    fields: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for (name, value) in fields {
        properties.insert(name.to_string(), value);
    }
    protocol::argschema::property(&protocol::argschema::Row {
        type_of: Some(spelling),
        description,
        object: Some(protocol::argschema::object(properties)),
        accepts: None,
        source: &protocol::CompletionSource::None,
        kind,
        hint: None,
        default: None,
        model_ref: false,
        update: false,
        schema_of: None,
        schemas: None,
    })
}

/// The `SessionConfig` shape a client completes inside `--config`.
///
/// # Invariant
///
/// Field names, the values each leaf offers, and the default each one has: what a
/// fill may not stand in for. Every default SPEC states is marked here, and a marked
/// field is exactly what a sticky entry may not fill. See
/// `docs/notes/server-session-config.md`.
pub fn schema(remotes: &[String]) -> serde_json::Value {
    use protocol::CompletionSource;
    use serde_json::Value;

    // The machines this server reaches, offered where a path may name one.
    let _ = remotes;
    // SPEC: `model: Model` -- no default, so an entry may fill it.
    // SPEC: `Model := {provider, model} # sticky`.
    let model = Field {
        description: "The model this session runs on.",
        spelling: "Model",
        hint: Some("provider/model"),
        source: Some(CompletionSource::Models),
        kind: Some("model"),
        model_ref: true,
        ..Default::default()
    };
    // SPEC: `Thinking = provider_default()`, re-defaulted when the model changes.
    let thinking = Field {
        description: "How hard the model thinks.",
        spelling: "Thinking",
        hint: Some("level"),
        accepts: crate::providers::level_accepts(),
        default: Some("provider_default()"),
        ..Default::default()
    };
    // SPEC: `ctx_limit = 70%`.
    let ctx_limit = Field {
        description: "The context level for a Context notice.",
        spelling: "str",
        hint: Some("n%"),
        source: Some(CompletionSource::Static(
            [
                "0", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        )),
        default: Some("70%"),
        ..Default::default()
    };
    // SPEC: `cache_tier = provider_default()`.
    let cache_tier = Field {
        description: "The cache tier every request sends.",
        spelling: "str",
        hint: Some("name"),
        accepts: crate::providers::tier_accepts(),
        default: Some("provider_default()"),
        ..Default::default()
    };
    // SPEC: `cwd: path = "."`; its `remote://<name>/<path>` spelling moves the session.
    let cwd = Field {
        description: "The session's directory. A `remote://<name>/<absolute path>` puts the \
                      session on that machine; a bare or `remote://local/<path>` one is this one.",
        spelling: "path",
        hint: Some("path"),
        source: Some(CompletionSource::Paths),
        default: Some("."),
        ..Default::default()
    };
    // SPEC: `worktree: optional[str] = none`.
    let worktree = Field {
        description: "A branch and worktree made with this session, in the repository its cwd sits in.",
        spelling: "optional[str]",
        hint: Some("name"),
        default: Some("none"),
        ..Default::default()
    };
    // SPEC: `keep_alive: bool = false`.
    let keep_alive = Field {
        description: "Whether a periodic notice keeps the prefix warm.",
        spelling: "bool",
        hint: Some("true|false"),
        source: Some(CompletionSource::Static(vec![
            "true".to_string(),
            "false".to_string(),
        ])),
        default: Some("false"),
        ..Default::default()
    };
    let live_compact_enabled = Field {
        description: "Whether the server asks this session to fold its own context.",
        spelling: "bool",
        hint: Some("true|false"),
        source: Some(CompletionSource::Static(vec![
            "true".to_string(),
            "false".to_string(),
        ])),
        ..Default::default()
    };
    let live_compact_interval = Field {
        description: "Input-token growth between this session's checkpoints.",
        spelling: "int",
        hint: Some("tokens"),
        ..Default::default()
    };
    let live_compact_mode = Field {
        description: "Whether checkpoints cut the transcript projection or declare a memory-mask view.",
        spelling: "str",
        hint: Some("cut|mask"),
        source: Some(CompletionSource::Static(vec![
            "cut".to_string(),
            "mask".to_string(),
        ])),
        ..Default::default()
    };
    let max_unmasked_blocks = Field {
        description: "Maximum original memory spans visible at once in mask mode.",
        spelling: "int",
        hint: Some("blocks"),
        ..Default::default()
    };
    let mut fields: Vec<(&str, Value)> = Vec::new();
    // SPEC: `ModelConfig := {model, thinking, ctx_limit, cache_tier} # sticky`.
    fields.push((
        "model_config",
        object(
            "The model and how it is asked.",
            "ModelConfig",
            Some("model_config"),
            vec![
                ("model", model.build()),
                ("thinking", thinking.build()),
                ("ctx_limit", ctx_limit.build()),
                ("cache_tier", cache_tier.build()),
            ],
        ),
    ));
    fields.push((
        "workspace",
        object(
            "Where the session works.",
            "Workspace",
            None,
            vec![("cwd", cwd.build()), ("worktree", worktree.build())],
        ),
    ));
    fields.push(("keep_alive", keep_alive.build()));
    fields.push((
        "live_compact",
        object(
            "Whether the server asks this session for checkpoint summaries.",
            "LiveCompact",
            None,
            vec![
                ("enabled", live_compact_enabled.build()),
                ("interval_tokens", live_compact_interval.build()),
                ("mode", live_compact_mode.build()),
                ("max_unmasked_blocks", max_unmasked_blocks.build()),
            ],
        ),
    ));
    object(
        "The session's settings as SPEC spells them.",
        "SessionConfig",
        None,
        fields,
    )
}
