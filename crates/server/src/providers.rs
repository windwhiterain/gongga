//! The machine's LLM roster: the community `providers.toml`, under the user's own.
//!
//! See `docs/notes/server-providers.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

/// Everything `providers.toml` says about one provider.
#[derive(Debug, Clone)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub endpoints: Vec<Endpoint>,
    pub models: Vec<Model>,
    /// The key as written in the file, if it was written there.
    key: Option<String>,
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

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Input+output ceiling when published; a `--context_limit` percent is of it.
    pub context: Option<u64>,
    /// Request paths this model accepts; empty means unstated, assume OpenAI.
    pub endpoints: Vec<String>,
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
}

impl Model {
    /// Whether this model can be driven through the Responses API, which is the
    /// only shape this server speaks.
    fn speaks_responses(&self) -> bool {
        self.endpoints.is_empty() || self.endpoints.iter().any(|e| e.contains("responses"))
    }
}

/// One model as a caller picks it: which provider serves it, and where to ask.
///
/// See `docs/notes/server-providers.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// An [`Entry`] plus the key that resolved for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub entry: Entry,
    pub api_key: String,
    /// Where `api_key` came from when it came from the environment, so a
    /// failure can name the variable instead of its value.
    pub api_key_env: Option<String>,
}

#[derive(Debug)]
pub struct Catalog {
    /// The files this list was assembled from, lowest precedence first.
    paths: Vec<PathBuf>,
    providers: Vec<Provider>,
    /// Why a file could not be read, kept so startup can report it once and go on.
    problem: Option<String>,
    /// Which read produced this list; see [`roster`].
    generation: u64,
}

impl Catalog {
    /// Reads one roster, or reports why it could not: what a test drives.
    #[cfg(test)]
    pub fn load(path: &Path) -> Self {
        Self::load_layers(std::slice::from_ref(&path.to_path_buf()))
    }

