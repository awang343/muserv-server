use crate::config::Config;
use crate::libraries::Library;
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// A user's access to a single library. All fields default to `false` for
/// any library not explicitly listed in the user's config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryAccess {
    pub read: bool,
    pub write: bool,
    pub upload: bool,
}

#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub token: String,
    /// Keyed by library id (see `Library::id`), not name — resolved once at
    /// startup so request handling never has to do name lookups.
    pub permissions: HashMap<i64, LibraryAccess>,
}

impl User {
    pub fn access(&self, library_id: i64) -> LibraryAccess {
        self.permissions.get(&library_id).copied().unwrap_or_default()
    }
}

/// Resolves the library-centric `read_users` / `write_users` / `upload_users`
/// lists in the config into id-based `User`s. Errors if a library references
/// a username that doesn't match any configured `[[user]]` — `Config::normalize`
/// already checks this, so it should only trip if that validation is ever
/// bypassed.
pub fn resolve(cfg: &Config, libraries: &[Library]) -> Result<Vec<User>> {
    let name_to_id: HashMap<&str, i64> =
        libraries.iter().map(|l| (l.name.as_str(), l.id)).collect();

    let mut permissions: HashMap<String, HashMap<i64, LibraryAccess>> = HashMap::new();
    for lib in &cfg.libraries {
        let id = *name_to_id
            .get(lib.name.as_str())
            .ok_or_else(|| anyhow!("library {} not found while resolving users", lib.name))?;
        for username in &lib.read_users {
            permissions.entry(username.trim().to_string()).or_default().entry(id).or_default().read = true;
        }
        for username in &lib.write_users {
            permissions.entry(username.trim().to_string()).or_default().entry(id).or_default().write = true;
        }
        for username in &lib.upload_users {
            permissions.entry(username.trim().to_string()).or_default().entry(id).or_default().upload = true;
        }
    }

    Ok(cfg
        .users
        .iter()
        .map(|u| {
            let username = u.username.trim().to_string();
            let permissions = permissions.remove(&username).unwrap_or_default();
            User {
                username,
                token: u.token.clone(),
                permissions,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LibraryConfig, UserConfig};

    fn lib(id: i64, name: &str) -> Library {
        Library {
            id,
            name: name.to_string(),
            path: "/tmp/x".to_string(),
        }
    }

    fn lib_cfg(name: &str, read: &[&str], write: &[&str], upload: &[&str]) -> LibraryConfig {
        LibraryConfig {
            name: name.to_string(),
            path: "/tmp/x".into(),
            read_users: read.iter().map(|s| s.to_string()).collect(),
            write_users: write.iter().map(|s| s.to_string()).collect(),
            upload_users: upload.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn user_cfg(username: &str) -> UserConfig {
        UserConfig {
            username: username.to_string(),
            token: "tok".to_string(),
        }
    }

    fn config(libraries: Vec<LibraryConfig>, users: Vec<UserConfig>) -> Config {
        Config {
            bind: "127.0.0.1:7700".to_string(),
            libraries,
            users,
            downloaders_path: None,
        }
    }

    #[test]
    fn resolves_names_to_ids() {
        let libraries = vec![lib(1, "main"), lib(2, "podcasts")];
        let cfg = config(
            vec![lib_cfg("main", &[], &[], &[]), lib_cfg("podcasts", &["alan"], &[], &[])],
            vec![user_cfg("alan")],
        );
        let users = resolve(&cfg, &libraries).unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(
            users[0].access(2),
            LibraryAccess {
                read: true,
                write: false,
                upload: false
            }
        );
        // No entry for library 1 → all-false default.
        assert_eq!(users[0].access(1), LibraryAccess::default());
    }

    #[test]
    fn missing_library_id_defaults_to_no_access() {
        let libraries = vec![lib(1, "main")];
        let cfg = config(vec![lib_cfg("main", &[], &[], &[])], vec![user_cfg("alan")]);
        let users = resolve(&cfg, &libraries).unwrap();
        assert_eq!(users[0].access(1), LibraryAccess::default());
    }
}
