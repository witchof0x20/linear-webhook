//! Configuration file parsing.
//!
//! ```toml
//! [server1]
//! discord_webhook_url = "https://discord.com/api/webhooks/<id>/<token>"
//! linear_secret = "lin_wh_..."
//!
//! [server2]
//! discord_webhook_url_file = "/run/secrets/server2-discord"
//! linear_secret_file = "/run/secrets/server2-linear"
//! ```
//!
//! Every table name becomes the route `POST /<name>`. Each value can be given
//! inline or read from a file (`*_file`), never both.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serenity::model::id::WebhookId;

use crate::verify::Secret;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    discord_webhook_url: Option<String>,
    discord_webhook_url_file: Option<PathBuf>,
    linear_secret: Option<String>,
    linear_secret_file: Option<PathBuf>,
}

/// A Discord webhook, validated at startup.
#[derive(Clone)]
pub struct DiscordWebhook {
    pub id: WebhookId,
    pub token: String,
}

impl fmt::Debug for DiscordWebhook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordWebhook")
            .field("id", &self.id)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl DiscordWebhook {
    pub fn parse(url: &str) -> Result<Self, ConfigError> {
        let parsed = url::Url::parse(url).map_err(|_| ConfigError::BadWebhookUrl)?;
        let (id, token) =
            serenity::utils::parse_webhook(&parsed).ok_or(ConfigError::BadWebhookUrl)?;
        Ok(Self { id, token: token.to_owned() })
    }
}

/// How deliveries to a server are authenticated.
///
/// `Disabled` can only be constructed when the operator passed
/// `--dangerously-ignore-secret`.
#[derive(Debug, Clone)]
pub enum Auth {
    Verify(Secret),
    Disabled,
}

