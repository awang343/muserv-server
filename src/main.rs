use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod api;
mod config;
mod db;
mod ingest;
mod libraries;

#[derive(Parser)]
#[command(name = "muserv", version, about = "Muserv: personal music library server")]
struct Cli {
    /// Path to config.toml. Defaults to $XDG_CONFIG_HOME/muserv/config.toml
    /// (or ~/.config/muserv/config.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

fn default_config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("muserv").join("config.toml")
}

#[derive(Subcommand)]
enum Cmd {
    /// Import audio files into a library's content-addressed storage.
    ///
    /// With no --path, self-migrates the library: converts any legacy
    /// (pre-restructure) rows still pointing at their original on-disk path,
    /// and picks up any audio files sitting loose in the library root that
    /// were never tracked at all. With --path, bulk-imports every audio file
    /// found under that directory instead (e.g. importing a folder of new
    /// music).
    Import {
        /// Only import into this library (by name). Required when --path is
        /// given (to disambiguate); defaults to all configured libraries
        /// otherwise.
        #[arg(long)]
        library: Option<String>,
        /// Import audio files from this directory instead of self-migrating
        /// the library's own root.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Move files into storage instead of copying them. Frees up space
        /// by removing originals once they're safely stored — recommended
        /// for migrating an existing library, since otherwise each file
        /// ends up duplicated (once at its original path, once in
        /// .storage).
        #[arg(long = "move")]
        move_files: bool,
    },
    /// Run the HTTP server.
    Serve {
        /// Import all libraries (self-migrate + pick up loose files) before
        /// starting, moving files into storage.
        #[arg(long)]
        import: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,muserv=debug")),
        )
        .init();

    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(default_config_path);
    let cfg = config::Config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;
    tracing::info!(?cfg, "loaded config");

    let pool = db::connect(&cfg.db_path).await?;
    let libs = libraries::sync(&pool, &cfg.libraries).await?;
    tracing::info!(count = libs.len(), "libraries synced");

    match cli.cmd {
        Cmd::Import { library, path, move_files } => {
            let mode = if move_files { ingest::CopyMode::Move } else { ingest::CopyMode::Copy };
            if let Some(dir) = path {
                let name = library
                    .ok_or_else(|| anyhow::anyhow!("--library is required when --path is given"))?;
                let lib = libs
                    .iter()
                    .find(|l| l.name == name)
                    .ok_or_else(|| anyhow::anyhow!("no matching library"))?;
                println!("== importing {} into library: {} ({}) ==", dir.display(), lib.name, lib.root_path);
                let stats = ingest::import_dir(&pool, lib, &dir, mode).await?;
                print_import_summary(&stats);
            } else {
                let to_import: Vec<&libraries::Library> = match library.as_deref() {
                    Some(name) => libs.iter().filter(|l| l.name == name).collect(),
                    None => libs.iter().collect(),
                };
                if to_import.is_empty() {
                    anyhow::bail!("no matching library");
                }
                for lib in to_import {
                    println!("== library: {} ({}) ==", lib.name, lib.root_path);
                    let legacy = ingest::migrate_legacy_rows(&pool, lib, mode).await?;
                    let loose = ingest::import_dir(&pool, lib, &lib.root(), mode).await?;
                    print_import_summary(&legacy.merge(loose));
                }
            }
        }
        Cmd::Serve { import } => {
            if import {
                for lib in &libs {
                    println!("== library: {} ({}) ==", lib.name, lib.root_path);
                    let legacy = ingest::migrate_legacy_rows(&pool, lib, ingest::CopyMode::Move).await?;
                    let loose = ingest::import_dir(&pool, lib, &lib.root(), ingest::CopyMode::Move).await?;
                    print_import_summary(&legacy.merge(loose));
                }
            }
            if cfg.auth_token.is_none() && !cfg.bind.starts_with("127.")
                && !cfg.bind.starts_with("localhost")
                && !cfg.bind.starts_with("[::1]")
            {
                tracing::warn!(bind = %cfg.bind, "auth_token is unset and bind is non-loopback — API is open");
            }
            let router = api::router(pool, cfg.auth_token.clone(), libs, cfg.downloaders_path.clone());
            let listener = tokio::net::TcpListener::bind(&cfg.bind)
                .await
                .with_context(|| format!("binding {}", cfg.bind))?;
            tracing::info!(addr = %cfg.bind, "listening");
            axum::serve(listener, router).await?;
        }
    }
    Ok(())
}

fn print_import_summary(stats: &ingest::ImportStats) {
    println!(
        "import: scanned={} imported={} duplicates={} failed={}",
        stats.scanned, stats.imported, stats.duplicates, stats.failed,
    );
    if !stats.failures.is_empty() {
        println!();
        println!(
            "{} file{} failed to import:",
            stats.failures.len(),
            if stats.failures.len() == 1 { "" } else { "s" },
        );
        for f in &stats.failures {
            println!("  {}: {}", f.path.display(), f.reason);
        }
    }
}
