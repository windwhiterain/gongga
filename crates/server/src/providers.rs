//! The machine's LLM roster: the community `providers.toml`, under the user's own.
//!
//! See `docs/notes/server-providers.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// Everything `providers.toml` says about one provider.
#[derive(Debug, Clone)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<Endpoint>,
    pub models: Vec<Model>,
    /// How long this provider holds a cached prefix, in seconds, when the roster
    /// states it.
    ///
    /// # Invariant
    ///
    /// `None` is "not stated" and means the plan assumes a prefix held forever: a
    /// lifetime nobody stated is not one to invent. See
    /// `docs/notes/server-prefix-cache.md`.
    pub cache_ttl_secs: Option<u64>,
    /// Tiers this provider accepts on the wire, by the endpoint's spelling.
    pub cache_ttl_tiers: HashMap<String, u64>,
    /// Whether this provider documents a field that keeps requests sharing a prefix on
    /// one node (`prompt_cache_key`).
    ///
    /// # Invariant
    ///
    /// `None` is "this provider says nothing about it", and then nothing is sent: a
    /// field nobody documents buys nothing, and an endpoint that rejects an unknown one
    /// fails the request. It is the roster's fact, not the home's.
    pub cache_affinity: Option<bool>,
    /// Whether this provider documents a header that pins a request to one inference
    /// instance (`X-Session-ID`).
    ///
    /// # Invariant
    ///
    /// The same rule as `cache_affinity`, and a separate fact: a body field and a
    /// header are two spellings, and a provider may document one of them. See
    /// `docs/notes/server-config.md`.
    pub cache_session_header: Option<bool>,
    /// The face this provider is worth being spoken in, by its endpoint's label.
    ///
    /// # Invariant
    ///
    /// A preference, not a capability: the endpoints list says what the provider
    /// offers, and this says which one this machine sends to. Absent keeps the
    /// built-in order. See `docs/notes/server-anthropic.md`.
    pub api: Option<String>,
    // No `key` here: a roster is a git checkout, and a key in one is a key in
    // git. Keys come from `~/.agents/keys.toml`.
    /// The variable that should hold the key instead, if one is named.
    key_env: Option<String>,
    /// How to read what is left, in the roster's own order of preference.
    quota: Vec<QuotaQuery>,
}

/// One `[[provider.quota_query]]`: how this machine reads what is left at a provider.
///
/// # Invariant
///
/// Only a query that names a `script` can be read: the roster's other fields
/// (`url`, `jsonpath`, ...) describe the endpoint for a human, and this server
/// does not speak them. See `docs/notes/server-providers.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaQuery {
    pub id: String,
    /// What the reading's numbers mean: `percent-left`, `cny-left`, `usd-left`, ...
    pub unit: String,
    /// The script, resolved against the layer that wrote it when the file was read.
    pub script: Option<PathBuf>,
    /// The variable that script reads the key from.
    pub script_env: Option<String>,
}

/// A provider's quota queries, and the key they are to be read with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaAccess {
    pub provider: String,
    pub queries: Vec<QuotaQuery>,
    /// The same credential a request would use, which is what the script expects.
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The provider's own protocol label: `openai-completions`,
    /// `openai-responses`, `anthropic-messages`, ...
    pub api: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Input+output ceiling when published; a `--context_limit` percent is of it.
    pub context: Option<u64>,
    /// Request paths this model accepts; empty means unstated, assume OpenAI.
    pub endpoints: Vec<String>,
    /// The longest answer this model gives, when the roster states it.
    ///
    /// # Invariant
    ///
    /// Required by the anthropic-messages face, which asks for it in every request:
    /// `None` there is a refusal that names this field rather than a guessed length.
    pub max_output_tokens: Option<u32>,
    /// Levels this model accepts, as the roster spells them; empty passes through.
    pub reasoning_levels: Vec<String>,
    /// The level the roster says to use when nobody asked for one.
    pub reasoning_default: Option<String>,
    /// How a level spelled by a caller maps onto one this model accepts.
    pub reasoning_mapping: HashMap<String, String>,
    /// What this model can take in and give back: `text`, `image`, ...
    pub modalities: Vec<String>,
    /// Tools the provider runs on its own side, in the provider's own spelling.
    pub builtin_tools: Vec<String>,
    /// What this model charges, when the roster says. See [`Price`].
    #[serde(default)]
    pub price: Option<Price>,
    /// How the roster rates this model: `flash`, `pro`, `sota`, ...
    ///
    /// # Invariant
    ///
    /// Kept as written, never mapped to a ranking of this server's: which tier a
    /// model belongs to is the roster author's judgement, and a server that
    /// re-derived it would disagree with the file it read.
    #[serde(default)]
    pub tier: Option<String>,
}