    /// Reads every roster that exists and merges them, later files over earlier.
    ///
    /// # Invariant
    ///
    /// A missing layer is not a problem worth a line, and a broken one costs only
    /// itself: the layers that did parse still answer `--model`. See
    /// `docs/notes/server-providers.md`.
    pub fn load_layers(paths: &[PathBuf]) -> Self {
        let mut catalog = Self {
            paths: paths.to_vec(),
            providers: Vec::new(),
            problem: None,
            generation: 0,
        };
        let mut merged: Option<toml::Value> = None;
        for path in paths {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    catalog
                        .problem
                        .get_or_insert(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            match toml::from_str::<toml::Value>(&text) {
                Ok(mut value) => {
                    resolve_scripts(&mut value, path.parent());
                    match &mut merged {
                        Some(base) => merge_into(base, value),
                        None => merged = Some(value),
                    }
                }
                Err(e) => {
                    catalog
                        .problem
                        .get_or_insert(format!("{}: {e}", path.display()));
                }
            }
        }
        match merged.map(parse_value) {
            Some(Ok(providers)) => catalog.providers = providers,
            Some(Err(e)) => {
                let layers: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                catalog
                    .problem
                    .get_or_insert(format!("{} (merged): {e}", layers.join(", ")));
            }
            None => {}
        }
        catalog
    }

    /// An empty roster: no providers to name, and no complaint to make.
    pub fn empty() -> Self {
        Self {
            paths: Vec::new(),
            providers: Vec::new(),
            problem: None,
            generation: 0,
        }
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
    /// on which every request fails. See `docs/notes/server-providers.md`.
    pub fn models(&self) -> Vec<String> {
        self.models_with(&lookup_env)
    }

    /// The same list, with the credential looked up by `lookup`: what a test drives.
    pub fn models_with(&self, lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        let mut out = Vec::new();
        for provider in &self.providers {
            if credential(provider, lookup).is_none() {
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
        let key = credential(provider, &lookup_env).map(|(key, _)| key);
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
        let (api_key, api_key_env) =
            credential(provider, lookup).ok_or_else(|| match &provider.key_env {
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
        let endpoint = openai_endpoint(provider)
            .ok_or_else(|| format!("provider {} has no OpenAI-compatible endpoint", provider.id))?;
        Ok(Entry {
            provider: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: format!("{}/{}", provider.id, found.id),
            model_name: found.id.clone(),
            model_title: found.name.clone(),
            base_url: endpoint.base_url.trim_end_matches('/').to_string(),
            api_key_env: provider.key_env.clone(),
            key: provider.key.clone(),
            context: found.context,
            reasoning_levels: found.reasoning_levels.clone(),
            reasoning_default: found.reasoning_default.clone(),
            reasoning_mapping: found.reasoning_mapping.clone(),
            modalities: found.modalities.clone(),
            builtin_tools: found.builtin_tools.clone(),
        })
    }
}

fn lookup_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The endpoint to talk to; `openai-responses` beats the other flavours.
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

/// The key, and what to call it in a message.
///
/// # Invariant
///
/// An exported environment variable wins over a literal key in the file.
///
/// See `docs/notes/server-providers.md`.
fn credential(
    provider: &Provider,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, Option<String>)> {
    if let Some(name) = &provider.key_env {
        if let Some(value) = lookup(name).filter(|v| !v.is_empty()) {
            return Some((value, Some(name.clone())));
        }
    }
    if let Some(key) = provider.key.as_ref().filter(|k| !k.is_empty()) {
        return Some((key.clone(), provider.key_env.clone()));
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
    key: Option<String>,
    #[serde(default)]
    key_env: Option<String>,
    #[serde(default)]
    endpoint: Vec<FileEndpoint>,
    #[serde(default)]
    model: Vec<FileModel>,
    #[serde(default)]
    quota_query: Vec<FileQuota>,
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
                        endpoints: m.endpoints,
                        reasoning_levels: m.reasoning_levels,
                        reasoning_default: m.reasoning_default,
                        reasoning_mapping: m.reasoning_mapping,
                        modalities: m.modalities,
                        builtin_tools: m.builtin_tools,
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
                key: p.key,
                key_env: p.key_env,
            }
        })
        .collect();
    Ok(providers)
}

#[cfg(test)]
/// A roster read from text, for the cases that never touch a file.
fn parse(text: &str) -> Result<Vec<Provider>, toml::de::Error> {
    parse_value(toml::from_str(text)?)
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

/// Lays `over` onto `base`: tables key by key, `[[tables]]` by their `id`.
///
/// # Invariant
///
/// A key only `over` has is added, and one both have is merged, never dropped:
/// the private file may name one field of a provider and leave the rest to the
/// community copy. Array order is the base's, so the preference order a roster
/// spells with file order survives the merge.
fn merge_into(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(base), toml::Value::Table(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(slot) => merge_into(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (toml::Value::Array(base), toml::Value::Array(over)) if by_id(base, &over) => {
            for element in over {
                let id = element.get("id").cloned();
                match base
                    .iter_mut()
                    .find(|candidate| candidate.get("id").cloned() == id)
                {
                    Some(slot) => merge_into(slot, element),
                    None => base.push(element),
                }
            }
        }
        (slot, over) => *slot = over,
    }
}

/// Whether both arrays are tables a merge can pair up by `id` rather than replace.
fn by_id(base: &[toml::Value], over: &[toml::Value]) -> bool {
    base.iter()
        .chain(over.iter())
        .all(|v| v.get("id").map(|id| id.is_str()).unwrap_or(false))
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

    pub fn entry(&self, model: &str) -> Result<Entry, String> {
        roster().entry(model)
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

/// Where the roster is: an override, then the community copy under this machine's.
fn roster_paths() -> Vec<PathBuf> {
    match std::env::var("GONGGA_PROVIDERS_CATALOG") {
        Ok(over) if !over.is_empty() => vec![PathBuf::from(over)],
        _ => crate::agents::Roots::of().rosters(),
    }
}

impl Live {
    /// Whether the roster on hand still matches the files it came from.
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
    let mut catalog = Catalog::load_layers(paths);
    catalog.generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    catalog
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
        })
    });
    let paths = roster_paths();
    if let Ok(guard) = live.read() {
        if guard.is_fresh(&paths) {
            return guard.catalog.clone();
        }
    }
    let catalog = Arc::new(read_fresh(&paths));
    if let Ok(mut guard) = live.write() {
        guard.read = true;
        guard.stamps = stamps(&paths);
        guard.catalog = catalog.clone();
    }
    tracing::info!(
        rosters = ?catalog.paths(),
        providers = catalog.providers().len(),
        generation = catalog.generation(),
        problem = catalog.problem(),
        "provider roster read"
    );
    catalog
}

#[cfg(test)]
mod tests {
    /// An edit to the roster lands without a restart; an untouched file is not
    /// read twice.
    #[test]
    fn the_roster_is_reread_only_when_the_file_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"[[provider]]
id = "p"
key = "k"
[[provider.endpoint]]
api = "openai-responses"
base_url = "http://127.0.0.1:1/v1"
[[provider.model]]
id = "m1"
"#,
        )
        .unwrap();
        let first = read_fresh(std::slice::from_ref(&path));
        let second = read_fresh(std::slice::from_ref(&path));
        assert_eq!(first.providers().len(), 1);
        // Forward, not exactly one: the counter is one per process, so a read
        // from another test's thread is counted here too.
        assert!(
            second.generation() > first.generation(),
            "each read is counted"
        );
        let live = Live {
            read: true,
            stamps: stamps(std::slice::from_ref(&path)),
            catalog: Arc::new(first),
        };
        assert!(
            live.is_fresh(std::slice::from_ref(&path)),
            "the file did not move"
        );
        std::fs::write(
            &path,
            r#"[[provider]]
id = "p"
key = "k"
[[provider.endpoint]]
api = "openai-responses"
base_url = "http://127.0.0.1:1/v1"
[[provider.model]]
id = "m1"
[[provider.model]]
id = "m2"
"#,
        )
        .unwrap();
        assert!(
            !live.is_fresh(std::slice::from_ref(&path)),
            "a model was added"
        );
        let third = read_fresh(std::slice::from_ref(&path));
        assert_eq!(
            third.models().len(),
            2,
            "the new roster is what the next route resolves against"
        );
        // The model that was not in the file at startup now resolves, key and
        // endpoint and all.
        let added = third
            .entry("p/m2")
            .unwrap_or_else(|e| panic!("the added model must resolve: {e}"));
        assert_eq!(added.model_name, "m2");
        assert_eq!(added.model, "p/m2", "a model is named provider and name");
    }

    use super::*;

    const ROSTER: &str = r#"
[[provider]]
id = "deepseek-official"
name = "DeepSeek 官方"
key = "sk-literal"
key_env = "DEEPSEEK_API_KEY"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-completions"
  base_url = "https://api.deepseek.com/v1/"

  [[provider.model]]
  id = "deepseek-flash"
  name = "DeepSeek V4.1-Flash-1017"
  context = 1000000
  max_output = 384000

  [[provider.model]]
  id = "deepseek-v4-pro"
  name = "DeepSeek V4-Pro"
  context = 1000000

[[provider]]
id = "goat"
name = "GOAT"
key = "user_literal"

  [[provider.endpoint]]
  id = "openai"
  api = "openai-completions"
  base_url = "https://api.commandcode.ai/provider/v1"

  # A slash inside the model id: the provider id is what splits, not the model.
  [[provider.model]]
  id = "deepseek/deepseek-v4.1-flash"
  context = 1000000
  endpoints = ["/chat/completions", "/responses"]

[[provider]]
id = "anthropic-only"
key = "x"

  [[provider.endpoint]]
  id = "anthropic"
  api = "anthropic-messages"
  base_url = "https://api.example.com/anthropic"

  [[provider.model]]
  id = "opus"
  context = 200000
"#;

    fn catalog() -> Catalog {
        Catalog {
            generation: 0,
            paths: Vec::new(),
            providers: parse(ROSTER).unwrap(),
            problem: None,
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// # Invariant
    ///
    /// Completion is a promise that the model can be run here. A provider whose
    /// credential resolves is offered; one that names a key this machine does not
    /// have is not, because the request it would name fails with the roster's own
    /// "no key" error before it leaves. `models()` asks the environment; this test
    /// drives `models_with` with its own answer.
    #[test]
    fn a_provider_without_a_key_is_not_offered() {
        let c = catalog();
        let none = c.models_with(&|_| None);
        // Every provider in the fixture carries a literal key except this one.
        assert!(
            none.iter().any(|m| m.starts_with("deepseek-official/")),
            "a literal key counts: {none:?}"
        );
        assert!(
            none.iter().any(|m| m.starts_with("goat/")),
            "a literal key counts: {none:?}"
        );

        let mut no_literal = catalog();
        for provider in &mut no_literal.providers {
            provider.key = None;
        }
        let by_env =
            no_literal.models_with(&|name| (name == "DEEPSEEK_API_KEY").then(|| "k".to_string()));
        assert!(
            by_env.iter().all(|m| m.starts_with("deepseek-official/")),
            "only the provider whose variable is set is offered: {by_env:?}"
        );
        assert_eq!(
            by_env.len(),
            no_literal.providers[0].models.len(),
            "every model of a keyed provider is offered"
        );
        assert!(
            no_literal.models_with(&|_| None).is_empty(),
            "no key anywhere offers nothing, rather than what cannot run"
        );
    }

    #[test]
    fn parses_providers_endpoints_and_models() {
        let c = catalog();
        assert_eq!(c.providers().len(), 3);
        let p = &c.providers()[0];
        assert_eq!(p.id, "deepseek-official");
        assert_eq!(p.name, "DeepSeek 官方");
        assert_eq!(p.endpoints[0].base_url, "https://api.deepseek.com/v1/");
        assert_eq!(p.models[0].id, "deepseek-flash");
        assert_eq!(p.models[0].context, Some(1_000_000));
    }

    /// The quota a provider publishes, and the key it is read with: the same
    /// credential a request would use.
    #[test]
    fn quota_queries_come_with_the_providers_own_credential() {
        let text = r#"[[provider]]
id = "p"
key_env = "P_KEY"
[[provider.quota_query]]
id = "wallet"
unit = "cny-left"
script = "providers/p.py"
script_env = "P_KEY"
[[provider.quota_query]]
id = "second"
unit = "percent-left"
"#;
        let catalog = Catalog {
            generation: 0,
            paths: Vec::new(),
            providers: parse(text).unwrap(),
            problem: None,
        };
        let env = |name: &str| (name == "P_KEY").then(|| "sk-env".to_string());
        let provider = &catalog.providers()[0];
        let key = credential(provider, &env).map(|(k, _)| k);
        assert_eq!(key.as_deref(), Some("sk-env"));
        assert_eq!(
            provider.quota.len(),
            2,
            "file order is the preference order"
        );
        assert_eq!(provider.quota[0].id, "wallet");
        assert_eq!(provider.quota[0].unit, "cny-left");
        assert_eq!(provider.quota[0].script_env.as_deref(), Some("P_KEY"));
        assert_eq!(
            provider.quota[1].script, None,
            "a query with no script is not one this server can read"
        );
        assert!(catalog.quota_of("nobody").is_none());
    }

    /// A `script` is a path inside the layer that wrote it: resolved before merging.
    #[test]
    fn a_quota_script_resolves_against_its_own_layer() {
        let community = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let text = r#"[[provider]]
id = "p"
[[provider.model]]
id = "m"
[[provider.quota_query]]
id = "wallet"
script = "providers/p.py"
"#;
        let base = community.path().join("providers.toml");
        let over = private.path().join("providers.toml");
        std::fs::write(&base, text).unwrap();
        std::fs::write(&over, text).unwrap();
        let catalog = Catalog::load_layers(&[base.clone(), over.clone()]);
        let script = catalog.providers()[0].quota[0]
            .script
            .clone()
            .expect("a script");
        // The later layer wins the merge, and its own directory comes with it.
        assert_eq!(script, private.path().join("providers").join("p.py"));
        assert!(script.is_absolute());
    }

    #[test]
    fn resolves_a_provider_model_pair() {
        let c = catalog();
        let choice = c
            .resolve_with("deepseek-official/deepseek-flash", &no_env)
            .unwrap();
        assert_eq!(choice.entry.model, "deepseek-official/deepseek-flash");
        // The trailing slash in the file must not become a double slash in the
        // URL the request is built from.
        assert_eq!(choice.entry.base_url, "https://api.deepseek.com/v1");
        assert_eq!(choice.api_key, "sk-literal");
        assert_eq!(choice.entry.context, Some(1_000_000));
    }

    #[test]
    fn a_model_id_may_contain_a_slash() {
        let c = catalog();
        let choice = c
            .resolve_with("goat/deepseek/deepseek-v4.1-flash", &no_env)
            .unwrap();
        assert_eq!(choice.entry.provider, "goat");
        assert_eq!(choice.entry.model_name, "deepseek/deepseek-v4.1-flash");
        assert_eq!(
            choice.entry.model, "goat/deepseek/deepseek-v4.1-flash",
            "the first slash is the providers"
        );
    }

    #[test]
    fn the_environment_beats_the_literal_key() {
        let c = catalog();
        let env = |name: &str| (name == "DEEPSEEK_API_KEY").then(|| "sk-from-env".to_string());
        let choice = c
            .resolve_with("deepseek-official/deepseek-flash", &env)
            .unwrap();
        assert_eq!(choice.api_key, "sk-from-env");
        assert_eq!(choice.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    }

    /// An unknown provider reports what is known: the message is the fix.
    #[test]
    fn an_unknown_provider_lists_the_known_ones() {
        let c = catalog();
        let err = c.resolve_with("nope/model", &no_env).unwrap_err();
        assert!(
            err.contains("known: anthropic-only, deepseek-official, goat"),
            "{err}"
        );
        let err = c.resolve_with("goat/missing-model", &no_env).unwrap_err();
        assert!(err.contains("has no model"), "{err}");
    }

    #[test]
    fn a_provider_without_a_key_is_refused_by_name() {
        let c = catalog();
        let err = c.resolve_with("deepseek-official/deepseek-flash", &no_env);
        assert!(err.is_ok(), "the literal key is enough");

        let mut c = catalog();
        c.providers[0].key = None;
        let err = c
            .resolve_with("deepseek-official/deepseek-flash", &no_env)
            .unwrap_err();
        assert!(err.contains("DEEPSEEK_API_KEY"), "{err}");
    }

    /// `--model opus` should find the one provider that has it, and skip the one
    /// whose endpoint this server cannot speak.
    #[test]
    fn a_bare_model_id_finds_its_provider() {
        let c = catalog();
        assert_eq!(
            c.resolve_by_name("deepseek-flash").unwrap().entry.provider,
            "deepseek-official"
        );
        // Only an Anthropic endpoint: not usable through the Responses API.
        assert!(c.resolve_by_name("opus").is_none());
        assert!(c.resolve_by_name("nope").is_none());
    }

    #[test]
    fn the_default_is_the_first_entry_that_can_actually_be_called() {
        let c = catalog();
        let entry = c.default_entry().expect("some entry is usable");
        assert_eq!(entry.provider, "deepseek-official");
        assert_eq!(entry.model_name, "deepseek-flash");
        assert_eq!(entry.model, "deepseek-official/deepseek-flash");
        // The point of picking it by default is that it is usable, not merely
        // listed: its key has to resolve too.
        assert!(c.resolve(&entry.model).is_ok());
    }

    /// A model that only declares `/chat/completions` is not a Responses
    /// model, so it must not be offered as the default.
    #[test]
    fn a_chat_only_model_is_not_the_default() {
        let text = r#"
[[provider]]
id = "chat-only"
key = "k"
  [[provider.endpoint]]
  api = "openai-completions"
  base_url = "https://example.com/v1"
  [[provider.model]]
  id = "m"
  endpoints = ["/chat/completions"]
"#;
        let c = Catalog {
            generation: 0,
            paths: Vec::new(),
            providers: parse(text).unwrap(),
            problem: None,
        };
        assert!(c.default_entry().is_none());
    }

    #[test]
    fn a_percentage_becomes_a_token_budget() {
        assert_eq!(tokens_for_percent(1_000_000, 10), 100_000);
        assert_eq!(tokens_for_percent(1_000_000, 0), 0);
        // Never zero for a real percentage: zero reads as compression being off.
        assert_eq!(tokens_for_percent(10, 1), 1);
    }

    /// Absent is normal: a machine that never wrote a roster is the first start.
    #[test]
    fn a_missing_file_is_an_empty_catalog_with_no_complaint() {
        let c = Catalog::load(Path::new("this/path/does/not/exist.toml"));
        assert!(c.providers().is_empty());
        assert!(c.problem().is_none());
        assert!(c.default_entry().is_none());
    }

    /// The community copy is the base; the private one overrides field by field.
    #[test]
    fn a_private_field_overrides_the_community_one() {
        let d = tempfile::tempdir().unwrap();
        let community = d.path().join("community.toml");
        let private = d.path().join("private.toml");
        std::fs::write(
            &community,
            r#"
[[provider]]
id = "shared"
name = "Shared"
key_env = "SHARED_API_KEY"
  [[provider.endpoint]]
  id = "openai"
  api = "openai-responses"
  base_url = "https://community.example/v1"
  [[provider.model]]
  id = "m1"
  context = 1000
  [[provider.model]]
  id = "m2"
  context = 2000
[[provider]]
id = "community-only"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "https://only.example/v1"
  [[provider.model]]
  id = "m"
"#,
        )
        .unwrap();
        // Only what this machine knows differently: one endpoint host, one key.
        std::fs::write(
            &private,
            r#"
[[provider]]
id = "shared"
key = "private-key"
  [[provider.endpoint]]
  id = "openai"
  base_url = "https://private.example/v1"
  [[provider.model]]
  id = "m2"
  context = 9000
"#,
        )
        .unwrap();
        let c = Catalog::load_layers(&[community, private]);
        let ids: Vec<&str> = c.providers().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["shared", "community-only"],
            "community order, then new"
        );

        // The endpoint stays the community one, with the private host on it.
        let entry = c.entry("shared/m1").unwrap();
        assert_eq!(
            entry.provider_name, "Shared",
            "an unset field keeps its base"
        );
        assert_eq!(entry.base_url, "https://private.example/v1");
        assert_eq!(
            entry.context,
            Some(1000),
            "an untouched model is the base's"
        );
        assert_eq!(c.entry("shared/m2").unwrap().context, Some(9000));
        assert_eq!(
            c.resolve_with("shared/m1", &no_env).unwrap().api_key,
            "private-key"
        );
    }

    /// One layer that does not parse costs only itself: the others still answer.
    #[test]
    fn a_broken_layer_is_reported_and_the_rest_still_loads() {
        let d = tempfile::tempdir().unwrap();
        let community = d.path().join("community.toml");
        let private = d.path().join("private.toml");
        std::fs::write(
            &community,
            r#"
[[provider]]
id = "p"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "https://p.example/v1"
  [[provider.model]]
  id = "m"
"#,
        )
        .unwrap();
        std::fs::write(&private, "[[provider]\nid = ").unwrap();
        let c = Catalog::load_layers(&[community, private]);
        assert_eq!(c.providers().len(), 1, "the good layer is still read");
        assert!(c.problem().unwrap().contains("private.toml"));
    }

    #[test]
    fn a_broken_file_reports_why_it_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(&path, "[[provider]\nid = ").unwrap();
        let c = Catalog::load(&path);
        assert!(c.providers().is_empty());
        assert!(c.problem().unwrap().contains("providers.toml"));
    }

    /// A duplicate id would make `--model` ambiguous; the first one wins so the
    /// answer does not depend on map iteration order.
    #[test]
    fn duplicate_provider_ids_keep_the_first() {
        let text = r#"
[[provider]]
id = "dup"
key = "first"
  [[provider.endpoint]]
  api = "openai-completions"
  base_url = "https://one.example/v1"
  [[provider.model]]
  id = "m"
[[provider]]
id = "dup"
key = "second"
  [[provider.endpoint]]
  api = "openai-completions"
  base_url = "https://two.example/v1"
  [[provider.model]]
  id = "m"
"#;
        let providers = parse(text).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].endpoints[0].base_url, "https://one.example/v1");
    }

    /// `--thinking_effort` is checked against these, and the mapping translates.
    #[test]
    fn reasoning_levels_are_read_and_mapped() {
        let text = r#"
[[provider]]
id = "p"
key = "k"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "https://p.example/v1"
  [[provider.model]]
  id = "m"
  name = "M"
  context = 100000
  reasoning_levels = ["low", "high", "max"]
  reasoning_default = "high"
  [provider.model.reasoning_mapping]
  medium = "high"
  xhigh = "max"
"#;
        let providers = parse(text).unwrap();
        let model = &providers[0].models[0];
        assert_eq!(model.reasoning_levels, ["low", "high", "max"]);
        assert_eq!(model.reasoning_default.as_deref(), Some("high"));
        assert_eq!(
            model.reasoning_mapping.get("medium").map(String::as_str),
            Some("high")
        );
    }

    /// Capabilities are read off the roster, in the roster's own spelling.
    #[test]
    fn modalities_and_builtin_tools_reach_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"
[[provider]]
id = "p"
key = "k"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "https://p.example/v1"
  [[provider.model]]
  id = "sees"
  modalities = ["text", "image"]
  builtin_tools = ["web_search", "web_fetch"]
  [[provider.model]]
  id = "plain"
"#,
        )
        .unwrap();
        let roster = Catalog::load(&path);
        let entry = roster.entry("p/sees").unwrap();
        assert_eq!(entry.modalities, vec!["text", "image"]);
        assert_eq!(entry.builtin_tools, vec!["web_search", "web_fetch"]);

