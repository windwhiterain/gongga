//! Configuration resolution: config file, then environment, then defaults.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transport::TransportConfig;

pub const DEFAULT_BIND: &str = "127.0.0.1:9700";
pub const DEFAULT_DB: &str = "gongga.db";

/// Default provider: MiniMax's China endpoint, OpenAI-compatible Responses API.
pub const DEFAULT_BASE_URL: &str = "https://api.minimax.cn/v1";
pub const DEFAULT_MODEL: &str = "MiniMax-M3.1-Flash";
pub const DEFAULT_API_KEY_ENV: &str = "MINIMAX_CN_API_KEY";
/// Tool output larger than this is capped and spilled to disk, keeping `cat`s out
/// of later requests.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 8_000;
/// Input-token ceiling that triggers the context notice; fallback with no roster window.
pub const DEFAULT_COMPRESS_ABOVE_TOKENS: u64 = 40_000;

/// Everything the server needs to boot.
#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub db_path: PathBuf,
    pub bind: SocketAddr,
    /// Shared secret a client must present before it may drive the harness.
    pub key: String,
    pub llm: LlmConfig,
    pub bash: BashConfig,
    pub agent: AgentConfig,
    pub transport: TransportConfig,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Roster provider id this configuration points at; only used in messages.
    pub provider: Option<String>,
    /// Base URL of an OpenAI-compatible Responses API. Defaults to MiniMax CN.
    pub base_url: String,
    pub model: String,
    /// Context window of `model`; `--compact_threshold` is a percentage of it.
    pub context: Option<u64>,
    /// Name of the environment variable holding the API key.
    pub api_key_env: String,
    /// Resolved key. Empty when the variable is unset.
    pub api_key: String,
    /// System instructions sent as `instructions` on every request.
    pub instructions: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    /// Ask the API not to retain server-side state; we replay the transcript.
    pub store: bool,
    /// Wall-clock ceiling on one request, streaming included.
    ///
    /// # Invariant
    ///
    /// Past it the turn fails and can be retried, rather than waiting forever on
    /// a socket that never finishes. See `docs/notes/server-config.md`.
    pub request_timeout: Duration,
    /// Silence between chunks that counts as a broken stream, kept below
    /// `request_timeout` on purpose.
    pub idle_timeout: Duration,
    /// Extra attempts after a failure or an empty response; 0 means one attempt.
    pub max_retries: u32,
    /// First retry delay; doubles per attempt up to the [`MAX_RETRY_DELAY`] cap.
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

/// Why an image cannot be sent to `model`, in the roster's own words.
pub fn image_refusal(model: &str, modalities: &[String]) -> String {
    let listed = modalities
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "this model cannot look at images: the roster lists modalities = [{listed}] for {model}, with no \"image\" in it. Use a model whose roster entry includes \"image\" (set_session --llm <provider>/<model>), or leave the image out."
    )
}

#[derive(Debug, Clone)]
pub struct BashConfig {
    /// Explicit shell path. `None` means auto-detect Git Bash.
    pub program: Option<PathBuf>,
    /// How long a `bash` call waits before returning.
    ///
    /// # Invariant
    ///
    /// Not a budget on the command: one still running when this passes keeps
    /// running and posts its output when it finishes.
    pub timeout: Duration,
    /// Tool output longer than this is capped: head and tail only, all of it on disk.
    pub max_output_chars: usize,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Input tokens above which the agent is told to fold its own context.
    pub compress_above_tokens: u64,
}

