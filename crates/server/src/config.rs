//! Configuration resolution: config file, then environment, then defaults.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transport::TransportConfig;

pub const DEFAULT_BIND: &str = "127.0.0.1:9700";
pub const DEFAULT_DB: &str = "gongga.db";

/// Default provider: MiniMax's China endpoint, OpenAI-compatible Responses API.
pub const DEFAULT_BASE_URL: &str = "https://api.minimax.cn/v1";
pub const DEFAULT_MODEL_NAME: &str = "MiniMax-M3.1-Flash";
pub const DEFAULT_API_KEY_ENV: &str = "MINIMAX_CN_API_KEY";

/// The model a session runs on when the session itself names none.
///
/// # Invariant
///
/// A session's model belongs to the session: `set_session --model` writes it,
/// and nothing re-derives it from the `[model]` section above.
///
/// A constant, not a config key: a config key would decide what every session
/// runs on. An empty one makes `set_session` require `--model`: refusing at the
/// command beats failing at the first turn.
pub const DEFAULT_SESSION_MODEL: &str = "deepseek-official/deepseek-flash";

/// Tool output larger than this is capped and spilled to disk, keeping `cat`s out
/// of later requests.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 8_000;
/// Input-token ceiling that triggers the context notice; fallback with no roster window.
pub const DEFAULT_COMPRESS_ABOVE_TOKENS: u64 = 40_000;

/// The token growth an enabled live compact measures against when no interval is stated.
pub const DEFAULT_LIVE_COMPACT_INTERVAL_TOKENS: u64 = 20_000;

/// How often a prefix the provider once held is assumed to be gone.
///
/// # Invariant
///
/// A hyperparameter, not a measurement: it turns the roster's rates into what a
/// warm item is expected to be worth. See `docs/notes/server-prefix-cache.md`.
pub const DEFAULT_CACHE_INVALID_RATE: f64 = 0.0;

/// The lifetime a cached prefix gets when this machine states none.
///
/// # Invariant
///
/// `0` means the roster decides, and a provider it says nothing about is left
/// never expiring.
pub const DEFAULT_CACHE_TTL_SECS: u64 = 0;

/// The share of a planned entry's lines that may change before it is re-sent.
pub const DEFAULT_PLAN_CHANGE_TOLERANCE: f64 = 0.2;

/// The room left below which a volume counts as out of room, in MiB.
///
/// # Invariant
///
/// A threshold, not a budget: nothing is held back and nothing is written. It is
/// the number the free space reading is compared against, on every tool call. See
/// `docs/notes/server-disk.md`.
pub const DEFAULT_DISK_LOW_MB: u64 = 1024;

/// How long one quota endpoint's reading stands before it is asked again, in
/// seconds; `0` asks at every step's end.
///
/// # Invariant
///
/// A cadence, not a budget: nothing is held back and no number is withheld. It is
/// what one endpoint's last answer is compared against, so a turn of many steps
/// reads a provider's allowance once instead of once per step. See
/// `docs/notes/server-quota.md`.
pub const DEFAULT_QUOTA_INTERVAL_SECS: u64 = 60;

/// How long a tool's own mechanism may take before the harness calls it a fault.
///
/// # Invariant
///
/// Internal and invisible: nothing a caller writes, reads or is told depends on
/// this number while the harness is correct. It exists so a hung mechanism ends
/// instead of holding a turn for ever. A tool whose wait is the call's own --
/// `exec`'s `wait`, `wait`'s `time` -- does not use it at all.
pub const CALL_TIMEOUT_MS: u64 = 120_000;

/// How long a peer may stay silent when `[remote] answer_timeout_ms` states nothing.
///
/// # Invariant
///
/// A ceiling on one answer, never on a command: what it ends is the request, with an
/// error. A call's own clock is measured on this machine, and no network number
/// may move it.
pub const DEFAULT_ANSWER_TIMEOUT_MS: u64 = 30_000;

/// Share of a model's window `--context_limit` starts at; see `docs/notes/server-config.md`.
pub const DEFAULT_CONTEXT_LIMIT_PERCENT: u8 = 70;
/// Everything the server needs to boot.
#[derive(Debug, Clone)]
pub struct Config {
    /// Which read of the files produced this value, counted for this process.
    ///
    /// # Invariant
    ///
    /// A cache that holds something built *from* a config -- an HTTP client, a
    /// tool list -- is keyed on this, so an edit that changes a timeout or a base
    /// URL cannot keep sending the one the old read stated.
    pub read: u64,
    /// The file these values came from; the templates read the same path.
    pub config_path: PathBuf,
    pub home: PathBuf,
    pub db_path: PathBuf,
    pub bind: SocketAddr,
    /// Shared secret a client must present before it may drive the harness.
    pub key: String,
    /// The model the server runs on when a session names none.
    pub model: ModelConfig,
    /// What every tool call shares: the cap, and the rows this home declares.
    pub tools: ToolsConfig,
    /// The persistent Python interpreter a `python` call runs in.
    pub python: PythonConfig,
    pub scripts: ScriptConfig,
    pub agent: AgentConfig,
    pub transport: TransportConfig,
    /// What a conversation with a peer may cost in silence.
    pub endpoint: EndpointConfig,
    /// Room left below which this server says the disk is out of room. See
    /// `docs/notes/server-disk.md`.
    pub disk_low: u64,
    /// How long one endpoint's reading stands; `0` asks at every step end. See
    /// `docs/notes/server-quota.md`.
    pub quota_interval: Duration,
    /// `[ship] url`: this home's content repository. See `docs/notes/server-ship.md`.
    pub ship_url: Option<String>,
}