/// A model's prices, in whatever currency and unit the roster records them.
///
/// # Invariant
///
/// Only the ratio is used: `input / cache_read` is what a cached token is worth, and
/// it is the same number whether the table says 1.00/0.10 CNY per million or
/// 0.15/0.015 USD. So no currency is ever converted and no unit is ever assumed.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
pub struct Price {
    #[serde(default)]
    pub input: Option<f64>,
    /// The roster spells the cached rate `cache_read`; `cached` is the same thing.
    #[serde(default, alias = "cached")]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
}

impl Price {
    /// How many fresh tokens one cached token stands for.
    ///
    /// # Invariant
    ///
    /// `1.0` when the roster states no usable pair, which the cache's plan reads as
    /// "no rate here" and replaces with its own stand-in.
    pub fn miss_ratio(&self) -> f64 {
        match (self.input, self.cache_read) {
            (Some(input), Some(cached)) if input > 0.0 && cached > 0.0 => input / cached,
            _ => 1.0,
        }
    }
}

impl Model {
    /// Whether this model can be driven through the Responses API, which is the
    /// only shape this server speaks.
    ///
    /// # Invariant
    ///
    /// An empty list is unstated, and unstated admits everything: the roster
    /// having no opinion is not the roster saying no.
    pub(crate) fn speaks_responses(&self) -> bool {
        self.endpoints.is_empty()
            || self
                .endpoints
                .iter()
                .any(|e| e.starts_with("openai-") || e.starts_with("anthropic-"))
    }
}

/// One model as a caller picks it: which provider serves it, and where to ask.
///
/// See `docs/notes/server-providers.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The provider's id, e.g. `deepseek-official`.
    pub provider: String,
    /// The provider's display name.
    pub provider_name: String,
    /// The model a caller names: `provider/model_name`.
    pub model: String,
    /// The name the provider's API takes in its `model` field.
    pub model_name: String,
    /// The model's display name.
    pub model_title: String,
    pub base_url: String,
    /// Variable the provider says its key lives in, when it says.
    pub api_key_env: Option<String>,
    /// The key as written in the file; `None` is normal when a var is named.
    pub key: Option<String>,
    pub context: Option<u64>,
    /// The roster's reasoning levels, in its own spelling, for validating a level.
    pub reasoning_levels: Vec<String>,
    pub reasoning_default: Option<String>,
    pub reasoning_mapping: HashMap<String, String>,
    /// What this model can take in and give back: `text`, `image`, ...
    pub modalities: Vec<String>,
    /// Tools the provider runs on its own side, in the provider's own spelling.
    pub builtin_tools: Vec<String>,
    /// What the roster says this model charges, for the prefix cache's arithmetic.
    pub price: Option<Price>,
    /// How long a cached prefix lives in the tier a request without a cache mode
    /// lands in; `None` if it states none.
    pub cache_ttl_secs: Option<u64>,
    /// Tiers this provider accepts on the wire; empty means none selectable.
    pub cache_ttl_tiers: HashMap<String, u64>,
    /// The roster's tier for this model, as written.
    pub tier: Option<String>,
    /// Whether the provider documents `prompt_cache_key`; `None` sends nothing.
    pub cache_affinity: Option<bool>,
    /// Which face this entry is spoken in, by the endpoint's own label.
    pub api: String,
    /// The longest answer this model gives, when the roster states it.
    pub max_output_tokens: Option<u32>,
    /// Whether the provider documents `X-Session-ID`; `None` sends nothing.
    pub cache_session_header: Option<bool>,
}

impl Entry {
    /// The level to send for `requested`, or why there is none.
    ///
    /// # Invariant
    ///
    /// A returned level is always one the roster lists for this model, mapped
    /// through `reasoning_mapping`; otherwise the error names the levels that
    /// would work. See `docs/notes/server-providers.md`.
    pub fn reasoning_effort(&self, requested: &str) -> Result<String, String> {
        if self.reasoning_levels.is_empty() {
            return Ok(requested.to_string());
        }
        let mapped = self
            .reasoning_mapping
            .get(requested)
            .cloned()
            .unwrap_or_else(|| requested.to_string());
        if self.reasoning_levels.contains(&mapped) {
            return Ok(mapped);
        }
        Err(format!(
            "`{requested}` is not a level this model takes; it accepts {}",
            self.reasoning_levels.join(", ")
        ))
    }

