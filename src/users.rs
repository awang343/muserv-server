use crate::config::UserConfig;
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

/// Resolves name-based config permissions into id-based `User`s. Errors if a
/// user references a library name that doesn't match any configured
/// library — `Config::normalize` already checks this, so it should only
/// trip if that validation is ever bypassed.
pub fn resolve(cfg: &[UserConfig], libraries: &[Library]) -> Result<Vec<User>> {
    let name_to_id: HashMap<&str, i64> =
        libraries.iter().map(|l| (l.name.as_str(), l.id)).collect();

    cfg.iter()
        .map(|u| {
            let mut permissions = HashMap::new();
            for perm in &u.permissions {
                let name = perm.name.trim();
                let id = *name_to_id
                    .get(name)
                    .ok_or_else(|| anyhow!("user {} references unknown library {name}", u.username))?;
                permissions.insert(
                    id,
                    LibraryAccess {
                        read: perm.read,
                        write: perm.write,
                        upload: perm.upload,
                    },
                );
            }
            Ok(User {
                username: u.username.trim().to_string(),
                token: u.token.clone(),
                permissions,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UserLibraryPermission;

    fn lib(id: i64, name: &str) -> Library {
        Library {
            id,
            name: name.to_string(),
            path: "/tmp/x".to_string(),
        }
    }

    fn user(username: &str, perms: Vec<(&str, bool, bool, bool)>) -> UserConfig {
        UserConfig {
            username: username.to_string(),
            token: "tok".to_string(),
            permissions: perms
                .into_iter()
                .map(|(name, read, write, upload)| UserLibraryPermission {
                    name: name.to_string(),
                    read,
                    write,
                    upload,
                })
                .collect(),
        }
    }

    #[test]
    fn resolves_names_to_ids() {
        let libraries = vec![lib(1, "main"), lib(2, "podcasts")];
        let cfg = vec![user("alan", vec![("podcasts", true, false, false)])];
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
    fn unknown_library_name_errors() {
        let libraries = vec![lib(1, "main")];
        let cfg = vec![user("alan", vec![("nope", true, true, true)])];
        assert!(resolve(&cfg, &libraries).is_err());
    }

    #[test]
    fn missing_library_id_defaults_to_no_access() {
        let libraries = vec![lib(1, "main")];
        let cfg = vec![user("alan", vec![])];
        let users = resolve(&cfg, &libraries).unwrap();
        assert_eq!(users[0].access(1), LibraryAccess::default());
    }
}
