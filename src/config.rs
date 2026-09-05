use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub bind: String,

    /// One entry per library. Names are user-visible and must be unique.
    #[serde(rename = "library", default)]
    pub libraries: Vec<LibraryConfig>,

    /// One entry per user account. Every request must present
    /// `Authorization: Basic <base64(username:token)>` matching one of these
    /// users — if this is empty, every request is rejected, since there's
    /// nothing to match against.
    #[serde(rename = "user", default)]
    pub users: Vec<UserConfig>,

    /// Directory of executable downloader scripts the API can offer to run
    /// (see `api::downloaders`). Optional — the downloaders API returns an
    /// empty list if unset.
    #[serde(default)]
    pub downloaders_path: Option<PathBuf>,
}

/// `path` is a folder muserv owns end-to-end: it holds `library.db` (this
/// library's own sqlite db) and `.storage/` (its content-addressed audio
/// files), both created on first run. Libraries are fully independent —
/// nothing is shared across them.
///
/// `read_users` / `write_users` / `upload_users` grant access by username —
/// a user not listed anywhere for a library has no access to it at all, as
/// if it didn't exist. `read` controls visibility of the library and access
/// to its contents; `write` controls editing playlists and tags; `upload`
/// controls running downloader scripts against it. The lists are
/// independent, so a username can appear in `write_users` without also
/// being in `read_users`.
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub read_users: Vec<String>,
    #[serde(default)]
    pub write_users: Vec<String>,
    #[serde(default)]
    pub upload_users: Vec<String>,
}

/// One user account. Library access is granted by listing this user's
/// `username` in the relevant library's `read_users` / `write_users` /
/// `upload_users`.
#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    pub username: String,
    pub token: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&raw).context("parsing config")?;
        cfg.normalize()?;
        Ok(cfg)
    }

    fn normalize(&mut self) -> Result<()> {
        if self.libraries.is_empty() {
            bail!("config has no libraries — add at least one [[library]] section");
        }
        let mut seen = HashSet::new();
        for lib in &self.libraries {
            let name = lib.name.trim();
            if name.is_empty() {
                bail!("library has empty name");
            }
            if !seen.insert(name.to_string()) {
                bail!("duplicate library name: {name}");
            }
        }

        let mut usernames = HashSet::new();
        for user in &self.users {
            let username = user.username.trim();
            if username.is_empty() {
                bail!("user has empty username");
            }
            if !usernames.insert(username.to_string()) {
                bail!("duplicate username: {username}");
            }
            if user.token.trim().is_empty() {
                bail!("user {username} has empty token");
            }
        }

        for lib in &self.libraries {
            let lists = [
                ("read_users", &lib.read_users),
                ("write_users", &lib.write_users),
                ("upload_users", &lib.upload_users),
            ];
            for (field, list) in lists {
                for username in list {
                    let username = username.trim();
                    if !usernames.contains(username) {
                        bail!(
                            "library {} {field} references unknown user: {username}",
                            lib.name
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<Config> {
        let mut cfg: Config = toml::from_str(toml_str).context("parsing config")?;
        cfg.normalize()?;
        Ok(cfg)
    }

    const BASE: &str = r#"
        bind = "127.0.0.1:7700"
        [[library]]
        name = "main"
        path = "/tmp/main"
    "#;

    #[test]
    fn no_users_is_valid_config() {
        let cfg = parse(BASE).unwrap();
        assert!(cfg.users.is_empty());
    }

    #[test]
    fn valid_user_with_permissions() {
        let toml_str = format!(
            "bind = \"127.0.0.1:7700\"\n[[library]]\nname = \"main\"\npath = \"/tmp/main\"\nread_users = [\"alan\"]\n\n[[user]]\nusername = \"alan\"\ntoken = \"secret\"\n"
        );
        let cfg = parse(&toml_str).unwrap();
        assert_eq!(cfg.users.len(), 1);
        assert_eq!(cfg.libraries[0].read_users, vec!["alan".to_string()]);
    }

    #[test]
    fn rejects_permission_for_unknown_user() {
        let toml_str = format!("{BASE}\n").replace(
            "path = \"/tmp/main\"",
            "path = \"/tmp/main\"\nread_users = [\"nope\"]",
        );
        let err = parse(&toml_str).unwrap_err();
        assert!(err.to_string().contains("unknown user"), "{err}");
    }

    #[test]
    fn rejects_duplicate_username() {
        let toml_str = format!(
            "{BASE}\n[[user]]\nusername = \"alan\"\ntoken = \"a\"\n[[user]]\nusername = \"alan\"\ntoken = \"b\"\n"
        );
        let err = parse(&toml_str).unwrap_err();
        assert!(err.to_string().contains("duplicate username"), "{err}");
    }

    #[test]
    fn rejects_empty_token() {
        let toml_str = format!("{BASE}\n[[user]]\nusername = \"alan\"\ntoken = \"\"\n");
        let err = parse(&toml_str).unwrap_err();
        assert!(err.to_string().contains("empty token"), "{err}");
    }
}