/// The on-disk shape. Every field is optional so a config file can be tiny.
#[derive(Debug, Default, Deserialize, Serialize)]
struct FileConfig {
    server: Option<FileServer>,
    llm: Option<FileLlm>,
    tools: Option<FileTools>,
    agent: Option<FileAgent>,
    transport: Option<FileTransport>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileServer {
    home: Option<PathBuf>,
    db: Option<PathBuf>,
    bind: Option<String>,
    key: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileLlm {
    /// Roster provider id, e.g. `deepseek-official`. Optional: naming only a
    /// model finds the provider that has it.
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
    instructions: Option<String>,
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    store: Option<bool>,
    request_timeout_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
    max_retries: Option<u32>,
    retry_base_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileTools {
    bash: Option<FileBash>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileBash {
    program: Option<PathBuf>,
    timeout_ms: Option<u64>,
    max_output_chars: Option<usize>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileAgent {
    compress_above_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct FileTransport {
    mtu: Option<usize>,
    window: Option<u16>,
    keepalive_ms: Option<u64>,
    idle_timeout_ms: Option<u64>,
}

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
        let home_override = home_override.map(Path::to_path_buf);
        let config_path = match config_path {
            Some(p) => p.to_path_buf(),
            None => home_override
                .clone()
                .unwrap_or_else(home_dir)
                .join("config.toml"),
        };
        let file = if config_path.exists() {
            read_file(&config_path)?
        } else {
            FileConfig::default()
        };

        let home = home_override.unwrap_or_else(|| {
            file.server
                .as_ref()
                .and_then(|s| s.home.clone())
                .map(expand)
                .unwrap_or_else(home_dir)
        });
        std::fs::create_dir_all(&home)?;

        let db_path = file
            .server
            .as_ref()
            .and_then(|s| s.db.clone())
            .map(expand)
            .unwrap_or_else(|| home.join(DEFAULT_DB));

        let bind: SocketAddr = env_or("GONGGA_BIND")
            .or_else(|| file.server.as_ref().and_then(|s| s.bind.clone()))
            .unwrap_or_else(|| DEFAULT_BIND.to_string())
            .parse()?;

        // No key means anyone on the network drives this shell; mint one and store it.
        let key = env_or("GONGGA_KEY")
            .or_else(|| file.server.as_ref().and_then(|s| s.key.clone()))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                let generated = generate_key();
                if let Err(e) = persist_key(&config_path, &generated) {
                    tracing::warn!(error = %e, "could not save the generated key; it is only valid for this run");
                }
                generated
            });

        let llm_file = file.llm.unwrap_or_default();
        // The roster is the machine's own provider list; using it needs no config.
        let roster = crate::providers::roster();
        let entry = pick_entry(
            roster,
            env_or("GONGGA_PROVIDER").or(llm_file.provider.clone()),
            env_or("GONGGA_MODEL").or(llm_file.model.clone()),
        );
        let api_key_env = env_or("GONGGA_API_KEY_ENV")
            .or_else(|| llm_file.api_key_env.clone())
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

        let llm = LlmConfig {
            provider: entry.as_ref().map(|e| e.provider.clone()),
            base_url: env_or("GONGGA_BASE_URL")
                .or(llm_file.base_url)
                .or_else(|| entry.as_ref().map(|e| e.base_url.clone()))
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model: env_or("GONGGA_MODEL")
                .or(llm_file.model)
                .or_else(|| entry.as_ref().map(|e| e.model.clone()))
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            context: entry.as_ref().and_then(|e| e.context),
            api_key_env,
            api_key,
            instructions: llm_file.instructions.unwrap_or_else(default_instructions),
            temperature: llm_file.temperature,
            max_output_tokens: llm_file.max_output_tokens,
            store: llm_file.store.unwrap_or(false),
            request_timeout: Duration::from_millis(
                llm_file
                    .request_timeout_ms
                    .or_else(|| env_or("GONGGA_LLM_TIMEOUT_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(180_000),
            ),
            idle_timeout: Duration::from_millis(
                llm_file
                    .idle_timeout_ms
                    .or_else(|| env_or("GONGGA_LLM_IDLE_TIMEOUT_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(60_000),
            ),
            max_retries: llm_file
                .max_retries
                .or_else(|| env_or("GONGGA_LLM_MAX_RETRIES").and_then(|v| v.parse().ok()))
                // 0 is no ceiling: retry until the turn is cancelled.
                .unwrap_or(0),
            retry_base: Duration::from_millis(
                llm_file
                    .retry_base_ms
                    .or_else(|| env_or("GONGGA_LLM_RETRY_BASE_MS").and_then(|v| v.parse().ok()))
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

        let bash_file = file.tools.and_then(|t| t.bash).unwrap_or_default();
        let bash = BashConfig {
            program: env_path("GONGGA_BASH")
                .or(bash_file.program)
                .map(shell_program)
                .or_else(detect_git_bash),
            timeout: Duration::from_millis(
                bash_file
                    .timeout_ms
                    .or_else(|| env_or("GONGGA_BASH_TIMEOUT_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(120_000),
            ),
            max_output_chars: bash_file
                .max_output_chars
                .or_else(|| env_or("GONGGA_BASH_MAX_OUTPUT_CHARS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS),
        };

        let agent = AgentConfig {
            compress_above_tokens: file
                .agent
                .as_ref()
                .and_then(|a| a.compress_above_tokens)
                .or_else(|| env_or("GONGGA_COMPRESS_ABOVE_TOKENS").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_COMPRESS_ABOVE_TOKENS),
        };

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

        Ok(Self {
            home,
            db_path,
            bind,
            key,
            llm,
            bash,
            agent,
            transport,
        })
    }

    /// Writes a commented starter config if none exists yet.
    ///
    /// # Invariant
    ///
    /// A file the reader has touched is never rewritten: "untouched" means the
    /// key and nothing else, which is all a first start leaves behind. Anything
    /// else in the file is the reader's, and the template is not worth an edit.
    /// See `docs/notes/server-config.md`.
    pub fn write_default_if_absent(&self) -> anyhow::Result<()> {
        let path = self.home.join("config.toml");
        if path.exists() && !is_key_only(&path)? {
            return Ok(());
        }
        let text = format!(
            "# gongga server configuration\n\
             # Every value can be overridden by a GONGGA_* environment variable.\n\
             \n\
             [server]\n\
             bind = \"{bind}\"\n\
             db = \"{db}\"\n\
             key = \"{key}\"\n\
             \n\
             [llm]\n\
             # Which provider to talk to. These are left commented out on purpose:\n\
             # the roster at ~/.agents/llm.toml already says what this machine can\n\
             # reach, and the first entry that works is a better default than a\n\
             # name frozen into this file. Uncomment to override it.\n\
             # provider = \"{provider}\"\n\
             # model = \"{model}\"\n\
             # base_url = \"{base_url}\"\n\
             # api_key_env = \"{api_key_env}\"\n\
             # Capabilities are NOT configured here: they belong to the model, so\n\
             # they come from its entry in the roster (~/.agents/llm.toml):\n\
             #\n\
             #   [[provider.model]]\n\
             #   id = \"some-model\"\n\
             #   # What it takes in and gives back. A list without \"image\" makes\n\
             #   # an attachment be refused by this entry's name instead of letting the\n\
             #   # provider answer with an opaque 400. Omit the list and nothing is\n\
             #   # refused: unstated is not a no.\n\
             #   modalities = [\"text\", \"image\"]\n\
             #   # Tools the provider runs on its own side, spelled the way that\n\
             #   # provider spells them -- \"web_search\", \"web_search_preview\",\n\
             #   # \"web_fetch\", ... Sent verbatim, so no mapping table is needed.\n\
             #   # Omit it and none are offered: offering one a provider does not\n\
             #   # know fails every request of every turn.\n\
             #   builtin_tools = [\"web_search\"]\n\
             store = {store}\n\
             # A request that never answers used to park a turn forever. These\n\
             # bound it: one request may take this long overall, and this long\n\
             # between chunks, before it counts as broken and is retried.\n\
             request_timeout_ms = {request_timeout}\n\
             idle_timeout_ms = {idle_timeout}\n\
             # Retries after a failure, a timeout, or a response with nothing in\n\
             # it. 0 = no ceiling: retry until the turn is cancelled. The delay\n\
             # doubles each time up to 30s: {retry_base}ms, then twice that.\n\
             max_retries = {max_retries}\n\
             retry_base_ms = {retry_base}\n\
             \n\
             [tools.bash]\n\
             timeout_ms = {timeout}\n\
             # Output longer than this is capped: head and tail come back, the\n\
             # whole thing lands in ~/.gongga/outputs/. 0 disables the cap.\n\
             max_output_chars = {max_output_chars}\n\
             \n\
             [agent]\n\
             # No step limit: a turn runs until the work is done.\n\
             # This server does not compact the transcript. When a request carries\n\
             # more than this many input tokens, it injects one message telling the\n\
             # agent so, and the agent folds with the script in\n\
             # .agents/skills/compact/. 0 disables the\n\
             # notice. Only used when the model's context window is not in the\n\
             # roster; otherwise the threshold is a share of the window.\n\
             compress_above_tokens = {compress_above_tokens}\n\
             \n\
             [transport]\n\
             mtu = {mtu}\n\
             window = {window}\n\
             ",
            bind = self.bind,
            db = toml_path(&self.db_path),
            key = self.key,
            provider = self.llm.provider.clone().unwrap_or_default(),
            base_url = self.llm.base_url,
            model = self.llm.model,
            api_key_env = self.llm.api_key_env,
            store = self.llm.store,
            request_timeout = self.llm.request_timeout.as_millis(),
            idle_timeout = self.llm.idle_timeout.as_millis(),
            max_retries = self.llm.max_retries,
            retry_base = self.llm.retry_base.as_millis(),
            timeout = self.bash.timeout.as_millis(),
            max_output_chars = self.bash.max_output_chars,
            compress_above_tokens = self.agent.compress_above_tokens,
            mtu = self.transport.mtu,
            window = self.transport.window,
        );
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Whether a config file holds only the key minted at first start.
fn is_key_only(path: &Path) -> anyhow::Result<bool> {
    let file = read_file(path)?;
    let key_only = file
        .server
        .as_ref()
        .is_some_and(|s| s.key.is_some() && s.home.is_none() && s.db.is_none() && s.bind.is_none());
    Ok(key_only && file.llm.is_none() && file.tools.is_none() && file.agent.is_none())
}

/// Picks the roster entry the server should talk to.
///
/// # Invariant
///
/// A provider and a model name an exact entry, a model alone finds its provider,
/// neither takes the first usable entry; a spec the roster cannot place warns and
/// falls back rather than failing. See `docs/notes/server-config.md`.
fn pick_entry(
    roster: &crate::providers::Catalog,
    provider: Option<String>,
    model: Option<String>,
) -> Option<crate::providers::Entry> {
    match (provider, model) {
        (Some(p), Some(m)) => {
            let spec = format!("{p}/{m}");
            match roster.entry(&spec) {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::warn!(spec = %spec, error = %e, "llm: the roster cannot place this; using the built-in default");
                    None
                }
            }
        }
        (Some(p), None) => match roster.first_entry_of(&p) {
            Some(entry) => Some(entry),
            None => {
                tracing::warn!(provider = %p, "llm: no such provider in the roster; using the built-in default");
                None
            }
        },
        (None, Some(m)) => match roster.find_model_entry(&m) {
            Some(entry) => Some(entry),
            None => {
                tracing::warn!(model = %m, "llm: no provider in the roster has this model; using the built-in default");
                None
            }
        },
        (None, None) => roster.default_entry(),
    }
}

impl LlmConfig {
    /// The same timeouts, retries and instructions, pointed at another entry.
    ///
    /// See `docs/notes/server-config.md`.
    pub fn for_entry(&self, entry: &crate::providers::Entry, api_key: String) -> Self {
        let mut cfg = self.clone();
        cfg.provider = Some(entry.provider.clone());
        cfg.base_url = entry.base_url.clone();
        cfg.model = entry.model.clone();
        cfg.context = entry.context;
        cfg.api_key_env = entry.api_key_env.clone().unwrap_or_default();
        cfg.api_key = api_key;
        // Capabilities follow the model: another entry keeps its own tools and eyes.
        cfg.builtin_tools = entry.builtin_tools.clone();
        cfg.modalities = entry.modalities.clone();
        cfg
    }
}

/// Renders a path as a TOML basic string; `\` becomes `/` so it is no escape.
fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

/// A 128-bit random key, hex encoded.
fn generate_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Writes `key` back into config.toml, preserving everything else in it.
fn persist_key(path: &Path, key: &str) -> anyhow::Result<()> {
    let mut file = if path.exists() {
        read_file(path).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    let mut server = file.server.take().unwrap_or_default();
    server.key = Some(key.to_string());
    file.server = Some(server);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

fn read_file(path: &Path) -> anyhow::Result<FileConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
}

fn home_dir() -> PathBuf {
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

/// Expands a leading `~` and normalises separators.
pub(crate) fn expand(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path
}

fn default_instructions() -> String {
    "You are gongga, a coding agent running on the user's machine.\n\
     \n\
     You have a persistent bash shell. It keeps its working directory and \
     environment between calls, so prefer `cd` followed by relative paths over \
     re-stating absolute paths every time.\n\
     \n\
     Work in small verifiable steps: inspect, change, then re-run whatever \
     proves the change. State what you are about to do before doing it. When a \
     command fails, read the error before retrying.\n\
     Do not ask for confirmation; the user has already authorised bash execution.\n\
     \n\
     You are running inside a gongga server, and you can drive it from bash \
     with the gongga CLI: `$GONGGA_CLI <command>`, e.g. `$GONGGA_CLI cancel` \
     to stop the running turn and shut the server down, or `set_session` / \
     `send_message` / `del_session`. The command is just its name -- no leading \
     `/`, which is the interactive client's input syntax; a `/word` here is \
     only a path to the shell. `$GONGGA_CLI`, `$GONGGA_SERVER` and \
     `$GONGGA_HOME` are already set in your shell, so no flags are needed. \
     Prefer this over killing processes: `cancel` saves the turn so the next \
     server picks it up, whereas killing the server from outside does not.\n\
     \n\
     You can also replace the server you are running on. `cancel` keeps the \
     current turn as a pending task and the next server resumes it, so a swap \
     only has to put a new process in place before the old one leaves. In the \
     the harness repo, `.agents/skills/evolve/SKILL.md` owns it: its script \
     `stage`s (tests, build, launcher), you run that launcher with `bash \
     --detached`, and `swap` checks the successor is really waiting before it \
     sends `cancel`. Do this only after changing the server itself, and let the \
     script decide when it is safe: it refuses to cancel when no successor is \
     waiting, because then nothing would resume the turn.\n\
     \n\
     Do not write a handoff note before a swap. The resume replays the whole \
     transcript, so a note adds nothing; what must be re-measured is the \
     outside world -- the running pid, the port, the pending rows -- because \
     those are not in the transcript and will have changed.\n\nSkills are prose that teaches a procedure this server does not encode as a \
     tool, and they live in .agents/skills/<name>/SKILL.md, relative to the \
     repo. Do not read them all. Each file opens with frontmatter holding a \
     name and a one-line description, so one command lists every skill for two \
     lines each -- cheap enough to run whenever a task feels like it might have \
     a skill:\n\ngrep -H -m3 -E '^(name|description):' .agents/skills/*/SKILL.md\n\nRead a body only when its description matches the task in front of you. A \
     skill may ship a script next to it; run that instead of retyping the \
     commands it already encodes. A session whose working directory is not the \
     repo has no skills -- check before assuming, and do not invent one.\n"
        .to_string()
}

/// Locates Git Bash on Windows, falling back to whatever `bash` is on PATH.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Line continuations hide a stray escape until a model reads the prompt.
    #[test]
    fn the_instructions_are_well_formed() {
        let text = default_instructions();
        assert!(!text.contains("\\n"), "literal backslash-n leaked: {text}");
        assert!(text.contains("\n\nYou can also replace the server"));
        assert!(text.contains(".agents/skills/evolve/SKILL.md"));
        assert!(text.contains("grep -H -m3 -E '^(name|description):'"));
        assert!(text.ends_with("do not invent one.\n"));
        assert!(!text.contains("  "), "double space: {text}");
    }

    #[test]
    fn parses_a_minimal_config() {
        let cfg: FileConfig = toml::from_str("[llm]\nmodel = \"x\"\n").unwrap();
        assert_eq!(cfg.llm.unwrap().model.as_deref(), Some("x"));
        assert!(cfg.server.is_none());
    }

    #[test]
    fn generated_keys_are_distinct_and_hex() {
        let a = generate_key();
        let b = generate_key();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn persisting_a_key_keeps_the_rest_of_the_file() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        std::fs::write(&path, "[llm]\nmodel = \"keep\"\n").unwrap();
        persist_key(&path, "abc").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("model = \"keep\""), "{text}");
        let cfg = read_file(&path).unwrap();
        assert_eq!(cfg.server.unwrap().key.as_deref(), Some("abc"));
    }

    /// The template replaces a first start's key-only file; the fields it teaches matter.
    #[test]
    fn the_template_replaces_a_file_holding_only_the_generated_key() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        std::fs::write(&path, "[server]\nkey = \"abc\"\n").unwrap();
        let cfg = Config::load_with_home(Some(&path), Some(d.path())).unwrap();
        cfg.write_default_if_absent().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("key = \"abc\""), "the key is kept: {text}");
        assert!(
            text.contains("builtin_tools"),
            "the roster fields are taught: {text}"
        );
        assert!(text.contains("modalities"), "{text}");
        assert!(text.contains("[agent]"), "{text}");
    }

    /// A file the reader has edited is left alone, even if it says very little.
    #[test]
    fn the_template_never_rewrites_a_file_someone_edited() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        let written = "[server]\nkey = \"abc\"\nbind = \"127.0.0.1:9999\"\n";
        std::fs::write(&path, written).unwrap();
        let cfg = Config::load_with_home(Some(&path), Some(d.path())).unwrap();
        cfg.write_default_if_absent().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
    }

    /// The four ways a config names a provider, and a typo that must not stop
    /// startup.
    #[test]
    fn the_roster_answers_what_the_config_does_not_say() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("llm.toml");
        std::fs::write(
            &path,
            r#"
[[provider]]
id = "p1"
key = "k1"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "https://one.example/v1"
  [[provider.model]]
  id = "m1"
  context = 1000

[[provider]]
id = "p2"
key = "k2"
  [[provider.endpoint]]
  api = "openai-completions"
  base_url = "https://two.example/v1"
  [[provider.model]]
  id = "m2"
  context = 2000
  endpoints = ["/responses"]
"#,
        )
        .unwrap();
        let roster = crate::providers::Catalog::load(&path);

        // Nothing said: the first entry that works.
        let entry = pick_entry(&roster, None, None).expect("a default exists");
        assert_eq!(entry.spec(), "p1/m1");
        assert_eq!(entry.base_url, "https://one.example/v1");

        // A model alone finds the provider that carries it.
        assert_eq!(
            pick_entry(&roster, None, Some("m2".into())).unwrap().spec(),
            "p2/m2"
        );
        // A provider alone takes its first model.
        assert_eq!(
            pick_entry(&roster, Some("p2".into()), None).unwrap().spec(),
            "p2/m2"
        );
        // Both is exact.
        assert_eq!(
            pick_entry(&roster, Some("p2".into()), Some("m2".into()))
                .unwrap()
                .spec(),
            "p2/m2"
        );
        // A typo is a warning and a fallback, not a refusal to start.
        assert!(pick_entry(&roster, Some("nope".into()), Some("m2".into())).is_none());
        assert!(pick_entry(&roster, None, Some("nope".into())).is_none());
        // An empty roster is simply no opinion.
        assert!(pick_entry(&crate::providers::Catalog::empty(), None, None).is_none());
    }

    #[test]
    fn expands_tilde_paths() {
        let p = expand(PathBuf::from("~/gongga/db.sqlite"));
        assert!(!p.to_string_lossy().starts_with("~"));
    }

    /// A launcher starts the real shell, so the path resolves to that shell.
    #[test]
    fn a_launcher_resolves_to_the_shell_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let usr = dir.path().join("usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&usr).unwrap();
        let launcher = bin.join("bash.exe");
        std::fs::write(&launcher, b"launcher").unwrap();
        // Nothing to resolve to yet: the launcher is the only shell there is.
        assert_eq!(shell_program(launcher.clone()), launcher);
        let real = usr.join("bash.exe");
        std::fs::write(&real, b"shell").unwrap();
        assert_eq!(shell_program(launcher), real);
        // A shell that is not a launcher keeps its path.
        let direct = usr.join("elsewhere.exe");
        assert_eq!(shell_program(direct.clone()), direct);
    }
}
