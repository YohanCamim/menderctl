use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::Cli;

const DEFAULT_SERVER: &str = "https://hosted.mender.io";

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub server: String,
    pub token: Option<String>,
    pub token_path: PathBuf,
    pub skip_verify: bool,
    pub login_username: Option<String>,
    pub login_password: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FileConfig {
    pub server: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(alias = "token-value")]
    pub token_value: Option<String>,
}

impl RuntimeConfig {
    pub fn resolve(cli: &Cli) -> Result<Self> {
        if cli.token_value.is_some() && cli.token.is_some() {
            bail!("--token-value and --token cannot be used together");
        }

        let file_config = load_file_config()?.unwrap_or_default();
        let server = cli
            .server
            .clone()
            .or_else(|| env::var("MENDER_SERVER").ok())
            .or(file_config.server)
            .unwrap_or_else(|| DEFAULT_SERVER.to_owned());
        let server = normalize_server(&server)?;

        let token_path = cli.token.clone().unwrap_or_else(default_token_path);
        let token = cli
            .token_value
            .clone()
            .or_else(|| env::var("MENDER_TOKEN").ok())
            .or(file_config.token_value)
            .or_else(|| read_token(&token_path).ok());
        let login_username = env::var("MENDER_USERNAME").ok().or(file_config.username);
        let login_password = env::var("MENDER_PASSWORD").ok().or(file_config.password);

        Ok(Self {
            server,
            token,
            token_path,
            skip_verify: cli.skip_verify,
            login_username,
            login_password,
        })
    }

    pub fn require_token(&self) -> Result<&str> {
        self.token
            .as_deref()
            .filter(|token| !token.is_empty())
            .context("no token is available; run `menderctl login` or use --token-value")
    }
}

pub fn default_token_path() -> PathBuf {
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mender")
        .join("authtoken")
}

pub fn save_token(path: &PathBuf, token: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, token)
        .with_context(|| format!("failed to write the token to {}", path.display()))?;
    set_private_permissions(path)?;
    Ok(())
}

fn read_token(path: &PathBuf) -> Result<String> {
    let token =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(token.trim().to_owned())
}

fn load_file_config() -> Result<Option<FileConfig>> {
    let home = dirs::home_dir();
    let candidates = config_candidates(home.as_deref());

    let Some(path) = candidates.into_iter().find(|path| path.is_file()) else {
        return Ok(None);
    };
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let config = serde_json::from_str(&contents)
        .with_context(|| format!("invalid JSON configuration in {}", path.display()))?;
    Ok(Some(config))
}

fn config_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(".menderctlrc"),
        PathBuf::from(".mender-clirc"),
    ];
    if let Some(home) = home {
        candidates.push(home.join(".menderctlrc"));
        candidates.push(home.join(".mender-clirc"));
    }
    candidates.push(PathBuf::from("/etc/menderctl/.menderctlrc"));
    candidates.push(PathBuf::from("/etc/mender/.mender-clirc"));
    candidates
}

fn normalize_server(server: &str) -> Result<String> {
    let server = if server.contains("://") {
        server.to_owned()
    } else {
        format!("https://{server}")
    };
    let parsed = url::Url::parse(&server).context("invalid server URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("the server must use HTTP or HTTPS");
    }
    Ok(server.trim_end_matches('/').to_owned())
}

#[cfg(unix)]
fn set_private_permissions(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &PathBuf) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{config_candidates, normalize_server};

    #[test]
    fn adds_https_when_the_scheme_is_missing() {
        assert_eq!(
            normalize_server("hosted.mender.io/").unwrap(),
            "https://hosted.mender.io"
        );
    }

    #[test]
    fn rejects_non_http_schemes() {
        assert!(normalize_server("file:///tmp/mender").is_err());
    }

    #[test]
    fn supports_legacy_configuration_during_migration() {
        let candidates = config_candidates(Some(Path::new("/home/alice")));
        assert!(candidates.contains(&Path::new("/home/alice/.menderctlrc").to_path_buf()));
        assert!(candidates.contains(&Path::new("/home/alice/.mender-clirc").to_path_buf()));
    }
}