    /// The level this model runs at when the command naming it says none.
    ///
    /// # Invariant
    ///
    /// `stored` while this model publishes it, else the level the roster records as
    /// this model's own default, else the highest it publishes -- and always a level
    /// this model publishes, so a stored level is always one a request can carry.
    /// See `docs/notes/server-providers.md`.
    pub fn level_in_force(&self, stored: Option<&str>) -> Option<String> {
        let published = |level: &str| self.reasoning_levels.iter().any(|known| known == level);
        if let Some(level) = stored.filter(|level| published(level)) {
            return Some(level.to_string());
        }
        self.stated_default().or_else(|| self.highest_published())
    }

    /// The roster's default for this model, spelled the way this model publishes it.
    ///
    /// # Invariant
    ///
    /// The default travels through [`Self::reasoning_mapping`] like any other level,
    /// so a roster that states it in a caller's spelling (`medium`) and publishes
    /// another (`high`) still answers with one the request can carry.
    fn stated_default(&self) -> Option<String> {
        let default = self.reasoning_default.as_deref()?;
        let mapped = self
            .reasoning_mapping
            .get(default)
            .map_or(default, String::as_str);
        self.reasoning_levels
            .iter()
            .find(|known| known.as_str() == mapped)
            .cloned()
    }

    /// The hardest level this model publishes, when [`LEVELS`] can rank any of them.
    fn highest_published(&self) -> Option<String> {
        self.reasoning_levels
            .iter()
            .filter_map(|level| Some((rank(level)?, level)))
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, level)| level.clone())
    }
}

/// How hard an agent is being asked to think, weakest first.
///
/// # Invariant
///
/// The one order this server knows, so `highest in supported` and the completion
/// list cannot disagree. A roster entry is free to publish a spelling of its own
/// (`on`, `off`): [`rank`] then answers `None` rather than inventing a position,
/// and nothing is capped on that model's behalf.
pub const LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Where `level` sits in [`LEVELS`], or `None` for a spelling that is not a rung.
pub fn rank(level: &str) -> Option<usize> {
    LEVELS.iter().position(|known| *known == level)
}

/// An [`Entry`] plus the key that resolved for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub entry: Entry,
    pub api_key: String,
    /// Where `api_key` came from when it came from the environment, so a
    /// failure can name the variable instead of its value.
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    /// The files this list was assembled from, lowest precedence first.
    paths: Vec<PathBuf>,
    /// The keys this machine keeps out of the roster, by provider id.
    keys: std::collections::HashMap<String, String>,
    providers: Vec<Provider>,
    /// Why a file could not be read, kept so startup can report it once and go on.
    problem: Option<String>,
    /// Which read produced this list; see [`roster`].
    generation: u64,
}

