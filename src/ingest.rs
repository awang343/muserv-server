use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::libraries::Library;

const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "m4b", "mp4", "aac", "wav", "aiff", "aif", "wv",
    "ape", "mka",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| AUDIO_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

#[derive(Clone, Copy)]
pub enum CopyMode {
    Copy,
    Move,
}

pub enum IngestOutcome {
    Imported,
    Duplicate,
}

#[derive(Debug, Default)]
pub struct ImportStats {
    pub scanned: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub failed: u64,
    pub failures: Vec<ImportFailure>,
}

#[derive(Debug)]
pub struct ImportFailure {
    pub path: PathBuf,
    pub reason: String,
}

/// Serializable summary of an `ImportStats`, for API responses (e.g. the
/// downloader job's post-download ingest summary).
#[derive(Debug, Clone, Serialize)]
pub struct ImportStatsView {
    pub scanned: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub failed: u64,
}

impl From<&ImportStats> for ImportStatsView {
    fn from(s: &ImportStats) -> Self {
        Self {
            scanned: s.scanned,
            imported: s.imported,
            duplicates: s.duplicates,
            failed: s.failed,
        }
    }
}

/// Walks `dir` recursively and ingests every audio file found, skipping the
/// library's own `.storage` directory (already-ingested files live there).
/// Used for ad-hoc bulk imports of a folder of new music (the `muserv
/// import --path` CLI command) and for the downloader feature's
/// post-download staging directory. Safe to re-run: files whose content
/// already exists in the library (by hash) are reported as duplicates
/// rather than re-imported.
pub async fn import_dir(
    pool: &SqlitePool,
    lib: &Library,
    dir: &Path,
    mode: CopyMode,
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();
    let storage_root = lib.root().join(".storage");

    for entry in WalkDir::new(dir).follow_links(true) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "walk error");
                stats.failed += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().starts_with(&storage_root) {
            continue;
        }
        if !is_audio_file(entry.path()) {
            continue;
        }
        stats.scanned += 1;
        match ingest_file(pool, lib, entry.path(), mode).await {
            Ok(IngestOutcome::Imported) => stats.imported += 1,
            Ok(IngestOutcome::Duplicate) => stats.duplicates += 1,
            Err(e) => {
                warn!(path = %entry.path().display(), error = ?e, "import failed");
                stats.failed += 1;
                stats.failures.push(ImportFailure {
                    path: entry.path().to_path_buf(),
                    reason: format!("{e:#}"),
                });
            }
        }
    }

    info!(
        scanned = stats.scanned,
        imported = stats.imported,
        duplicates = stats.duplicates,
        failed = stats.failed,
        "import complete"
    );
    Ok(stats)
}

/// Hashes, tag-parses, and inserts one new track from `src` into `lib`.
/// Returns `Duplicate` (without touching `src`) if a track with the same
/// content hash already exists in the library.
async fn ingest_file(pool: &SqlitePool, lib: &Library, src: &Path, mode: CopyMode) -> Result<IngestOutcome> {
    let hash = hash_file(src).await?;

    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM tracks WHERE hash = ?")
        .bind(&hash)
        .fetch_optional(pool)
        .await?;
    if existing.is_some() {
        return Ok(IngestOutcome::Duplicate);
    }

    let src_owned = src.to_path_buf();
    let parsed = tokio::task::spawn_blocking(move || parse_file(&src_owned))
        .await
        .context("blocking parse task")??;

    let storage_rel = store_file(lib, src, &hash, mode).await?;
    let original_filename = filename_of(src);
    let file_size = tokio::fs::metadata(lib.root().join(&storage_rel)).await?.len() as i64;
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"
        INSERT INTO tracks (
            hash, storage_path, original_filename,
            title, album, artist, album_artist,
            track_no, disc_no, duration_ms, year,
            bitrate, sample_rate, channels,
            file_size, added_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&hash)
    .bind(&storage_rel)
    .bind(&original_filename)
    .bind(&parsed.title)
    .bind(&parsed.album)
    .bind(&parsed.artist)
    .bind(&parsed.album_artist)
    .bind(parsed.track_no)
    .bind(parsed.disc_no)
    .bind(parsed.duration_ms)
    .bind(parsed.year)
    .bind(parsed.bitrate)
    .bind(parsed.sample_rate)
    .bind(parsed.channels)
    .bind(file_size)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(IngestOutcome::Imported)
}

fn filename_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "track".to_string())
}

/// Copies or moves `src` into `<library_root>/.storage/<hash[..2]>/<hash>.<ext>`,
/// creating parent directories as needed, and returns the path relative to
/// the library root. `Move` falls back to copy+remove if `src` is on a
/// different filesystem than the library (rename can't cross devices).
async fn store_file(lib: &Library, src: &Path, hash: &str, mode: CopyMode) -> Result<String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let rel = if ext.is_empty() {
        format!(".storage/{}/{}", &hash[..2], hash)
    } else {
        format!(".storage/{}/{}.{}", &hash[..2], hash, ext)
    };
    let dest = lib.root().join(&rel);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("creating storage dir")?;
    }

    match mode {
        CopyMode::Copy => {
            tokio::fs::copy(src, &dest).await.context("copying into storage")?;
        }
        CopyMode::Move => {
            if tokio::fs::rename(src, &dest).await.is_err() {
                tokio::fs::copy(src, &dest).await.context("copying into storage")?;
                tokio::fs::remove_file(src)
                    .await
                    .context("removing source after copy")?;
            }
        }
    }

    Ok(rel)
}

async fn hash_file(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    let hash = tokio::task::spawn_blocking(move || -> Result<String> {
        let mut file =
            std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).context("reading file")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .context("blocking hash task")??;
    Ok(hash)
}

#[derive(Debug, Default)]
struct Parsed {
    title: Option<String>,
    album: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    track_no: Option<i64>,
    disc_no: Option<i64>,
    duration_ms: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
}

fn parse_file(path: &Path) -> Result<Parsed> {
    let probe = Probe::open(path)
        .with_context(|| format!("probe::open {}", path.display()))?
        .read()
        .with_context(|| format!("probe::read {}", path.display()))?;

    let props = probe.properties();
    let mut out = Parsed {
        duration_ms: Some(props.duration().as_millis() as i64),
        bitrate: props.audio_bitrate().map(|b| b as i64),
        sample_rate: props.sample_rate().map(|s| s as i64),
        channels: props.channels().map(|c| c as i64),
        ..Default::default()
    };

    let tag = probe.primary_tag().or_else(|| probe.first_tag());
    if let Some(tag) = tag {
        out.title = tag.get_string(ItemKey::TrackTitle).map(str::to_owned);
        out.album = tag.get_string(ItemKey::AlbumTitle).map(str::to_owned);
        out.artist = tag.get_string(ItemKey::TrackArtist).map(str::to_owned);
        out.album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::to_owned);
        out.track_no = tag
            .get_string(ItemKey::TrackNumber)
            .and_then(|s| s.split('/').next())
            .and_then(|s| s.trim().parse().ok());
        out.disc_no = tag
            .get_string(ItemKey::DiscNumber)
            .and_then(|s| s.split('/').next())
            .and_then(|s| s.trim().parse().ok());
        out.year = tag
            .get_string(ItemKey::Year)
            .and_then(|s| s.trim().parse().ok())
            .or_else(|| {
                tag.get_string(ItemKey::RecordingDate)
                    .and_then(|s| s.get(..4))
                    .and_then(|y| y.parse().ok())
            });
    }

    Ok(out)
}
