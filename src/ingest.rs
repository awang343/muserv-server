use anyhow::{Context, Result};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
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

impl ImportStats {
    pub fn merge(mut self, other: ImportStats) -> Self {
        self.scanned += other.scanned;
        self.imported += other.imported;
        self.duplicates += other.duplicates;
        self.failed += other.failed;
        self.failures.extend(other.failures);
        self
    }
}

#[derive(Debug)]
pub struct ImportFailure {
    pub path: PathBuf,
    pub reason: String,
}

/// Walks `dir` recursively and ingests every audio file found, skipping the
/// library's own `.storage` directory (already-ingested files live there).
/// Used both for ad-hoc bulk imports of a folder of new music and, pointed
/// at a library's own root, to pick up files that were never tracked at all
/// (e.g. dropped in by hand). Safe to re-run: files whose content already
/// exists in the library (by hash) are reported as duplicates rather than
/// re-imported.
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

/// Migrates rows left over from the old path-based scanner (`storage_path`
/// still NULL): hashes the file at each row's legacy `path`, copies/moves it
/// into `.storage`, and updates the row *in place* — the track id doesn't
/// change, so playlists and tags that already reference it keep working.
/// If two legacy rows turn out to have identical content, the second one
/// processed is dropped (its playlist/tag associations are lost — this is
/// content dedup, same outcome a fresh import of a duplicate file would
/// produce).
pub async fn migrate_legacy_rows(pool: &SqlitePool, lib: &Library, mode: CopyMode) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, path FROM tracks WHERE library_id = ? AND storage_path IS NULL AND path IS NOT NULL",
    )
    .bind(lib.id)
    .fetch_all(pool)
    .await?;

    for (track_id, path) in rows {
        stats.scanned += 1;
        let src = PathBuf::from(&path);
        match migrate_one(pool, lib, track_id, &src, mode).await {
            Ok(true) => stats.imported += 1,
            Ok(false) => stats.duplicates += 1,
            Err(e) => {
                warn!(track_id, path = %path, error = ?e, "legacy migration failed");
                stats.failed += 1;
                stats.failures.push(ImportFailure {
                    path: src,
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
        "legacy migration complete"
    );
    Ok(stats)
}

/// Returns `true` if the row was migrated in place, `false` if it was
/// dropped as a duplicate of another (already-migrated or already-processed)
/// row in the same library.
async fn migrate_one(
    pool: &SqlitePool,
    lib: &Library,
    track_id: i64,
    src: &Path,
    mode: CopyMode,
) -> Result<bool> {
    let hash = hash_file(src).await?;

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE library_id = ? AND hash = ? AND id != ?")
            .bind(lib.id)
            .bind(&hash)
            .bind(track_id)
            .fetch_optional(pool)
            .await?;

    if existing.is_some() {
        sqlx::query("DELETE FROM tracks WHERE id = ?")
            .bind(track_id)
            .execute(pool)
            .await?;
        return Ok(false);
    }

    let storage_rel = store_file(lib, src, &hash, mode).await?;
    let original_filename = filename_of(src);
    let file_size = tokio::fs::metadata(lib.root().join(&storage_rel)).await?.len() as i64;

    sqlx::query(
        "UPDATE tracks SET hash = ?, storage_path = ?, original_filename = ?, file_size = ? WHERE id = ?",
    )
    .bind(&hash)
    .bind(&storage_rel)
    .bind(&original_filename)
    .bind(file_size)
    .bind(track_id)
    .execute(pool)
    .await?;

    Ok(true)
}

/// Hashes, tag-parses, and inserts one new track from `src` into `lib`.
/// Returns `Duplicate` (without touching `src`) if a track with the same
/// content hash already exists in the library.
async fn ingest_file(pool: &SqlitePool, lib: &Library, src: &Path, mode: CopyMode) -> Result<IngestOutcome> {
    let hash = hash_file(src).await?;

    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM tracks WHERE library_id = ? AND hash = ?")
        .bind(lib.id)
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
            library_id, hash, storage_path, original_filename,
            title, album, artist, album_artist,
            track_no, disc_no, duration_ms, year,
            bitrate, sample_rate, channels,
            file_size, added_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(lib.id)
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