/// The model the server talks to by default: which provider serves it, and how.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// The provider's id, e.g. `deepseek-official`; only used in messages.
    pub provider: Option<String>,
    /// The name the provider's API takes in its `model` field.
    pub model_name: String,
    /// Base URL of an OpenAI-compatible Responses API. Defaults to MiniMax CN.
    pub base_url: String,
    /// Context window of the model; `--context_limit` is a percentage of it.
    pub context: Option<u64>,
    /// Name of the environment variable holding the API key.
    pub api_key_env: String,
    /// Resolved key. Empty when the variable is unset.
    pub api_key: String,
    pub temperature: Option<f32>,
    /// The longest answer to ask for. Required by the anthropic-messages face.
    pub max_output_tokens: Option<u32>,
    /// The endpoint's protocol label: `openai-responses`, `anthropic-messages`, ...
    pub api: String,
    /// Where a request's measured tokens are recorded, when a store exists.
    pub measure: Option<crate::store::Store>,
    /// What the roster says this model charges, for the prefix cache's plan.
    pub price: Option<crate::providers::Price>,
    /// The cache tier to ask for; `None` sends no directive.
    pub cache_tier: Option<String>,
    /// Tiers the roster says this provider accepts, by its API's spelling.
    pub cache_tiers: HashMap<String, u64>,
    /// Percent of the tier's window left when the interrupt or a heartbeat fires.
    pub cache_margin_pct: u8,
    /// Send `prompt_cache_key`, for a provider known to accept one.
    ///
    /// # Invariant
    ///
    /// The value is the per-conversation id, the same one `session_id_headers`
    /// carries: vendors that document it ask for session isolation, not a model
    /// region and not a directory. See `docs/notes/server-config.md`.
    pub cache_affinity: bool,
    /// Header names that carry the per-conversation id; empty sends nothing.
    pub session_id_headers: Vec<String>,
    /// Ask the API not to retain server-side state; we replay the transcript.
    pub store: bool,
    /// How long the provider may take to answer with response headers, the
    /// upload of the request included.
    ///
    /// # Invariant
    ///
    /// Nothing about the body: a stream that is still producing is never cut by
    /// a wall clock. See `docs/notes/server-config.md`.
    pub headers_timeout: Duration,
    /// Silence of content that counts as a broken stream: chunks that carry no
    /// event (keep-alives) do not move this clock.
    pub idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response; 0 is no ceiling.
    pub max_retries: u32,
    /// First retry delay; every further attempt doubles it, with no ceiling.
    pub retry_base: Duration,
    /// Provider-side tools, in the provider's spelling; sent verbatim. Empty is none.
    pub builtin_tools: Vec<String>,
    /// What the roster says this model takes in; empty means unstated.
    pub modalities: Vec<String>,
}

/// Whether a roster `modalities` list admits pictures.
///
/// # Invariant
///
/// Unstated (`[]`) admits everything: the roster having no opinion is not the
/// roster saying no. See `docs/notes/server-providers.md`.
pub fn accepts_image(modalities: &[String]) -> bool {
    modalities.is_empty() || modalities.iter().any(|m| m == "image")
}

/// Why an image cannot be sent to `model`: the `image_refusal` template, with the
/// roster's own list spelt out for it.
pub fn image_refusal(
    prompts: &crate::prompts::Prompts,
    model: &str,
    modalities: &[String],
) -> String {
    let listed = modalities
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    prompts.render(
        "image_refusal",
        &[("model", model), ("modalities", listed.as_str())],
    )
}

#[derive(Debug, Clone)]
pub struct PythonConfig {
    /// Explicit interpreter. `None` means `python3`, then `python`, on PATH.
    pub program: Option<PathBuf>,
    /// Arguments before the driver: `uv run python` uses `["run", "python"]`.
    pub args: Vec<String>,
    /// How long a `python` call waits before the interpreter is stopped.
    pub timeout: Duration,
}