        // Undeclared is empty, not a default: the server sends no tool and
        // refuses nothing on the roster's behalf.
        let plain = roster.entry("p/plain").unwrap();
        assert!(plain.modalities.is_empty());
        assert!(plain.builtin_tools.is_empty());
    }

    /// A level is mapped first, then checked against what the model takes.
    #[test]
    fn a_level_is_mapped_then_checked() {
        let entry = Entry {
            provider: "p".into(),
            provider_name: "P".into(),
            model: "p/m".into(),
            model_name: "m".into(),
            model_title: "M".into(),
            base_url: String::new(),
            api_key_env: None,
            key: None,
            context: None,
            reasoning_levels: vec!["low".into(), "high".into(), "max".into()],
            modalities: Vec::new(),
            builtin_tools: Vec::new(),
            reasoning_default: Some("high".into()),
            reasoning_mapping: [
                ("medium".to_string(), "high".to_string()),
                ("xhigh".to_string(), "max".to_string()),
            ]
            .into(),
        };
        assert_eq!(entry.reasoning_effort("medium").unwrap(), "high");
        assert_eq!(entry.reasoning_effort("xhigh").unwrap(), "max");
        assert_eq!(entry.reasoning_effort("low").unwrap(), "low");
        let refused = entry.reasoning_effort("ultra").unwrap_err();
        assert!(
            refused.contains("low, high, max"),
            "the refusal has to say what would work: {refused}"
        );

        // A roster silent on levels cannot refuse one: the level goes through.
        let silent = Entry {
            reasoning_levels: Vec::new(),
            reasoning_mapping: Default::default(),
            ..entry
        };
        assert_eq!(silent.reasoning_effort("ultra").unwrap(), "ultra");
    }
}