impl Catalog {
    /// Reads the one roster file this machine has.
    ///
    /// # Invariant
    ///
    /// One file, not a stack of overlays: a machine's own entries are a branch of
    /// the repository the file lives in (`local`), so an upstream change is a
    /// rebase and there is nothing here to merge. See
    /// `docs/notes/server-providers.md`.
    ///
    /// A missing file is not a problem worth a line; an unreadable one is, and it
    /// is the only thing that keeps `--model` from answering.
    pub fn load_file(path: &Path) -> Self {
        let mut catalog = Self {
            paths: vec![path.to_path_buf()],
            keys: std::collections::HashMap::new(),
            providers: Vec::new(),
            problem: None,
            generation: 0,
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<toml::Value>(&text) {
                Ok(mut value) => {
                    resolve_scripts(&mut value, path.parent());
                    match parse_value(value) {
                        Ok(providers) => catalog.providers = providers,
                        Err(e) => catalog.problem = Some(format!("{}: {e}", path.display())),
                    }
                }
                Err(e) => catalog.problem = Some(format!("{}: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => catalog.problem = Some(format!("{}: {e}", path.display())),
        }
        catalog
    }

    /// An empty roster: no providers to name, and no complaint to make.
    pub fn empty() -> Self {
        Self {
            paths: Vec::new(),
            keys: std::collections::HashMap::new(),
            providers: Vec::new(),
            problem: None,
            generation: 0,
        }
    }

    /// This list, with `problem` replaced by why the file no longer parses.
    ///
    /// # Invariant
    ///
    /// The generation is kept. Serving this list is serving the same providers a
    /// client was already built against, so a failed read must not retire
    /// anything: the cost of a broken edit is a stale roster, not a dead session.
    fn blaming(&self, problem: String) -> Self {
        let mut copy = self.clone();
        copy.problem = Some(problem);
        copy
    }

    /// The files this list came from, which is what a reader has to edit.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Which read produced this list. A cached client on an older number talks
    /// to a list that no longer exists.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    /// Every model a request could actually be sent on, as `provider/model_name`,
    /// in file order.
    ///
    /// # Invariant
    ///
    /// A provider whose credential resolves takes part; one that names a key this
    /// machine does not have is left out, because offering it would offer a model
    /// on which every request fails. "Resolves" means the keystore first, then
    /// the variable. See `docs/notes/server-providers.md`.
    pub fn models(&self) -> Vec<String> {
        self.models_with_kept(&lookup_env, &|id| self.keys.get(id).cloned())
    }

    /// Both halves of the credential, so a caller decides which sources it means.
    fn models_with_kept(
        &self,
        lookup: &dyn Fn(&str) -> Option<String>,
        kept: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for provider in &self.providers {
            let in_keystore = kept(&provider.id);
            if credential(provider, lookup, in_keystore.as_ref()).is_none() {
                continue;
            }
            for model in &provider.models {
                out.push(format!("{}/{}", provider.id, model.id));
            }
        }
        out
    }

    /// Resolves a model into a destination plus the key to use for it.
    pub fn resolve(&self, model: &str) -> Result<Choice, String> {
        self.resolve_with(model, &lookup_env)
    }

    /// Resolves the destination only. The caller brings its own key.
    pub fn entry(&self, model: &str) -> Result<Entry, String> {
        self.entry_with(model)
    }

    /// A model however it was written: with its provider, or as a bare name.
    pub fn place(&self, model: &str) -> Option<Entry> {
        self.entry(model).ok().or_else(|| self.entry_by_name(model))
    }

    /// Whether a request for `model` is one this server could make.
    ///
    /// # Invariant
    ///
    /// The same test [`Self::default_entry`] applies, asked about one named model:
    /// the roster places it and it takes the Responses API. A model the roster
    /// knows and this shape cannot drive would be offered and then fail.
    pub fn speaks_responses(&self, model: &str) -> bool {
        let Some((provider_id, model_name)) = model.split_once('/') else {
            return false;
        };
        self.providers
            .iter()
            .filter(|p| p.id == provider_id)
            .flat_map(|p| p.models.iter())
            .find(|m| m.id == model_name)
            .is_some_and(Model::speaks_responses)
    }

    /// A bare model name, looked for in every provider in file order.
    pub fn resolve_by_name(&self, model_name: &str) -> Option<Choice> {
        self.entry_by_name(model_name)
            .and_then(|e| self.resolve_with(&e.model, &lookup_env).ok())
    }

    /// The same search as [`Self::resolve_by_name`], without requiring a key.
    pub fn entry_by_name(&self, model_name: &str) -> Option<Entry> {
        self.providers
            .iter()
            .find(|p| p.models.iter().any(|m| m.id == model_name))
            .and_then(|p| self.entry_with(&format!("{}/{}", p.id, model_name)).ok())
    }

    /// The first model of `provider`, for a provider configured without a model.
    pub fn first_entry_of(&self, provider_id: &str) -> Option<Entry> {
        let provider = self.providers.iter().find(|p| p.id == provider_id)?;
        let model = provider.models.first()?;
        self.entry_with(&format!("{}/{}", provider.id, model.id))
            .ok()
    }

    /// The entry a server with nothing configured should use: the first one that
    /// speaks Responses.
    ///
    /// # Invariant
    ///
    /// File order is the preference order: the scan only skips what cannot work,
    /// never re-ranks what is left. See `docs/notes/server-providers.md`.
    pub fn default_entry(&self) -> Option<Entry> {
        for p in &self.providers {
            for m in &p.models {
                if !m.speaks_responses() {
                    continue;
                }
                if let Ok(entry) = self.entry_with(&format!("{}/{}", p.id, m.id)) {
                    return Some(entry);
                }
            }
        }
        None
    }

    /// A provider's allowance queries, and the key a request there would use.
    pub fn quota_of(&self, provider_id: &str) -> Option<QuotaAccess> {
        let provider = self.providers.iter().find(|p| p.id == provider_id)?;
        let kept = self.keys.get(&provider.id);
        let key = credential(provider, &lookup_env, kept).map(|(key, _)| key);
        Some(QuotaAccess {
            provider: provider.id.clone(),
            queries: provider.quota.clone(),
            key,
        })
    }

    fn resolve_with(
        &self,
        model: &str,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Choice, String> {
        let entry = self.entry_with(model)?;
        let provider = self
            .providers
            .iter()
            .find(|p| p.id == entry.provider)
            .ok_or_else(|| format!("no provider {:?}", entry.provider))?;
        let (api_key, api_key_env) = credential(provider, lookup, self.keys.get(&provider.id))
            .ok_or_else(|| match &provider.key_env {
                Some(env) => format!(
                    "no key for provider {}: set {env} (the roster has no literal key either)",
                    provider.id
                ),
                None => format!(
                    "no key for provider {}: the roster names no key and no key_env",
                    provider.id
                ),
            })?;
        Ok(Choice {
            entry,
            api_key,
            api_key_env,
        })
    }

    /// Splits at the first `/`: a model name may itself contain one.
    fn entry_with(&self, model: &str) -> Result<Entry, String> {
        let (provider_id, model_name) = model
            .split_once('/')
            .ok_or_else(|| format!("expected <provider>/<model name>, got {model:?}"))?;
        let provider = self
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| {
                let mut known: Vec<&str> = self.providers.iter().map(|p| p.id.as_str()).collect();
                known.sort_unstable();
                if known.is_empty() {
                    format!("no provider {provider_id:?}: the roster has no providers")
                } else {
                    format!("no provider {provider_id:?}; known: {}", known.join(", "))
                }
            })?;
        let found = provider
            .models
            .iter()
            .find(|m| m.id == model_name)
            .ok_or_else(|| {
                let mut known: Vec<&str> = provider.models.iter().map(|m| m.id.as_str()).collect();
                known.sort_unstable();
                format!(
                    "provider {} has no model {model_name:?}; known: {}",
                    provider.id,
                    known.join(", ")
                )
            })?;
        let endpoint = endpoint_of(provider)
            .ok_or_else(|| format!("provider {} has no usable endpoint", provider.id))?;
        Ok(Entry {
            provider: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: format!("{}/{}", provider.id, found.id),
            model_name: found.id.clone(),
            model_title: found.name.clone(),
            base_url: endpoint.base_url.trim_end_matches('/').to_string(),
            api_key_env: provider.key_env.clone(),
            // The keystore's answer, not a roster field: what the server falls
            // back to for its own model.
            key: self.keys.get(&provider.id).cloned(),
            context: found.context,
            reasoning_levels: found.reasoning_levels.clone(),
            reasoning_default: found.reasoning_default.clone(),
            reasoning_mapping: found.reasoning_mapping.clone(),
            modalities: found.modalities.clone(),
            builtin_tools: found.builtin_tools.clone(),
            price: found.price,
            cache_ttl_secs: provider.cache_ttl_secs,
            cache_ttl_tiers: provider.cache_ttl_tiers.clone(),
            cache_affinity: provider.cache_affinity,
            cache_session_header: provider.cache_session_header,
            api: endpoint.api.clone(),
            max_output_tokens: found.max_output_tokens,
            tier: found.tier.clone(),
        })
    }
}

fn lookup_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The endpoint to talk to: the one this provider names, else `openai-responses`
/// beating the other flavours.
///
/// # Invariant
///
/// A named endpoint wins because the choice is a provider's fact -- which face has a
/// cache worth the request shape is not this server's to re-rank. A name the provider
/// does not offer falls through to the built-in order rather than failing: the roster
/// is read, not obeyed.
fn endpoint_of(provider: &Provider) -> Option<&Endpoint> {
    provider
        .api
        .as_deref()
        .and_then(|api| {
            provider
                .endpoints
                .iter()
                .find(|e| e.api == api && !e.base_url.is_empty())
        })
        .or_else(|| openai_endpoint(provider))
        .or_else(|| provider.endpoints.iter().find(|e| !e.base_url.is_empty()))
}

/// The endpoint for an OpenAI-shaped request; `openai-responses` beats the other
/// flavours.
fn openai_endpoint(provider: &Provider) -> Option<&Endpoint> {
    provider
        .endpoints
        .iter()
        .find(|e| e.api == "openai-responses")
        .or_else(|| {
            provider
                .endpoints
                .iter()
                .find(|e| e.api.starts_with("openai") && !e.base_url.is_empty())
        })
}

/// The key, and what to call it in a message: a provider's key, from the one
/// place a key may live.
///
/// # Invariant
///
/// Two sources, and the keystore wins: `~/.agents/keys.toml`, which git does
/// not track, and the variable `key_env` names.
///
/// A variable is copied into a process once, so a stale one would shadow
/// every edit to the file.
///
/// A `key` in the roster is **ignored**: the roster is a git checkout.
///
/// See `docs/notes/server-providers.md`.
fn credential(
    provider: &Provider,
    lookup: &dyn Fn(&str) -> Option<String>,
    kept: Option<&String>,
) -> Option<(String, Option<String>)> {
    if let Some(value) = kept.filter(|v| !v.is_empty()) {
        return Some((value.clone(), provider.key_env.clone()));
    }
    if let Some(name) = &provider.key_env {
        if let Some(value) = lookup(name).filter(|v| !v.is_empty()) {
            return Some((value, Some(name.clone())));
        }
    }
    None
}

/// How many tokens `n%` of a context window is.
///
/// # Invariant
///
/// A non-zero percentage never rounds down to zero: zero reads as compression
/// being off. See `docs/notes/server-providers.md`.
pub fn tokens_for_percent(context: u64, percent: u8) -> u64 {
    if percent == 0 {
        return 0;
    }
    // Rounding down to zero would read as "compression is off", which is the
    // opposite of what a real percentage asked for.
    (context.saturating_mul(percent as u64) / 100).max(1)
}

// -- parsing ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FileRoster {
    #[serde(default)]
    provider: Vec<FileProvider>,
}

#[derive(Debug, Deserialize)]
struct FileProvider {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    key_env: Option<String>,
    #[serde(default)]
    endpoint: Vec<FileEndpoint>,
    #[serde(default)]
    model: Vec<FileModel>,
    #[serde(default)]
    quota_query: Vec<FileQuota>,
    /// How long a cached prefix survives at this provider, in seconds.
    #[serde(default)]
    cache_ttl_secs: Option<u64>,
    /// Tiers that can be asked for on the wire, by the endpoint's spelling.
    #[serde(default)]
    cache_ttl_tiers: HashMap<String, u64>,
    /// Whether this provider documents `prompt_cache_key`.
    #[serde(default)]
    cache_affinity: Option<bool>,
    /// Whether this provider documents the `X-Session-ID` header.
    #[serde(default)]
    cache_session_header: Option<bool>,
    /// The endpoint label this provider is worth being spoken in.
    #[serde(default)]
    api: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileQuota {
    #[serde(default)]
    id: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    script: Option<PathBuf>,
    #[serde(default)]
    script_env: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileEndpoint {
    #[serde(default)]
    api: String,
    #[serde(default)]
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct FileModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    endpoints: Vec<String>,
    /// Levels this model accepts, as the roster spells them; empty means unstated.
    #[serde(default)]
    reasoning_levels: Vec<String>,
    #[serde(default)]
    reasoning_default: Option<String>,
    /// How a level this machine's callers spell maps onto what the model takes
    /// (`medium` -> `high`, `minimal` -> `low`, ...).
    #[serde(default)]
    reasoning_mapping: HashMap<String, String>,
    /// Input/output kinds the roster says this model handles; unstated is no opinion.
    #[serde(default)]
    modalities: Vec<String>,
    /// Provider-side tools, spelled that provider's way, sent verbatim; absent none.
    #[serde(default)]
    builtin_tools: Vec<String>,
    /// The price table the roster records for this model, in its own currency.
    #[serde(default)]
    price: Option<Price>,
    /// The roster's own rating of this model, `flash` / `pro` / `sota` / `test`.
    #[serde(default)]
    tier: Option<String>,
}

/// The providers a merged roster describes, first claim on an id winning.
fn parse_value(value: toml::Value) -> Result<Vec<Provider>, toml::de::Error> {
    let file: FileRoster = value.try_into()?;
    // Duplicate ids make `--model` ambiguous; keep the first, drop the rest.
    let mut seen: HashMap<String, ()> = HashMap::new();
    let providers = file
        .provider
        .into_iter()
        .filter(|p| seen.insert(p.id.clone(), ()).is_none())
        .map(|p| {
            let name = if p.name.is_empty() {
                p.id.clone()
            } else {
                p.name
            };
            Provider {
                endpoints: p
                    .endpoint
                    .into_iter()
                    .map(|e| Endpoint {
                        api: e.api,
                        base_url: e.base_url,
                    })
                    .collect(),
                models: p
                    .model
                    .into_iter()
                    .map(|m| Model {
                        name: if m.name.is_empty() {
                            m.id.clone()
                        } else {
                            m.name
                        },
                        id: m.id,
                        context: m.context,
                        max_output_tokens: m.max_output_tokens,
                        endpoints: m.endpoints,
                        reasoning_levels: m.reasoning_levels,
                        reasoning_default: m.reasoning_default,
                        reasoning_mapping: m.reasoning_mapping,
                        modalities: m.modalities,
                        builtin_tools: m.builtin_tools,
                        price: m.price,
                        tier: m.tier,
                    })
                    .collect(),
                quota: p
                    .quota_query
                    .into_iter()
                    .map(|q| QuotaQuery {
                        id: q.id,
                        unit: q.unit,
                        script: q.script,
                        script_env: q.script_env,
                    })
                    .collect(),
                id: p.id,
                name,
                cache_ttl_secs: p.cache_ttl_secs,
                cache_ttl_tiers: p.cache_ttl_tiers,
                cache_affinity: p.cache_affinity,
                cache_session_header: p.cache_session_header,
                api: p.api,
                key_env: p.key_env,
            }
        })
        .collect();
    Ok(providers)
}

/// Turns every `quota_query.script` of one layer into the path it names, from
/// that layer's own directory.
///
/// # Invariant
///
/// Done before the layers merge, because after it the file a path was written in
/// is no longer known -- and a private layer's `providers/x.py` is not the
/// community layer's. See `docs/notes/server-providers.md`.
fn resolve_scripts(value: &mut toml::Value, dir: Option<&Path>) {
    let Some(dir) = dir else {
        return;
    };
    let Some(providers) = value.get_mut("provider").and_then(|p| p.as_array_mut()) else {
        return;
    };
    for provider in providers {
        let Some(queries) = provider
            .get_mut("quota_query")
            .and_then(|q| q.as_array_mut())
        else {
            continue;
        };
        for query in queries {
            let Some(table) = query.as_table_mut() else {
                continue;
            };
            let script = table
                .get("script")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            if let Some(script) = script.filter(|s| !Path::new(s).is_absolute()) {
                table.insert(
                    "script".to_string(),
                    toml::Value::String(dir.join(script).to_string_lossy().to_string()),
                );
            }
        }
    }
}

/// The file state a roster was read from: enough to notice an edit without
/// hashing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<std::time::SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// The roster in force, and the files it came from.
struct Live {
    /// Whether a read happened: no roster file is still an answer, and asking
    /// twice must not re-read anything.
    read: bool,
    stamps: Vec<Option<Stamp>>,
    catalog: Arc<Catalog>,
    /// The last read that parsed: a broken edit leaves running sessions on the
    /// roster they already use.
    last_good: Option<Arc<Catalog>>,
}

/// A live view of the roster: every question asks the file first.
///
/// # Invariant
///
/// Nothing here holds a `Catalog`. Holding one would freeze the roster at the
/// moment the server started -- which is exactly what a reload is meant to undo
/// -- and the next `set_session --model` would refuse a model the user had just
/// written into the file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Handle;

impl Handle {
    pub fn resolve(&self, model: &str) -> Result<Choice, String> {
        roster().resolve(model)
    }

    pub fn resolve_by_name(&self, model_name: &str) -> Option<Choice> {
        roster().resolve_by_name(model_name)
    }

    pub fn place(&self, model: &str) -> Option<Entry> {
        roster().place(model)
    }

    pub fn generation(&self) -> u64 {
        roster().generation()
    }
}

/// Counts the reads, so a client built from a retired list is recognised as one.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A machine's keys, kept out of the roster because a roster is a git checkout.
///
/// # Invariant
///
/// `[keys]` maps a provider id to its key: `<id> = "sk-..."`. It is read on every
/// roster generation, so an edit hot-updates the credential without a restart.
///
/// It is never committed: `~/.agents/` is not a repository, and the file is
/// created `0600` where the platform honours that.
fn keys_path() -> PathBuf {
    crate::agents::Roots::of().private.join("keys.toml")
}

fn read_keys() -> std::collections::HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(keys_path()) else {
        return std::collections::HashMap::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        tracing::warn!(file = %keys_path().display(), "the keystore is not TOML; ignoring it");
        return std::collections::HashMap::new();
    };
    value
        .get("keys")
        .and_then(|keys| keys.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(id, key)| key.as_str().map(|key| (id.clone(), key.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Where the roster is: an override, then the community copy under this machine's.
fn roster_paths() -> Vec<PathBuf> {
    match std::env::var("GONGGA_PROVIDERS_CATALOG") {
        Ok(over) if !over.is_empty() => vec![PathBuf::from(over)],
        _ => vec![crate::agents::Roots::of().roster()],
    }
}

impl Live {
    /// Whether the roster on hand still matches the files it came from: the
    /// roster, and the keystore beside it.
    fn is_fresh(&self, paths: &[PathBuf]) -> bool {
        self.read && self.stamps == stamps(paths)
    }
}

/// What each roster file looked like when it was read.
fn stamps(paths: &[PathBuf]) -> Vec<Option<Stamp>> {
    paths.iter().map(|path| Stamp::of(path)).collect()
}

/// One read of every roster file, stamped with the generation it belongs to.
fn read_fresh(paths: &[PathBuf]) -> Catalog {
    let mut catalog = Catalog::load_file(&paths[0]);
    catalog.keys = read_keys();
    catalog.generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    catalog
}

/// Every model a request could run on here.
///
/// # Invariant
///
/// One list, whether it reaches a client in the greeting or in [`Event::Models`]
/// after an edit: a completion is the same facts however late it learns them.
/// What each model *takes* is not here: it belongs to the argument that carries
/// it, so one roster edit republishes one catalog ([`level_accepts`] and
/// [`tier_accepts`] are where it comes from).
pub fn model_table() -> Vec<protocol::ModelInfo> {
    let roster = roster();
    roster
        .models()
        .into_iter()
        .map(|model| protocol::ModelInfo { model })
        .collect()
}

/// The reasoning levels a `--thinking` value may name, per model.
///
/// # Invariant
///
/// The same set [`Entry::reasoning_effort`] lets through: the roster's published
/// levels, the aliases its `reasoning_mapping` gives them, and
/// [`protocol::THINKING_DEFAULT`]. A model that publishes no levels takes any
/// spelling, so it is *absent* from the table rather than listed empty -- the two
/// are different answers, and `None` at the top says no model here constrains it.
pub fn level_accepts() -> Option<protocol::Accepts> {
    let mut table = std::collections::BTreeMap::new();
    for model in roster().models() {
        let Ok(entry) = roster().entry(&model) else {
            continue;
        };
        if entry.reasoning_levels.is_empty() {
            continue;
        }
        let mut levels = entry.reasoning_levels.clone();
        let mut aliases: Vec<String> = entry.reasoning_mapping.keys().cloned().collect();
        aliases.sort_unstable();
        for alias in aliases {
            if !levels.contains(&alias) {
                levels.push(alias);
            }
        }
        if !levels
            .iter()
            .any(|level| level == protocol::THINKING_DEFAULT)
        {
            levels.push(protocol::THINKING_DEFAULT.to_string());
        }
        table.insert(model, levels);
    }
    (!table.is_empty()).then_some(protocol::Accepts::PerModel(table))
}

/// The cache tiers a `--cache_tier` value may name, per model.
///
/// # Invariant
///
/// Every model is listed, and its list is exactly the tiers it declares, shortest
/// window first, with [`protocol::NONE_VALUE`] first of all: "send no tier" is
/// accepted by every model, including one that declares none -- which is the
/// model whose empty list would otherwise leave no value to type. A model that
/// declares no tiers therefore takes one value: `none`.
pub fn tier_accepts() -> Option<protocol::Accepts> {
    let models = roster().models();
    if models.is_empty() {
        return None;
    }
    let mut table = std::collections::BTreeMap::new();
    for model in models {
        let mut tiers = vec![protocol::NONE_VALUE.to_string()];
        tiers.extend(
            roster()
                .entry(&model)
                .map(|entry| {
                    let mut tiers: Vec<(String, u64)> = entry
                        .cache_ttl_tiers
                        .iter()
                        .map(|(name, secs)| (name.clone(), *secs))
                        .collect();
                    tiers.sort_by_key(|(_, secs)| *secs);
                    tiers
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default(),
        );
        table.insert(model, tiers);
    }
    Some(protocol::Accepts::PerModel(table))
}

/// The machine's roster, re-read whenever a file says it changed.
///
/// # Invariant
///
/// A caller keeps an `Arc`, never a `&'static`: a roster that is re-read must not
/// leave the retired list in someone's hand. Which read produced a list is
/// [`Catalog::generation`], and that is what retires a cached client.
pub fn roster() -> Arc<Catalog> {
    static LIVE: OnceLock<std::sync::RwLock<Live>> = OnceLock::new();
    let live = LIVE.get_or_init(|| {
        std::sync::RwLock::new(Live {
            read: false,
            stamps: Vec::new(),
            catalog: Arc::new(Catalog::empty()),
            last_good: None,
        })
    });
    // The keystore is watched as a file of its own: rotating a key is an edit
    // to it and nothing else.
    let mut paths = roster_paths();
    paths.push(keys_path());
    if let Ok(guard) = live.read() {
        if guard.is_fresh(&paths) {
            return guard.catalog.clone();
        }
    }
    let fresh = Arc::new(read_fresh(&paths));
    // A file that stops parsing is an edit in flight: the last good list is
    // still the best answer there is.
    let previous = live.read().ok().and_then(|guard| guard.last_good.clone());
    let fallback = match (fresh.problem(), previous) {
        (Some(problem), Some(good)) => Some(Arc::new(good.blaming(problem.to_string()))),
        _ => None,
    };
    let (catalog, serving_cached) = match fallback {
        Some(cached) => (cached, true),
        None => (fresh, false),
    };
    if let Ok(mut guard) = live.write() {
        guard.read = true;
        guard.stamps = stamps(&paths);
        guard.catalog = catalog.clone();
        // Only a read that parsed may become the fallback: a blaming copy must
        // not replace the roster it came from.
        if catalog.problem().is_none() {
            guard.last_good = Some(catalog.clone());
        }
    }
    tracing::info!(
        rosters = ?catalog.paths(),
        providers = catalog.providers().len(),
        generation = catalog.generation(),
        problem = catalog.problem(),
        serving_cached,
        "provider roster read"
    );
    catalog
}