/// What every tool call shares.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    /// The longest answer one tool call returns, in characters; `0` is no cap.
    ///
    /// # Invariant
    ///
    /// One cap for every call, whatever tool answered it: a tool that capped as well
    /// would be a second answer to the same question, and the smaller would win. A
    /// call's own `max_output` overrides this for that call; see `tools::laws`.
    pub max_output_chars: usize,
    /// Every tool this home declares, in the order the names sort.
    ///
    /// # Invariant
    ///
    /// The one table: what the model is offered, what the operator may type and what
    /// this build carries are all read from here. See
    /// `docs/notes/server-tools-spec.md`.
    pub rows: std::collections::BTreeMap<String, crate::tools::spec::ToolSpec>,
    /// Every program `exec` knows: what it starts with, and how a reply is framed.
    ///
    /// # Invariant
    ///
    /// A name absent here is a program started with the call's own words: the table
    /// says what is *not* raw, and nothing else.
    pub channels: std::collections::BTreeMap<String, crate::tools::spec::ChannelSpec>,
}

/// What a conversation with a peer costs when the peer is silent.
///
/// # Invariant
///
/// Not a wait, and not a budget: this is the network clock, and it ends a request
/// with an error. What bounds a call is its own wait, which this number never
/// touches. See `docs/notes/server-remote.md`.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// The longest a peer may stay silent, at its greeting and at every answer.
    pub answer_timeout: Duration,
}

/// Where commands registered by Python scripts live.
#[derive(Debug, Clone)]
pub struct ScriptConfig {
    /// The directory scanned for `*.py`; defaults to `<home>/scripts`.
    pub dir: PathBuf,
    /// Explicit interpreter. `None` means detect one that answers `-V`.
    pub program: Option<PathBuf>,
}

impl ScriptConfig {
    /// The interpreter to run scripts with: the configured one, else the first
    /// candidate that is really Python.
    ///
    /// # Invariant
    ///
    /// A name on `PATH` is not enough: Windows ships a `python3` stub that
    /// prints an install advert and fails, so the candidate has to answer.
    pub fn interpreter(&self) -> Option<PathBuf> {
        if let Some(explicit) = &self.program {
            return explicit.is_file().then(|| explicit.clone());
        }
        detect_python()
    }
}