#[derive(Debug, Clone)]
pub struct Server {
    pub webhook: DiscordWebhook,
    pub auth: Auth,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub servers: BTreeMap<String, Server>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid config: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("no servers defined")]
    Empty,
    #[error("[{0}]: server names must be 1-64 characters of [A-Za-z0-9_-]")]
    BadName(String),
    #[error("[{server}]: {key} and {key}_file are mutually exclusive")]
    BothSet { server: String, key: &'static str },
    #[error("[{server}]: {key} or {key}_file is required")]
    Missing { server: String, key: &'static str },
    #[error("[{server}]: {key} is empty")]
    EmptyValue { server: String, key: &'static str },
    #[error("not a valid Discord webhook URL (expected https://discord.com/api/webhooks/<id>/<token>)")]
    BadWebhookUrl,
    #[error("[{server}]: {source}")]
    InServer { server: String, source: Box<ConfigError> },
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn read_secret_file(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .map_err(|source| ConfigError::Io { path: path.to_owned(), source })
}

/// Resolve `key` / `key_file` into a value. `Ok(None)` if neither is set.
fn resolve(
    server: &str,
    key: &'static str,
    inline: Option<String>,
    file: Option<PathBuf>,
) -> Result<Option<String>, ConfigError> {
    let value = match (inline, file) {
        (Some(_), Some(_)) => return Err(ConfigError::BothSet { server: server.into(), key }),
        (None, None) => return Ok(None),
        (Some(v), None) => v.trim().to_owned(),
        (None, Some(path)) => read_secret_file(&path)?,
    };
    if value.is_empty() {
        return Err(ConfigError::EmptyValue { server: server.into(), key });
    }
    Ok(Some(value))
}

impl Config {
    pub fn load(path: &Path, ignore_secret: bool) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|source| ConfigError::Io { path: path.to_owned(), source })?;
        Self::parse(&text, ignore_secret)
    }

    pub fn parse(text: &str, ignore_secret: bool) -> Result<Self, ConfigError> {
        let raw: BTreeMap<String, RawServer> = toml::from_str(text)?;
        if raw.is_empty() {
            return Err(ConfigError::Empty);
        }
        let mut servers = BTreeMap::new();
        for (name, raw) in raw {
            if !valid_name(&name) {
                return Err(ConfigError::BadName(name));
            }
            let url = resolve(&name, "discord_webhook_url", raw.discord_webhook_url, raw.discord_webhook_url_file)?
                .ok_or_else(|| ConfigError::Missing { server: name.clone(), key: "discord_webhook_url" })?;
            let webhook = DiscordWebhook::parse(&url)
                .map_err(|e| ConfigError::InServer { server: name.clone(), source: Box::new(e) })?;
            let secret = resolve(&name, "linear_secret", raw.linear_secret, raw.linear_secret_file)?;
            let auth = match (ignore_secret, secret) {
                (true, _) => Auth::Disabled,
                (false, Some(s)) => Auth::Verify(Secret::new(s)),
                (false, None) => {
                    return Err(ConfigError::Missing { server: name, key: "linear_secret" });
                }
            };
            servers.insert(name, Server { webhook, auth });
        }
        Ok(Self { servers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://discord.com/api/webhooks/123456789012345678/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn cfg(body: &str) -> String {
        body.replace("URL", URL)
    }

    #[test]
    fn parses_inline() {
        let c = Config::parse(&cfg("[a]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"s\"\n"), false).unwrap();
        let s = &c.servers["a"];
        assert_eq!(s.webhook.id.get(), 123456789012345678);
        assert!(matches!(s.auth, Auth::Verify(_)));
    }

    #[test]
    fn reads_files_and_trims() {
        let dir = std::env::temp_dir().join(format!("lw-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("url"), format!("{URL}\n")).unwrap();
        std::fs::write(dir.join("sec"), "secret\n").unwrap();
        let text = format!(
            "[a]\ndiscord_webhook_url_file = {:?}\nlinear_secret_file = {:?}\n",
            dir.join("url"),
            dir.join("sec")
        );
        let c = Config::parse(&text, false).unwrap();
        assert_eq!(c.servers["a"].webhook.token.len(), 68);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secret_required_unless_ignored() {
        let text = cfg("[a]\ndiscord_webhook_url = \"URL\"\n");
        assert!(matches!(Config::parse(&text, false), Err(ConfigError::Missing { key: "linear_secret", .. })));
        assert!(matches!(Config::parse(&text, true).unwrap().servers["a"].auth, Auth::Disabled));
    }

    #[test]
    fn ignore_flag_overrides_configured_secret() {
        let text = cfg("[a]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"s\"\n");
        assert!(matches!(Config::parse(&text, true).unwrap().servers["a"].auth, Auth::Disabled));
    }

    #[test]
    fn rejects_bad_input() {
        let both = cfg("[a]\ndiscord_webhook_url = \"URL\"\ndiscord_webhook_url_file = \"/x\"\nlinear_secret = \"s\"\n");
        assert!(matches!(Config::parse(&both, false), Err(ConfigError::BothSet { .. })));

        let unknown = cfg("[a]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"s\"\nextra = 1\n");
        assert!(matches!(Config::parse(&unknown, false), Err(ConfigError::Toml(_))));

        let bad_url = "[a]\ndiscord_webhook_url = \"https://example.com/hook\"\nlinear_secret = \"s\"\n";
        assert!(matches!(Config::parse(bad_url, false), Err(ConfigError::InServer { .. })));

        let bad_name = cfg("[\"a/b\"]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"s\"\n");
        assert!(matches!(Config::parse(&bad_name, false), Err(ConfigError::BadName(_))));

        let empty_secret = cfg("[a]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"  \"\n");
        assert!(matches!(Config::parse(&empty_secret, false), Err(ConfigError::EmptyValue { .. })));

        assert!(matches!(Config::parse("", false), Err(ConfigError::Empty)));
        assert!(matches!(Config::parse(&cfg("[a]\nlinear_secret = \"s\"\n"), false), Err(ConfigError::Missing { .. })));
    }

    #[test]
    fn debug_redacts() {
        let c = Config::parse(&cfg("[a]\ndiscord_webhook_url = \"URL\"\nlinear_secret = \"hunter2\"\n"), false).unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("hunter2"));
        assert!(!dbg.contains("aaaaaaaa"));
    }
}
