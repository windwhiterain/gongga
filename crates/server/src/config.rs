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
/// Tool output larger than this is capped and spilled to disk. Sized so a
/// ordinary command still comes back whole while a `cat` of a source file does
/// not smuggle the whole file into every later request.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 8_000;
/// Compress once a single request would carry more than this many input tokens.
/// Only a fallback for a model whose context window the roster does not state;
/// otherwise the threshold is a share of the window itself.
pub const DEFAULT_COMPRESS_ABOVE_TOKENS: u64 = 40_000;
/// Messages kept verbatim after a compression: enough that the model still has
/// the last few steps in full, not just a paraphrase of them.
pub const DEFAULT_KEEP_RECENT: usize = 30;

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
    /// Roster id of the provider this configuration points at, when it points
    /// at one. `None` means the built-in default was used because the machine
    /// has no roster, and it is only used in messages.
    pub provider: Option<String>,
    /// Base URL of an OpenAI-compatible Responses API. Defaults to MiniMax CN.
    pub base_url: String,
    pub model: String,
    /// Context window of `model`, when something told us. `--compact_threshold`
    /// is a percentage of this, so without it a percentage has no meaning.
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
    /// Wall-clock ceiling on one request, streaming included. Without it a
    /// provider that accepts the connection and then goes quiet parks the turn
    /// forever: nothing in the process is waiting on a socket that ever
    /// finishes, and no operator is watching to notice.
    pub request_timeout: Duration,
    /// How long a stream may stay silent between chunks before it counts as
    /// broken. Shorter than `request_timeout` on purpose: a response that is
    /// still arriving is fine, one that stalled is not.
    pub idle_timeout: Duration,
    /// Extra attempts after a request fails, times out, or comes back with no
    /// content at all. 0 means one attempt and then the turn fails.
    pub max_retries: u32,
    /// First retry delay; every further attempt doubles it, capped at
    /// [`MAX_RETRY_DELAY`], so a provider having a bad minute is waited out
    /// rather than hammered.
    pub retry_base: Duration,
}

#[derive(Debug, Clone)]
pub struct BashConfig {
    /// Explicit shell path. `None` means auto-detect Git Bash.
    pub program: Option<PathBuf>,
    /// How long a `bash` call waits before returning.
    ///
    /// Not a budget on the command: a command still running when this passes is
    /// left alone, keeps running, and posts its output to the session when it
    /// finishes. The number decides when the model stops waiting, nothing else.
    pub timeout: Duration,
    /// Tool output longer than this is capped: only the head and the tail come
    /// back to the model, the whole thing is written to disk. Without this a
    /// single `cat` of a source file parks tens of kilobytes in the transcript
    /// and every later step pays for it again.
    pub max_output_chars: usize,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Fold the older transcript into a summary once a request would carry more
    /// than this many input tokens. 0 disables compression, which will
    /// eventually hit the provider's context limit.
    pub compress_above_tokens: u64,
    /// How many of the most recent messages a compression keeps verbatim.
    pub keep_recent: usize,
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
    keep_recent: Option<usize>,
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
    /// A flag beats the file, which beats the environment: `--home` is the most
    /// specific thing the operator typed, and it also decides where the config
    /// is looked for when `--config` is not given. Everything downstream --
    /// database, logs, detached-job records, the instance lock -- already keys
    /// off this one field, which is why there is nothing else to override.
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

