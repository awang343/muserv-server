use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod api;
mod config;
mod db;
mod ingest;
mod libraries;
mod users;

#[derive(Parser)]
#[command(
    name = "muserv",
    version,
    about = "Muserv: personal music library server"
)]
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
    /// Bulk-import a folder of audio files into a library's
    /// content-addressed storage (e.g. importing a folder of new music).
    Import {
        /// Library to import into (by name).
        #[arg(long)]
        library: String,
        /// Import audio files from this directory.
        #[arg(long)]
        path: PathBuf,
        /// Move files into storage instead of copying them. Frees up space
        /// by removing originals once they're safely stored — recommended
        /// for migrating an existing library, since otherwise each file
        /// ends up duplicated (once at its original path, once in
        /// .storage).
        #[arg(long = "move")]
        move_files: bool,
    },
    /// Run the HTTP server.
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,muserv=debug")),
        )
        .init();

    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(default_config_path);
    let cfg = config::Config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;
    tracing::info!(?cfg, "loaded config");

    let libs = libraries::open_all(&cfg.libraries).await?;
    tracing::info!(count = libs.len(), "libraries opened");

    let lib_list: Vec<libraries::Library> = libs.iter().map(|(l, _)| l.clone()).collect();
    let resolved_users = users::resolve(&cfg.users, &lib_list).context("resolving users from config")?;

    match cli.cmd {
        Cmd::Import {
            library,
            path,
            move_files,
        } => {
            let mode = if move_files {
                ingest::CopyMode::Move
            } else {
                ingest::CopyMode::Copy
            };
            let (lib, pool) = libs
                .iter()
                .find(|(l, _)| l.name == library)
                .ok_or_else(|| anyhow::anyhow!("no matching library"))?;
            println!(
                "== importing {} into library: {} ({}) ==",
                path.display(),
                lib.name,
                lib.path
            );
            let stats = ingest::import_dir(pool, lib, &path, mode).await?;
            print_import_summary(&stats);
        }
        Cmd::Serve => {
            if resolved_users.is_empty() {
                tracing::warn!(
                    "no users configured — every API request will be rejected with 401; add at least one [[user]] to config.toml"
                );
            }
            let router = api::router(libs, resolved_users, cfg.downloaders_path.clone());
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
