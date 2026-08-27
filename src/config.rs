use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub db_path: PathBuf,
    pub bind: String,
    /// If set, all /api/* routes require `Authorization: Bearer <token>`.
    /// If unset, the API is open — fine for localhost, never for non-loopback.
    #[serde(default)]
    pub auth_token: Option<String>,

    /// One entry per library. Names are user-visible and must be unique.
    #[serde(rename = "library", default)]
    pub libraries: Vec<LibraryConfig>,

    /// Legacy single-library path, kept for backwards-compat with old configs.
    /// If `libraries` is empty and this is set, it becomes the sole library
    /// named "main".
    #[serde(default)]
    pub library_path: Option<PathBuf>,

    /// Directory of executable downloader scripts the API can offer to run
    /// (see `api::downloaders`). Optional — the downloaders API returns an
    /// empty list if unset.
    #[serde(default)]
    pub downloaders_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    pub name: String,
    pub path: PathBuf,
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
            if let Some(p) = self.library_path.take() {
                self.libraries.push(LibraryConfig {
                    name: "main".into(),
                    path: p,
                });
            }
        }
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
        Ok(())
    }
}
