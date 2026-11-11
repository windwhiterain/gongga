//! SPEC's `SessionConfig`: the nested shape `new_session` / `set_session` take.
//! See `SPEC.md` (`## Server Cmd`). The wire carries it as one JSON argument;
//! this flattens it into the flat settings the session handler already reads, so
//! there is one place that applies a setting and not a second copy of it.

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

/// SPEC `ModelConfig := {model, thinking, ctx_limit, cache_tier}`. Every field is
/// optional so a `set_session` delta keeps what it does not name.
#[derive(Debug, Default, Deserialize)]
pub struct ModelConfig {
    #[serde(default)]
    pub model: Option<ModelRef>,
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
pub fn flatten(raw: &str, args: &mut Args) -> Result<(), String> {
    let config: SessionConfig = serde_json::from_str(raw)
        .map_err(|e| format!("--config is not a SessionConfig object: {e}"))?;
    if let Some(model_config) = config.model_config {
        if let Some(model) = model_config.model {
            let spelled = match (model.provider, model.model) {
                (Some(provider), Some(name)) => format!("{provider}/{name}"),
                (None, Some(name)) => name,
                (Some(provider), None) => {
                    return Err(format!(
                        "--config model names provider {provider:?} but no model"
                    ));
                }
                (None, None) => return Err("--config model is empty: name a model".to_string()),
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
