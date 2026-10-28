//! The machine's LLM roster: `~/.agents/llm.toml`.
//!
//! See `docs/notes/server-providers.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

/// Where the roster lives: one file per machine, at `~/.agents/llm.toml`.
pub const DEFAULT_PATH: &str = ".agents/llm.toml";

/// Everything `llm.toml` says about one provider.
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
    /// Input+output ceiling when published; a `--compact_threshold` percent is of it.
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

/// One resolved entry: where a request goes, nothing about whose key pays.
///
/// See `docs/notes/server-providers.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub model_name: String,
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

    /// The `provider/model` spelling, which is what `--llm` takes and what the
    /// session records.
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.model)
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
    path: Option<PathBuf>,
    providers: Vec<Provider>,
    /// Why the file could not be read, kept so startup can report it once and go on.
    problem: Option<String>,
    /// Which read produced this list; see [`roster`].
    generation: u64,
}

impl Catalog {
    /// Reads the roster, or reports why it could not.
    pub fn load(path: &Path) -> Self {
        let mut catalog = Self {
            path: Some(path.to_path_buf()),
            providers: Vec::new(),
            problem: None,
            generation: 0,
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                catalog.problem = Some(format!("{}: {e}", path.display()));
                return catalog;
            }
        };
        match parse(&text) {
            Ok(providers) => catalog.providers = providers,
            Err(e) => catalog.problem = Some(format!("{}: {e}", path.display())),
        }
        catalog
    }

    /// An empty roster: no providers to name, and no complaint to make.
    pub fn empty() -> Self {
        Self {
            path: None,
            providers: Vec::new(),
            problem: None,
            generation: 0,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
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

    /// Every `provider/model` worth offering, in file order, keys unresolved too.
    pub fn specs(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.providers {
            for m in &p.models {
                out.push(format!("{}/{}", p.id, m.id));
            }
        }
        out
    }

    /// Resolves `provider/model` into a destination plus the key to use for it.
    pub fn resolve(&self, spec: &str) -> Result<Choice, String> {
        self.resolve_with(spec, &lookup_env)
    }

    /// Resolves the destination only. The caller brings its own key.
    pub fn entry(&self, spec: &str) -> Result<Entry, String> {
        self.entry_with(spec)
    }

    /// A bare model id, looked for in every provider in file order. Handy when
    /// there is only one provider that could mean it.
    pub fn find_model(&self, model: &str) -> Option<Choice> {
        self.find_model_entry(model)
            .and_then(|e| self.resolve_with(&e.spec(), &lookup_env).ok())
    }

    /// The same search as [`Self::find_model`], without requiring a key at all.
    pub fn find_model_entry(&self, model: &str) -> Option<Entry> {
        self.providers
            .iter()
            .find(|p| p.models.iter().any(|m| m.id == model))
            .and_then(|p| self.entry_with(&format!("{}/{}", p.id, model)).ok())
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

    fn resolve_with(
        &self,
        spec: &str,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Choice, String> {
        let entry = self.entry_with(spec)?;
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

    /// Splits at the first `/`: provider ids never contain one, model ids
    /// sometimes do (`goat/deepseek/deepseek-v4.1-flash`).
    fn entry_with(&self, spec: &str) -> Result<Entry, String> {
        let (provider_id, model_id) = spec
            .split_once('/')
            .ok_or_else(|| format!("expected <provider>/<model>, got {spec:?}"))?;
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
        let model = provider
            .models
            .iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| {
                let mut known: Vec<&str> = provider.models.iter().map(|m| m.id.as_str()).collect();
                known.sort_unstable();
                format!(
                    "provider {} has no model {model_id:?}; known: {}",
                    provider.id,
                    known.join(", ")
                )
            })?;
        let endpoint = openai_endpoint(provider)
            .ok_or_else(|| format!("provider {} has no OpenAI-compatible endpoint", provider.id))?;
        Ok(Entry {
            provider: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: model.id.clone(),
            model_name: model.name.clone(),
            base_url: endpoint.base_url.trim_end_matches('/').to_string(),
            api_key_env: provider.key_env.clone(),
            key: provider.key.clone(),
            context: model.context,
            reasoning_levels: model.reasoning_levels.clone(),
            reasoning_default: model.reasoning_default.clone(),
            reasoning_mapping: model.reasoning_mapping.clone(),
            modalities: model.modalities.clone(),
            builtin_tools: model.builtin_tools.clone(),
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

fn parse(text: &str) -> Result<Vec<Provider>, toml::de::Error> {
    let file: FileRoster = toml::from_str(text)?;
    // Duplicate ids make `--llm` ambiguous; keep the first, drop the rest.
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
                id: p.id,
                name,
                key: p.key,
                key_env: p.key_env,
            }
        })
        .collect();
    Ok(providers)
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

/// The roster in force, and the file it came from.
struct Live {
    /// Whether a read happened: no roster file is still an answer, and asking
    /// twice must not re-read anything.
    read: bool,
    stamp: Option<Stamp>,
    catalog: Arc<Catalog>,
}

/// A live view of the roster: every question asks the file first.
///
/// # Invariant
///
/// Nothing here holds a `Catalog`. Holding one would freeze the roster at the
/// moment the server started -- which is exactly what a reload is meant to undo
/// -- and the next `set_session --llm` would refuse a model the user had just
/// written into the file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Handle;

impl Handle {
    pub fn resolve(&self, spec: &str) -> Result<Choice, String> {
        roster().resolve(spec)
    }

    pub fn entry(&self, spec: &str) -> Result<Entry, String> {
        roster().entry(spec)
    }

    pub fn find_model(&self, model: &str) -> Option<Choice> {
        roster().find_model(model)
    }

    pub fn generation(&self) -> u64 {
        roster().generation()
    }
}

/// Counts the reads, so a client built from a retired list is recognised as one.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where the roster is: an override, then the machine's own file.
fn roster_path() -> Option<PathBuf> {
    std::env::var("GONGGA_LLM_CATALOG")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(DEFAULT_PATH)))
}

impl Live {
    /// Whether the roster on hand still matches the file it came from.
    fn is_fresh(&self, path: Option<&Path>) -> bool {
        self.read && self.stamp == path.and_then(Stamp::of)
    }
}

/// One read of the roster file, stamped with the generation it belongs to.
fn read_fresh(path: Option<&Path>) -> Catalog {
    let mut catalog = match path {
        Some(p) => Catalog::load(p),
        None => Catalog::empty(),
    };
    catalog.generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    catalog
}

/// The machine's roster, re-read whenever the file says it changed.
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
            stamp: None,
            catalog: Arc::new(Catalog::empty()),
        })
    });
    let path = roster_path();
    if let Ok(guard) = live.read() {
        if guard.is_fresh(path.as_deref()) {
            return guard.catalog.clone();
        }
    }
    let catalog = Arc::new(read_fresh(path.as_deref()));
    if let Ok(mut guard) = live.write() {
        guard.read = true;
        guard.stamp = path.as_deref().and_then(Stamp::of);
        guard.catalog = catalog.clone();
    }
    tracing::info!(
        roster = catalog.path().unwrap_or(Path::new("(none)")).display().to_string(),
        providers = catalog.providers().len(),
        generation = catalog.generation(),
        problem = catalog.problem(),
        "llm roster read"
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
        let path = dir.path().join("llm.toml");
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
        let first = read_fresh(Some(&path));
        let second = read_fresh(Some(&path));
        assert_eq!(first.providers().len(), 1);
        assert_eq!(second.generation(), first.generation() + 1, "each read is counted");
        let live = Live {
            read: true,
            stamp: Stamp::of(&path),
            catalog: Arc::new(first),
        };
        assert!(live.is_fresh(Some(&path)), "the file did not move");
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
        assert!(!live.is_fresh(Some(&path)), "a model was added");
        let third = read_fresh(Some(&path));
        assert_eq!(
            third.specs().len(),
            2,
            "the new roster is what the next route resolves against"
        );
        // The model that was not in the file at startup now resolves, key and
        // endpoint and all.
        let added = third
            .entry("p/m2")
            .unwrap_or_else(|e| panic!("the added model must resolve: {e}"));
        assert_eq!(added.model, "m2");
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
                    path: None,
            providers: parse(ROSTER).unwrap(),
            problem: None,
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
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

    #[test]
    fn resolves_a_provider_model_pair() {
        let c = catalog();
        let choice = c
            .resolve_with("deepseek-official/deepseek-flash", &no_env)
            .unwrap();
        assert_eq!(choice.entry.spec(), "deepseek-official/deepseek-flash");
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
        assert_eq!(choice.entry.model, "deepseek/deepseek-v4.1-flash");
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

    /// `--llm opus` should find the one provider that has it, and skip the one
    /// whose endpoint this server cannot speak.
    #[test]
    fn a_bare_model_id_finds_its_provider() {
        let c = catalog();
        assert_eq!(
            c.find_model("deepseek-flash").unwrap().entry.provider,
            "deepseek-official"
        );
        // Only an Anthropic endpoint: not usable through the Responses API.
        assert!(c.find_model("opus").is_none());
        assert!(c.find_model("nope").is_none());
    }

    #[test]
    fn the_default_is_the_first_entry_that_can_actually_be_called() {
        let c = catalog();
        let entry = c.default_entry().expect("some entry is usable");
        assert_eq!(entry.provider, "deepseek-official");
        assert_eq!(entry.model, "deepseek-flash");
        // The point of picking it by default is that it is usable, not merely
        // listed: its key has to resolve too.
        assert!(c.resolve(&entry.spec()).is_ok());
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
                    path: None,
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

    #[test]
    fn a_missing_file_is_an_empty_catalog_with_a_reason() {
        let c = Catalog::load(Path::new("this/path/does/not/exist.toml"));
        assert!(c.providers().is_empty());
        assert!(c.problem().is_some());
        assert!(c.default_entry().is_none());
    }

    #[test]
    fn a_broken_file_reports_why_it_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.toml");
        std::fs::write(&path, "[[provider]\nid = ").unwrap();
        let c = Catalog::load(&path);
        assert!(c.providers().is_empty());
        assert!(c.problem().unwrap().contains("llm.toml"));
    }

    /// A duplicate id would make `--llm` ambiguous; the first one wins so the
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
        let path = dir.path().join("llm.toml");
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
            model: "m".into(),
            model_name: "M".into(),
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
