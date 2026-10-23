//! Bridge configuration: which IM to speak to, and who may drive the harness.
//!
//! The server's own settings are *not* duplicated here -- `bind` and `key` are
//! read out of `config.toml` the same way the CLI reads them, so one harness
//! home has one source of truth. This file only adds what the CLI has no
//! opinion about.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bridge: Bridge,
    pub qq: Qq,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Bridge {
    /// Working directory for sessions this bridge creates. Empty means "the
    /// bridge's own cwd", which is what you want when it runs from the repo.
    pub cwd: String,
    /// Platform user ids allowed to drive the harness.
    ///
    /// Empty means **nobody**: an agent with a real shell behind it is a root
    /// shell with a chat interface, so this fails closed. The refusal the
    /// bridge logs names the id to add.
    pub allowed: Vec<String>,
    /// Answer "on it" as soon as the harness accepts the message.
    pub ack: bool,
    /// Reply length cap in characters (0 disables). QQ drops bodies it thinks
    /// are too long, so the default sits well under its limit.
    pub max_chars: usize,
}

impl Default for Bridge {
    fn default() -> Self {
        Self { cwd: String::new(), allowed: Vec::new(), ack: true, max_chars: 1800 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Qq {
    pub app_id: String,
    pub app_secret: String,
    /// Talk to `sandbox.api.sgroup.qq.com` instead of the live environment.
    ///
    /// Default true: the sandbox needs no review, no published bot and no IP
    /// allow-list, which is the shape of "I want this working tonight".
    pub sandbox: bool,
}

impl Default for Qq {
    fn default() -> Self {
        Self { app_id: String::new(), app_secret: String::new(), sandbox: true }
    }
}

impl Config {
    pub fn path(home: &Path) -> PathBuf {
        home.join("bridge.toml")
    }

    /// Loads `bridge.toml`, writing a commented starter file if there is none.
    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let path = Self::path(home);
        if !path.exists() {
            std::fs::create_dir_all(home)?;
            std::fs::write(&path, TEMPLATE)?;
            tracing::info!(path = %path.display(), "wrote a starter bridge config");
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let mut cfg: Self = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;
        // Secrets may live in the environment, so a script can start the bridge
        // without the secret ever touching disk.
        if let Ok(v) = std::env::var("GONGGA_QQ_APP_ID") {
            if !v.is_empty() {
                cfg.qq.app_id = v;
            }
        }
        if let Ok(v) = std::env::var("GONGGA_QQ_APP_SECRET") {
            if !v.is_empty() {
                cfg.qq.app_secret = v;
            }
        }
        Ok(cfg)
    }
}

const TEMPLATE: &str = r#"# gongga bridge configuration
# The harness address and key are read from config.toml in the same home, so
# they are not repeated here. GONGGA_QQ_APP_ID / GONGGA_QQ_APP_SECRET override
# the values below.

[bridge]
# Working directory for the sessions this bridge creates. Empty = the bridge's
# own cwd. Point it at a sandbox directory if anyone else will talk to the bot:
# the agent runs commands here without asking.
cwd = ""

# Platform user ids allowed to drive the harness. Empty = nobody. The first
# refusal the bridge logs names the id to add.
allowed = []

# Reply "received, working on it" as soon as the harness takes the message.
ack = true

# Reply length cap in characters. 0 = no cap.
max_chars = 1800

[qq]
app_id = ""
app_secret = ""
# Sandbox first: no review, no published bot, no IP allow-list. Flip to false
# only once the bot is live.
sandbox = true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_written_with_safe_defaults() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert!(Config::path(home.path()).exists());
        // Fails closed: an empty allow-list means no one can drive the shell.
        assert!(cfg.bridge.allowed.is_empty());
        assert!(cfg.qq.sandbox, "the default target is the sandbox");
        assert!(cfg.bridge.ack);
    }

    #[test]
    fn values_round_trip() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::path(home.path()),
            "[bridge]\nallowed = [\"u1\"]\ncwd = \"C:/work\"\n\n[qq]\napp_id = \"1\"\napp_secret = \"s\"\nsandbox = false\n",
        )
        .unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert_eq!(cfg.bridge.allowed, vec!["u1".to_string()]);
        assert_eq!(cfg.bridge.cwd, "C:/work");
        assert!(!cfg.qq.sandbox);
    }

    /// The template has to parse against the current schema, or a fresh install
    /// starts with a config the program cannot read.
    #[test]
    fn the_template_parses() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.bridge.max_chars, 1800);
        assert!(cfg.qq.sandbox);
    }
}