        // Without a key anyone on the network could drive this machine's shell,
        // so an unconfigured server mints one and writes it down rather than
        // starting open.
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
        // The roster (`~/.agents/llm.toml`) is the machine's own list of
        // providers, endpoints and model context windows. Consulting it means
        // the common case needs no configuration at all, and it is the same
        // list `--llm` accepts, so there is one answer to "what models exist".
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
            // The roster is the machine's own answer, so it is consulted before
            // the DSH credential store rather than after: a file that names the
            // key is more specific than one that only names a variable.
            .or_else(|| {
                entry
                    .as_ref()
                    .and_then(|e| e.key.clone())
                    .filter(|k| !k.is_empty())
            })
            // Last resort: the credential store DSH keeps on this machine, so a
            // provider key does not have to be exported by hand.
            .or_else(|| dsh_credential(&api_key_env))
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
                // 0 is no ceiling: retry until the turn is cancelled. A long
                // run must not die because a provider had a bad minute at hour
                // three, and nothing about a provider's recovery time is
                // knowable in advance.
                .unwrap_or(0),
            retry_base: Duration::from_millis(
                llm_file
                    .retry_base_ms
                    .or_else(|| env_or("GONGGA_LLM_RETRY_BASE_MS").and_then(|v| v.parse().ok()))
                    .unwrap_or(1_000),
            ),
        };

        let bash_file = file.tools.and_then(|t| t.bash).unwrap_or_default();
        let bash = BashConfig {
            program: env_path("GONGGA_BASH")
                .or(bash_file.program)
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
            keep_recent: file
                .agent
                .as_ref()
                .and_then(|a| a.keep_recent)
                .or_else(|| env_or("GONGGA_KEEP_RECENT").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_KEEP_RECENT),
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
    pub fn write_default_if_absent(&self) -> anyhow::Result<()> {
        let path = self.home.join("config.toml");
        if path.exists() {
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
             # No step limit: a turn runs until the work is done. Only the\n\
             # transcript size is managed, so a long turn stays inside the\n\
             # model's context window.\n\
             # Fold the transcript into a summary once a request would carry more\n\
             # than this many input tokens, as the provider counts them. 0\n\
             # disables it. Only used when the model's context window is not in\n\
             # the roster; otherwise the threshold is a share of the window.\n\
             compress_above_tokens = {compress_above_tokens}\n\
             keep_recent = {keep_recent}\n\
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
            keep_recent = self.agent.keep_recent,
            mtu = self.transport.mtu,
            window = self.transport.window,
        );
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Picks the roster entry the server should talk to.
///
/// Precedence is "the more specific word wins": a provider and a model name an
/// exact entry; a model alone finds the provider that carries it; nothing at
/// all takes the roster's first usable entry. A configuration that names
/// something the roster cannot place is a warning rather than a failure -- the
/// built-in default still gets the server up, and the line says what was
/// actually used, which is how a typo is found.
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
    /// The same transport, retries and instructions, pointed at another roster
    /// entry.
    ///
    /// A session that names its own `--llm` is choosing a different brain, not
    /// a different protocol: timeouts, retry policy and the shell instructions
    /// stay exactly as configured.
    pub fn for_entry(&self, entry: &crate::providers::Entry, api_key: String) -> Self {
        let mut cfg = self.clone();
        cfg.provider = Some(entry.provider.clone());
        cfg.base_url = entry.base_url.clone();
        cfg.model = entry.model.clone();
        cfg.context = entry.context;
        cfg.api_key_env = entry.api_key_env.clone().unwrap_or_default();
        cfg.api_key = api_key;
        cfg
    }
}

/// Renders a path as a TOML basic string.
///
/// Windows separators are normalised to `/`: TOML would read `\` as an escape,
/// and every Windows API accepts forward slashes anyway.
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

/// Reads a provider key out of `~/.dsh/.credentials.yaml`.
///
/// Only the flat `refs:` mapping is understood. The file is DSH's, not ours, so
/// anything it adds elsewhere is ignored rather than parsed.
fn dsh_credential(name: &str) -> Option<String> {
    let path = dirs::home_dir()?.join(".dsh").join(".credentials.yaml");
    let text = std::fs::read_to_string(path).ok()?;
    dsh_ref(&text, name)
}

fn dsh_ref(text: &str, name: &str) -> Option<String> {
    let mut in_refs = false;
    for line in text.lines() {
        if line.starts_with("refs:") {
            in_refs = true;
            continue;
        }
        // A top-level key ends the block.
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_refs = false;
        }
        if !in_refs {
            continue;
        }
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        if key.trim() != name {
            continue;
        }
        let value = value.trim().trim_matches('\'').trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
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
     harness repo, `scripts/evolve.sh stage` runs the tests, builds a \
     successor and prints a launcher; run that launcher with `bash \
     --detached`, then `scripts/evolve.sh swap <stamp>` checks that the \
     successor is really waiting and sends `cancel`. Do this only after \
     changing the server itself, and let the script decide when it is safe: it \
     refuses to cancel when no successor is waiting, because then nothing \
     would resume the turn.\n"
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
                return Some(p);
            }
        }
    }
    which::which("bash").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instructions are assembled by line continuation, where a stray
    /// escape or a missing space is invisible until a model reads the prompt
    /// as one run-on line.
    #[test]
    fn the_instructions_are_well_formed() {
        let text = default_instructions();
        assert!(!text.contains("\\n"), "literal backslash-n leaked: {text}");
        assert!(text.contains("\n\nYou can also replace the server"));
        assert!(text.contains("scripts/evolve.sh stage"));
        assert!(text.ends_with("would resume the turn.\n"));
        assert!(!text.contains("  "), "double space: {text}");
    }

    #[test]
    fn parses_a_minimal_config() {
        let cfg: FileConfig = toml::from_str("[llm]\nmodel = \"x\"\n").unwrap();
        assert_eq!(cfg.llm.unwrap().model.as_deref(), Some("x"));
        assert!(cfg.server.is_none());
    }

    const DSH_FIXTURE: &str =
        "version: 1\nrecords: {}\nrefs:\n  MINIMAX_CN_API_KEY: 'sk-abc123'\n  OTHER: 'no'\n";

    #[test]
    fn reads_a_key_out_of_the_dsh_credential_store() {
        assert_eq!(
            dsh_ref(DSH_FIXTURE, "MINIMAX_CN_API_KEY").as_deref(),
            Some("sk-abc123")
        );
        assert_eq!(dsh_ref(DSH_FIXTURE, "OTHER").as_deref(), Some("no"));
        assert_eq!(dsh_ref(DSH_FIXTURE, "MISSING"), None);
    }

    #[test]
    fn dsh_refs_stay_inside_their_block() {
        // The same name appears outside `refs:`; only the block counts.
        let text = "MINIMAX_CN_API_KEY: 'outside'\nrefs:\n  MINIMAX_CN_API_KEY: 'inside'\n";
        assert_eq!(
            dsh_ref(text, "MINIMAX_CN_API_KEY").as_deref(),
            Some("inside")
        );
    }

    #[test]
    fn continuation_lines_are_not_treated_as_keys() {
        // A folded value keeps its leading spaces, which is why the block test
        // above matters: only two-space keys belong to `refs`.
        let text = "refs:\n  A: 'x'\n    continued\n  B: 'y'\n";
        assert_eq!(dsh_ref(text, "B").as_deref(), Some("y"));
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

    /// The roster is what makes "no [llm] section" work. These are the four
    /// ways a configuration can name something, and the one way it can be
    /// wrong: a typo must not stop the server from starting.
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
}