/// The first `python3`/`python` that answers `-V` with a version line.
pub fn detect_python() -> Option<PathBuf> {
    for name in ["python3", "python"] {
        let Ok(candidate) = which::which(name) else {
            continue;
        };
        let Ok(out) = std::process::Command::new(&candidate).arg("-V").output() else {
            continue;
        };
        let said = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        if out.status.success() && said.starts_with("Python") {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Input tokens above which the agent is told to fold its own context.
    pub compress_above_tokens: u64,
    /// Whether the agent is asked to write checkpoint summaries; the opt-in is a
    /// switch of its own, never a token count.
    pub live_compact_enabled: bool,
    /// Input-token growth between agent-written checkpoints; only the threshold an
    /// enabled live compact measures against.
    pub live_compact_interval_tokens: u64,
    /// Whether checkpoints cut the provider projection or declare a memory view.
    pub live_compact_mode: crate::live_compact::CompactMode,
    /// Whether the head carries the one-shot meta plan; off keeps it minimal.
    pub prefix_cache_plan: bool,
    /// The chance a cached prefix is gone by the next request, `0.0..=1.0`.
    pub cache_invalid_rate: f64,
    /// A lifetime for a cached prefix, in seconds, for every provider at once; `0`
    /// keeps the roster's own numbers.
    pub cache_ttl_secs: u64,
    /// The share of a planned entry's lines that may change and still be patched.
    pub plan_change_tolerance: f64,
    /// Share of the window a `--context_limit` percentage defaults to.
    pub context_limit_percent: u8,
}

/// The on-disk shape. Every field is optional so a config file can be tiny.
#[derive(Debug, Default, Deserialize, Serialize)]
struct FileConfig {
    server: Option<FileServer>,
    /// `[model]`; `[provider]` is where this table used to live.
    #[serde(alias = "provider")]
    model: Option<FileModel>,
    tools: Option<FileTools>,
    /// `[tool.<name>]`: one row per tool this home declares. See `server-tools-spec`.
    tool: Option<std::collections::BTreeMap<String, crate::tools::spec::ToolSpec>>,
    /// `[channel.<name>]`: what a program is, as `exec` reads it. See `server-channel`.
    channel: Option<std::collections::BTreeMap<String, crate::tools::spec::ChannelSpec>>,
    /// Where the rows used to live. Read for nothing, so an old home is not refused.
    tool_text: Option<toml::Table>,
    /// Never honoured: `crate::remotes` reads them from the machine.
    #[serde(alias = "targets")]
    remotes: Option<std::collections::BTreeMap<String, crate::remotes::FileRemote>>,
    scripts: Option<FileScripts>,
    ship: Option<FileShip>,
    agent: Option<FileAgent>,
    transport: Option<FileTransport>,
    endpoint: Option<FileEndpoint>,
}

impl FileConfig {
    /// The repository this file names, when it names one.
    ///
    /// # Invariant
    ///
    /// An empty line is the compiled URL, not a repository called nothing.
    fn ship_url(&self) -> Option<String> {
        self.ship
            .as_ref()
            .and_then(|s| s.url.as_deref())
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileServer {
    home: Option<PathBuf>,
    bind: Option<String>,
    key: Option<String>,
    /// Room left below which the disk counts as out of room, in MiB; `0` turns the
    /// reading off.
    disk_low_mb: Option<u64>,
    /// How long one quota endpoint's reading stands, in seconds; `0` asks at every
    /// step's end.
    quota_interval_secs: Option<u64>,
}

/// `[ship]`: where this home fetches content from; a path is a local checkout.
#[derive(Debug, Default, Deserialize, Serialize)]
struct FileShip {
    /// A URL or a path. Which commit is installed is still the protocol's question.
    url: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileModel {
    /// A provider's id, e.g. `deepseek-official`. Optional: naming only a model
    /// name finds the provider that has it.
    provider: Option<String>,
    base_url: Option<String>,
    #[serde(alias = "model")]
    model_name: Option<String>,
    api_key_env: Option<String>,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    /// The face to speak, by the endpoint's label; defaults to the roster's choice.
    api: Option<String>,
    store: Option<bool>,
    cache_tier: Option<String>,
    cache_margin_pct: Option<u8>,
    cache_affinity: Option<bool>,
    session_id_headers: Option<Vec<String>>,
    headers_timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
    max_retries: Option<u32>,
    retry_base_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct FileTools {
    /// The longest answer one tool call returns, in characters; `0` is no cap.
    max_output_chars: Option<usize>,
    /// Where this number used to live. Kept so an older home keeps its value.
    bash: Option<FileLegacyBash>,
    python: Option<FilePython>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct FileLegacyBash {
    max_output_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct FilePython {
    program: Option<PathBuf>,
    args: Option<Vec<String>>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileScripts {
    dir: Option<PathBuf>,
    program: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileAgent {
    compress_above_tokens: Option<u64>,
    live_compact_enabled: Option<bool>,
    live_compact_interval_tokens: Option<u64>,
    live_compact_mode: Option<String>,
    prefix_cache_plan: Option<bool>,
    cache_invalid_rate: Option<f64>,
    cache_ttl_secs: Option<u64>,
    #[serde(alias = "index_change_tolerance")]
    plan_change_tolerance: Option<f64>,
    context_limit_percent: Option<u8>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileEndpoint {
    answer_timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileTransport {
    mtu: Option<usize>,
    window: Option<u16>,
    keepalive_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
}

/// How many times this process has read a home's config, so each value carries the
/// name of the read it came from.
static READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Config {
    /// Loads the config, creating the home directory if it does not exist.
    ///
    /// # Invariant
    ///
    /// A flag beats the file, which beats the environment; everything downstream
    /// -- db, logs, job records, the instance lock -- keys off `home`.
    ///
    /// See `docs/notes/server-config.md`.
    pub fn load_with_home(
        config_path: Option<&Path>,
        home_override: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let named = home_override.map(|path| expand(path.to_path_buf()));
        let mut home = named.clone().unwrap_or_else(home_dir);
        // The content's own file; `--config` names one instead.
        let (config_path, inside) = match config_path {
            Some(path) => (path.to_path_buf(), false),
            None => (crate::ship::config(&home), true),
        };
        let file = if config_path.exists() {
            read_file(&config_path)?
        } else if inside {
            anyhow::bail!(
                "this home's config is the content's own file, and {} is not there: the                  checkout at {} holds no content to read",
                config_path.display(),
                crate::ship::checkout(&home).display()
            );
        } else {
            FileConfig::default()
        };
        // An empty line is the compiled URL, not a repository called nothing: read it
        // before any field of `file` is moved.
        let ship_url = file.ship_url();
        // These belong to the machine, not to the merged file.
        let stranded = crate::remotes::stranded(&config_path);
        if !stranded.is_empty() {
            tracing::warn!(
                file = %config_path.display(),
                remotes = %stranded.join(", "),
                "these remotes are in config.toml, which this build no longer reads for \
                 them: they belong to ~/.agents/remotes.toml -- move them with the \
                 migrator in <home>/migrate/3-to-4.py"
            );
        }

        // A config inside the home cannot move the home it is read from.
        if named.is_none()
            && let Some(stated) = file
                .server
                .as_ref()
                .and_then(|s| s.home.clone())
                .map(expand)
        {
            if inside {
                if std::path::absolute(&stated).ok() != std::path::absolute(&home).ok() {
                    tracing::warn!(
                        config = %config_path.display(),
                        home = %home.display(),
                        stated = %stated.display(),
                        "[server] home is not read: this config is already inside the home it                          names, so the home is where the file is"
                    );
                }
            } else {
                home = stated;
            }
        }
        std::fs::create_dir_all(&home)?;
        // Absolute from here on: a child in another directory (script, shell)
        // must resolve the same home.
        let home = std::path::absolute(&home).unwrap_or(home);

        // The home owns its database: no flag names it elsewhere, so a scratch
        // home cannot drive a real one.
        let db_path = home.join(DEFAULT_DB);

        let bind: SocketAddr = env_or("GONGGA_BIND")
            .or_else(|| file.server.as_ref().and_then(|s| s.bind.clone()))
            .unwrap_or_else(|| DEFAULT_BIND.to_string())
            .parse()?;

        // No key means anyone on the network drives this shell: mint one into
        // `server.key`, not into the merged config.toml.
        let key_path = home.join("server.key");
        let key = env_or("GONGGA_KEY")
            .or_else(|| read_key_file(&key_path))
            .or_else(|| file.server.as_ref().and_then(|s| s.key.clone()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                let generated = generate_key();
                if let Err(e) = persist_key(&key_path, &generated) {
                    tracing::warn!(file = %key_path.display(), error = %e, "could not save the generated key; it is only valid for this run");
                }
                generated
            });

        let model_file = file.model.unwrap_or_default();
        // The roster is the machine's own provider list; using it needs no config.
        let roster = crate::providers::roster();
        let provider = env_or("GONGGA_PROVIDER").or(model_file.provider.clone());
        let model_name = env_or("GONGGA_MODEL").or(model_file.model_name.clone());
        let entry = pick_model(&roster, provider.clone(), model_name.clone()).or_else(|| {
            // A script may name the model the file does not: ask it before the default.
            pick_model(&crate::providers::settle_scripts(), provider, model_name)
        });
        let api_key_env = env_or("GONGGA_API_KEY_ENV")
            .or_else(|| model_file.api_key_env.clone())
            .or_else(|| entry.as_ref().and_then(|e| e.api_key_env.clone()))
            .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string());
        let api_key = env_or("GONGGA_API_KEY")
            .or_else(|| std::env::var(&api_key_env).ok())
            // The roster's own key is the most specific source, so it comes first.
            .or_else(|| {
                entry
                    .as_ref()
                    .and_then(|e| e.key.clone())
                    .filter(|k| !k.is_empty())
            })
            .unwrap_or_default();

        let model = ModelConfig {
            provider: entry.as_ref().map(|e| e.provider.clone()),
            model_name: env_or("GONGGA_MODEL")
                .or(model_file.model_name)
                .or_else(|| entry.as_ref().map(|e| e.model_name.clone()))
                .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_string()),
            base_url: env_or("GONGGA_BASE_URL")
                .or(model_file.base_url)
                .or_else(|| entry.as_ref().map(|e| e.base_url.clone()))
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            context: entry.as_ref().and_then(|e| e.context),
            api_key_env,
            api_key,
            measure: None,
            price: None,
            cache_tier: env_or("GONGGA_CACHE_TIER").or(model_file.cache_tier),
            cache_tiers: entry
                .as_ref()
                .map(|e| e.cache_ttl_tiers.clone())
                .unwrap_or_default(),
            cache_margin_pct: env_or("GONGGA_CACHE_MARGIN_PCT")
                .and_then(|v| v.parse().ok())
                .or(model_file.cache_margin_pct)
                .unwrap_or(5)
                .min(50),
            // A fact about the provider, from the roster, unless this home overrides it.
            cache_affinity: env_or("GONGGA_CACHE_AFFINITY")
                .and_then(|v| v.parse().ok())
                .or(model_file.cache_affinity)
                .or_else(|| entry.as_ref().and_then(|e| e.cache_affinity))
                .unwrap_or(false),
            session_id_headers: env_or("GONGGA_SESSION_ID_HEADERS")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .or(model_file.session_id_headers)
                .or_else(|| entry.as_ref().map(|e| e.session_id_headers.clone()))
                .unwrap_or_default(),
            temperature: model_file.temperature,
            // The roster states it per model when the provider requires one; a home
            // may override what the face asks for.
            max_output_tokens: model_file
                .max_output_tokens
                .or_else(|| entry.as_ref().and_then(|e| e.max_output_tokens)),
            api: model_file
                .api
                .clone()
                .or_else(|| entry.as_ref().map(|e| e.api.clone()))
                .unwrap_or_else(|| "openai-responses".to_string()),
            store: model_file.store.unwrap_or(false),
            headers_timeout: Duration::from_millis(
                model_file
                    .headers_timeout_ms
                    .or_else(|| {
                        env_or("GONGGA_PROVIDER_HEADERS_TIMEOUT_MS").and_then(|v| v.parse().ok())
                    })
                    .unwrap_or(180_000),
            ),
            idle_timeout: Duration::from_millis(
                model_file
                    .idle_timeout_ms
                    .or_else(|| {
                        env_or("GONGGA_PROVIDER_IDLE_TIMEOUT_MS").and_then(|v| v.parse().ok())
                    })
                    .unwrap_or(60_000),
            ),
            max_retries: model_file
                .max_retries
                .or_else(|| env_or("GONGGA_PROVIDER_MAX_RETRIES").and_then(|v| v.parse().ok()))
                // 0 is no ceiling: retry until the turn is cancelled.
                .unwrap_or(0),
            retry_base: Duration::from_millis(
                model_file
                    .retry_base_ms
                    .or_else(|| {
                        env_or("GONGGA_PROVIDER_RETRY_BASE_MS").and_then(|v| v.parse().ok())
                    })
                    .unwrap_or(1_000),
            ),
            builtin_tools: entry
                .as_ref()
                .map(|e| e.builtin_tools.clone())
                .unwrap_or_default(),
            modalities: entry
                .as_ref()
                .map(|e| e.modalities.clone())
                .unwrap_or_default(),
        };

        let tools_file = file.tools.clone().unwrap_or_default();

        let python_file = tools_file.python.unwrap_or_default();
        let python = PythonConfig {
            program: env_path("GONGGA_PYTHON")
                .or(python_file.program)
                .or_else(detect_python),
            args: python_file.args.unwrap_or_default(),
            timeout: Duration::from_millis(
                python_file
                    .timeout_ms
                    .or_else(|| env_or("GONGGA_PYTHON_TIMEOUT_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(120_000),
            ),
        };

        let tools = ToolsConfig {
            max_output_chars: tools_file
                .max_output_chars
                .or_else(|| {
                    // One cap now: an old home's number is that cap. Dropping it would
                    // change what every call returns, silently.
                    let legacy = tools_file
                        .bash
                        .as_ref()
                        .and_then(|bash| bash.max_output_chars);
                    if let Some(max) = legacy {
                        tracing::warn!(
                            max_output_chars = max,
                            "`[tools.bash] max_output_chars` is `[tools] max_output_chars` now: \
                             one cap for every call, not one per tool"
                        );
                    }
                    legacy
                })
                .or_else(|| env_or("GONGGA_MAX_OUTPUT_CHARS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS),
            rows: file.tool.unwrap_or_default(),
            channels: file.channel.unwrap_or_default(),
        };

        let scripts_file = file.scripts.unwrap_or_default();
        let scripts = ScriptConfig {
            dir: scripts_file
                .dir
                .map(expand)
                .unwrap_or_else(|| home.join("scripts")),
            program: env_path("GONGGA_SCRIPT_PYTHON").or(scripts_file.program),
        };

        let compact_mode = file
            .agent
            .as_ref()
            .and_then(|agent| agent.live_compact_mode.clone())
            .or_else(|| env_or("GONGGA_LIVE_COMPACT_MODE"));
        let compact_mode = match compact_mode.as_deref() {
            None => crate::live_compact::CompactMode::Cut,
            Some("cut") => crate::live_compact::CompactMode::Cut,
            Some("mask") => crate::live_compact::CompactMode::Mask,
            Some(other) => anyhow::bail!("live_compact_mode must be cut or mask, not {other:?}"),
        };
        let agent = AgentConfig {
            compress_above_tokens: file
                .agent
                .as_ref()
                .and_then(|a| a.compress_above_tokens)
                .or_else(|| env_or("GONGGA_COMPRESS_ABOVE_TOKENS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_COMPRESS_ABOVE_TOKENS),
            live_compact_enabled: file
                .agent
                .as_ref()
                .and_then(|a| a.live_compact_enabled)
                .or_else(|| env_or("GONGGA_LIVE_COMPACT_ENABLED").and_then(|v| v.parse().ok()))
                .unwrap_or(false),
            live_compact_interval_tokens: file
                .agent
                .as_ref()
                .and_then(|a| a.live_compact_interval_tokens)
                .or_else(|| {
                    env_or("GONGGA_LIVE_COMPACT_INTERVAL_TOKENS").and_then(|v| v.parse().ok())
                })
                .unwrap_or(DEFAULT_LIVE_COMPACT_INTERVAL_TOKENS),
            live_compact_mode: compact_mode,
            prefix_cache_plan: file
                .agent
                .as_ref()
                .and_then(|a| a.prefix_cache_plan)
                .or_else(|| env_or("GONGGA_PREFIX_CACHE_PLAN").and_then(|v| v.parse().ok()))
                .unwrap_or(true),
            cache_invalid_rate: file
                .agent
                .as_ref()
                .and_then(|a| a.cache_invalid_rate)
                .or_else(|| env_or("GONGGA_CACHE_INVALID_RATE").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_CACHE_INVALID_RATE)
                .clamp(0.0, 1.0),
            cache_ttl_secs: file
                .agent
                .as_ref()
                .and_then(|a| a.cache_ttl_secs)
                .or_else(|| env_or("GONGGA_CACHE_TTL_SECS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_CACHE_TTL_SECS),
            plan_change_tolerance: file
                .agent
                .as_ref()
                .and_then(|a| a.plan_change_tolerance)
                .or_else(|| env_or("GONGGA_PLAN_CHANGE_TOLERANCE").and_then(|v| v.parse().ok()))
                .or_else(|| env_or("GONGGA_INDEX_CHANGE_TOLERANCE").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_PLAN_CHANGE_TOLERANCE)
                .clamp(0.0, 1.0),
            // Above 100 would put the notice past the window it is a share of.
            context_limit_percent: file
                .agent
                .as_ref()
                .and_then(|a| a.context_limit_percent)
                .or_else(|| env_or("GONGGA_CONTEXT_LIMIT_PERCENT").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_CONTEXT_LIMIT_PERCENT)
                .min(100),
        };
        // Zero would ask about every step; the opt-in must not double as the interval.
        if agent.live_compact_enabled && agent.live_compact_interval_tokens == 0 {
            anyhow::bail!(
                "live_compact is enabled but live_compact_interval_tokens is 0; the interval \
                 is the threshold, not the switch -- set a positive interval or disable \
                 live_compact"
            );
        }

        let transport = file.transport.unwrap_or_default();
        let transport = TransportConfig {
            mtu: transport.mtu.unwrap_or(1200),
            window: transport.window.unwrap_or(64),
            keepalive: transport
                .keepalive_ms
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_secs(2)),
            idle_timeout: transport
                .idle_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_secs(60)),
        };

        let endpoint_file = file.endpoint.unwrap_or_default();
        let endpoint = EndpointConfig {
            answer_timeout: Duration::from_millis(
                endpoint_file
                    .answer_timeout_ms
                    .or_else(|| env_or("GONGGA_ANSWER_TIMEOUT_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(DEFAULT_ANSWER_TIMEOUT_MS),
            ),
        };

        let disk_low = file
            .server
            .as_ref()
            .and_then(|s| s.disk_low_mb)
            .or_else(|| env_or("GONGGA_DISK_LOW_MB").and_then(|v| v.parse().ok()))
            .unwrap_or(DEFAULT_DISK_LOW_MB)
            .saturating_mul(1024 * 1024);

        let quota_interval = Duration::from_secs(
            file.server
                .as_ref()
                .and_then(|s| s.quota_interval_secs)
                .or_else(|| env_or("GONGGA_QUOTA_INTERVAL_SECS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_QUOTA_INTERVAL_SECS),
        );

        Ok(Self {
            read: READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            config_path: std::path::absolute(&config_path).unwrap_or(config_path),
            home,
            db_path,
            bind,
            key,
            model,
            tools,
            python,
            scripts,
            agent,
            transport,
            endpoint,
            disk_low,
            quota_interval,
            ship_url,
        })
    }
}

/// Picks the model the server should talk to.
///
/// # Invariant
///
/// A model named in full (`provider/model_name`) is that entry whatever else is
/// said; otherwise a provider and a model name are its two halves, a model name
/// alone finds the provider that carries it, and nothing named takes the first
/// usable entry. A name the roster cannot place warns and falls back rather than
/// failing. See `docs/notes/server-config.md`.
fn pick_model(
    roster: &crate::providers::Catalog,
    provider: Option<String>,
    model: Option<String>,
) -> Option<crate::providers::Entry> {
    fn place(roster: &crate::providers::Catalog, model: &str) -> Option<crate::providers::Entry> {
        match roster.entry(model) {
            Ok(entry) => Some(entry),
            Err(e) => {
                tracing::warn!(model = %model, error = %e, "model: the roster cannot place this; using the built-in default");
                None
            }
        }
    }
    match (provider, model) {
        // The model as a caller names it, provider and all.
        (_, Some(whole)) if whole.contains('/') => place(roster, &whole),
        (Some(p), Some(m)) => place(roster, &format!("{p}/{m}")),
        (Some(p), None) => match roster.first_entry_of(&p) {
            Some(entry) => Some(entry),
            None => {
                tracing::warn!(provider = %p, "provider: no such provider in the roster; using the built-in default");
                None
            }
        },
        (None, Some(m)) => match roster.entry_by_name(&m) {
            Some(entry) => Some(entry),
            None => {
                tracing::warn!(model = %m, "model: no provider in the roster has this name; using the built-in default");
                None
            }
        },
        (None, None) => roster.default_entry(),
    }
}

impl ModelConfig {
    /// The whole model, `provider/model_name`, or the bare name when unplaced.
    pub fn model(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{provider}/{}", self.model_name),
            None => self.model_name.clone(),
        }
    }

    /// The same timeouts and retries, pointed at another model.
    ///
    /// See `docs/notes/server-config.md`.
    pub fn for_entry(&self, entry: &crate::providers::Entry, api_key: String) -> Self {
        let mut cfg = self.clone();
        cfg.provider = Some(entry.provider.clone());
        cfg.base_url = entry.base_url.clone();
        cfg.model_name = entry.model_name.clone();
        cfg.context = entry.context;
        // The face is the provider's, and so is the length that face must state.
        cfg.api = entry.api.clone();
        cfg.max_output_tokens = entry.max_output_tokens;
        cfg.api_key_env = entry.api_key_env.clone().unwrap_or_default();
        cfg.api_key = api_key;
        // Capabilities follow the model: another entry keeps its own tools and eyes.
        cfg.builtin_tools = entry.builtin_tools.clone();
        cfg.modalities = entry.modalities.clone();
        // And so does what it charges: the plan is priced by the model in use.
        cfg.price = entry.price;
        // A tier belongs to the provider, not to the machine's default model.
        cfg.cache_tiers = entry.cache_ttl_tiers.clone();
        // Session affinity is the provider's fact too: the default model's list
        // must not be what a different provider sends.
        cfg.cache_affinity = entry.cache_affinity.unwrap_or(false);
        cfg.session_id_headers = entry.session_id_headers.clone();
        cfg
    }
}

/// The home's `config.toml` as this process follows it.
///
/// # Invariant
///
/// Everything a request reads comes from [`Current::now`]: the file is the machine's,
/// and changing a model, a timeout or a shell is an edit, never a restart. What
/// placed this process -- `home`, `db_path`, `bind`, `key` -- is
/// [`Current::anchor`], read once, because a database and a listening socket do not
/// move. See `docs/notes/server-config.md`.
pub struct Current {
    config_path: PathBuf,
    home: Option<PathBuf>,
    live: crate::live::Live<std::sync::Arc<Config>>,
}

impl Current {
    /// Reads the config once, and follows it from then on.
    pub fn load(
        config_path: Option<&Path>,
        home: Option<&Path>,
    ) -> anyhow::Result<std::sync::Arc<Self>> {
        let config = Config::load_with_home(config_path, home)?;
        Ok(std::sync::Arc::new(Self {
            live: crate::live::Live::of(
                vec![config.config_path.clone()],
                std::sync::Arc::new(config.clone()),
            ),
            config_path: config.config_path,
            home: home.map(Path::to_path_buf),
        }))
    }

    /// The config the file has now: a value that did not change is the same `Arc`.
    pub fn now(&self) -> std::sync::Arc<Config> {
        let (config_path, home) = (self.config_path.clone(), self.home.clone());
        self.live.current(|| {
            Config::load_with_home(Some(&config_path), home.as_deref()).map(std::sync::Arc::new)
        })
    }

    /// The config this process started with.
    pub fn anchor(&self) -> std::sync::Arc<Config> {
        self.live.initial()
    }
}

/// A 128-bit random key, hex encoded.
fn generate_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Writes the key as the whole contents of its own file.
fn persist_key(path: &Path, key: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{key}\n"))?;
    Ok(())
}

/// The key file, trimmed; no file and an empty file both mean "not here", so
/// the next source still gets its turn.
fn read_key_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn read_file(path: &Path) -> anyhow::Result<FileConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
}

/// The repository a config file names, when it names one.
///
/// # Invariant
///
/// Read without loading the config, for the one caller that cannot resolve its
/// content: the mapping that would name a different file lives inside the content it
/// cannot read, so this asks the file at the name this build has always used. See
/// `docs/notes/server-ship.md`.
pub fn ship_url_of(path: &Path) -> Option<String> {
    read_file(path).ok()?.ship_url()
}

/// The home this program has when nobody names one: `GONGGA_HOME`, else `~/.gongga`.
///
/// # Invariant
///
/// The one definition of it, so a runner on a peer keeps its channels where a
/// server on that machine would. See `docs/notes/server-channel.md`.
pub(crate) fn home_dir() -> PathBuf {
    env_path("GONGGA_HOME")
        .or_else(|| dirs::home_dir().map(|h| h.join(".gongga")))
        .unwrap_or_else(|| PathBuf::from(".gongga"))
}

fn env_or(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_path(key: &str) -> Option<PathBuf> {
    env_or(key).map(PathBuf::from)
}

/// What the operator typed, as a directory this process and the ones it starts agree on.
///
/// # Invariant
///
/// Absolute: a relative path is meant against the directory it was typed in, and a
/// child -- relay, runner or skill -- reads `GONGGA_HOME` in its own.
pub(crate) fn expand(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy().to_string();
    let expanded = match s.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => path,
        },
        None => path,
    };
    if expanded.is_absolute() {
        return expanded;
    }
    std::path::absolute(&expanded).unwrap_or(expanded)
}

pub(crate) fn detect_git_bash() -> Option<PathBuf> {
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
            r"C:\SoftWare\git\Git\bin\bash.exe",
        ];
        for c in candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(shell_program(p));
            }
        }
    }
    which::which("bash").ok().map(shell_program)
}

/// The real shell behind a launcher: spawning the launcher would record a pid that works on nothing.
pub(crate) fn shell_program(program: PathBuf) -> PathBuf {
    let Some(name) = program.file_name() else {
        return program;
    };
    let Some(bin) = program.parent() else {
        return program;
    };
    if bin.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return program;
    }
    let Some(root) = bin.parent() else {
        return program;
    };
    let real = root.join("usr").join("bin").join(name);
    if real.is_file() { real } else { program }
}
