use crate::config::LibraryConfig;
use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::SqlitePool;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub path: String,
}

impl Library {
    pub fn root(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }
}

/// Opens every configured library, each fully independent: creates its
/// folder if missing, then opens (or creates) `<path>/library.db`, running
/// migrations against it. `id` is assigned as the 1-based index into `cfg`
/// — stable as long as `[[library]]` entries aren't reordered or removed,
/// which is an acceptable tradeoff for a single-user server with no shared
/// registry across libraries.
pub async fn open_all(cfg: &[LibraryConfig]) -> Result<Vec<(Library, SqlitePool)>> {
    let mut out = Vec::with_capacity(cfg.len());
    for (i, lib) in cfg.iter().enumerate() {
        let id = (i + 1) as i64;
        let name = lib.name.trim().to_string();
        std::fs::create_dir_all(&lib.path)
            .with_context(|| format!("creating library directory {}", lib.path.display()))?;

        let db_path = lib.path.join("library.db");
        let pool = crate::db::connect(&db_path)
            .await
            .with_context(|| format!("opening library db for {name}"))?;

        out.push((
            Library {
                id,
                name,
                path: lib.path.to_string_lossy().to_string(),
            },
            pool,
        ));
    }
    Ok(out)
}
