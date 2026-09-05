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
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    pub name: String,
    pub path: PathBuf,
}

/// One user account. `permissions` grants access to a subset of libraries
/// (by name) — a library with no matching entry is completely invisible to
/// this user, as if it didn't exist.
#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    pub username: String,
    pub token: String,
    #[serde(rename = "library", default)]
    pub permissions: Vec<UserLibraryPermission>,
}

/// Permissions granted to a user for a single library, referenced by name.
/// `read` controls visibility of the library and access to its contents;
/// `write` controls editing playlists and tags; `upload` controls running
/// downloader scripts against it.
#[derive(Debug, Clone, Deserialize)]
pub struct UserLibraryPermission {
    pub name: String,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub upload: bool,
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
            let mut seen_libs = HashSet::new();
            for perm in &user.permissions {
                let lib_name = perm.name.trim();
                if !seen.contains(lib_name) {
                    bail!("user {username} references unknown library: {lib_name}");
                }
                if !seen_libs.insert(lib_name.to_string()) {
                    bail!("user {username} has duplicate permission entry for library: {lib_name}");
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
            "{BASE}\n[[user]]\nusername = \"alan\"\ntoken = \"secret\"\n  [[user.library]]\n  name = \"main\"\n  read = true\n"
        );
        let cfg = parse(&toml_str).unwrap();
        assert_eq!(cfg.users.len(), 1);
        assert!(cfg.users[0].permissions[0].read);
    }

    #[test]
    fn rejects_permission_for_unknown_library() {
        let toml_str = format!(
            "{BASE}\n[[user]]\nusername = \"alan\"\ntoken = \"secret\"\n  [[user.library]]\n  name = \"nope\"\n  read = true\n"
        );
        let err = parse(&toml_str).unwrap_err();
        assert!(err.to_string().contains("unknown library"), "{err}");
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
