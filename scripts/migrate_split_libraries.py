#!/usr/bin/env python3
"""One-time migration: split the old shared muserv db (one `libraries` table,
every table scoped by `library_id`) into a per-library `library.db` inside
each library's own folder, matching the new server layout where each
library owns `<path>/library.db` + `<path>/.storage/` with no library_id
column anywhere.

Run this ONCE on the machine holding the old db, before starting the new
server binary for the first time.

    python3 migrate_split_libraries.py --old-db /path/to/music-lib.db --config /path/to/config.toml --dry-run
    python3 migrate_split_libraries.py --old-db /path/to/music-lib.db --config /path/to/config.toml

The old db is opened read-only; nothing under `.storage/` is touched (it
already lives inside each library's own folder from the earlier
content-addressed-storage migration, so no files need to move). Tracks
that were never hashed yet (hash IS NULL — i.e. `muserv import` hadn't
picked them up yet under the old code) are skipped; run `muserv import`
against the old shared setup first if you need those carried over, or just
let the new per-library `muserv import` pick them up fresh afterwards
(they'll get new ids, so any playlist/tag entries pointing at them today
will not carry over for those specific tracks).
"""
import argparse
import hashlib
import pathlib
import sqlite3
import sys
import tomllib

MIGRATIONS_DIR = pathlib.Path(__file__).resolve().parent.parent / "migrations"
BASELINE_MIGRATION = MIGRATIONS_DIR / "20260827000002_initial.sql"
BASELINE_VERSION = 20260827000002
BASELINE_DESCRIPTION = "initial"

SCHEMA = """
PRAGMA foreign_keys = ON;

CREATE TABLE tracks (
    id                INTEGER PRIMARY KEY,
    hash              TEXT UNIQUE,
    storage_path      TEXT,
    original_filename TEXT,
    title         TEXT,
    album         TEXT,
    artist        TEXT,
    album_artist  TEXT,
    track_no      INTEGER,
    disc_no       INTEGER,
    duration_ms   INTEGER,
    year          INTEGER,
    bitrate       INTEGER,
    sample_rate   INTEGER,
    channels      INTEGER,
    file_size     INTEGER NOT NULL,
    added_at      INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX tracks_album_idx        ON tracks(album);
CREATE INDEX tracks_artist_idx       ON tracks(artist);
CREATE INDEX tracks_album_artist_idx ON tracks(album_artist);
CREATE INDEX tracks_title_idx        ON tracks(title);

CREATE TABLE tags (
    id        INTEGER PRIMARY KEY,
    namespace TEXT NOT NULL,
    value     TEXT NOT NULL,
    UNIQUE(namespace, value)
);

CREATE INDEX tags_value_idx ON tags(value);

CREATE TABLE track_tags (
    track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    tag_id    INTEGER NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (track_id, tag_id)
);

CREATE INDEX track_tags_tag_idx ON track_tags(tag_id);

CREATE TABLE playlists (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE playlist_tracks (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, track_id)
);

CREATE INDEX playlist_tracks_pos_idx ON playlist_tracks(playlist_id, position);
"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--old-db", required=True, type=pathlib.Path)
    ap.add_argument("--config", required=True, type=pathlib.Path)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    cfg = tomllib.loads(args.config.read_text())
    libs = cfg.get("library") or []
    if not libs:
        print("no [[library]] entries found in config", file=sys.stderr)
        return 1

    old = sqlite3.connect(f"file:{args.old_db}?mode=ro", uri=True)
    old_libs = {
        name: (lib_id, root_path)
        for lib_id, name, root_path in old.execute("SELECT id, name, root_path FROM libraries")
    }

    for entry in libs:
        name = entry["name"]
        path = pathlib.Path(entry["path"])
        if name not in old_libs:
            print(f"[{name}] no matching row in old libraries table — skipping (new library?)")
            continue
        old_lib_id, old_root = old_libs[name]
        db_path = path / "library.db"
        if db_path.exists():
            print(f"[{name}] {db_path} already exists — skipping (already migrated?)")
            continue

        n = old.execute(
            "SELECT COUNT(*) FROM tracks WHERE library_id = ? AND hash IS NOT NULL", (old_lib_id,)
        ).fetchone()[0]
        skipped = old.execute(
            "SELECT COUNT(*) FROM tracks WHERE library_id = ? AND hash IS NULL", (old_lib_id,)
        ).fetchone()[0]
        print(f"[{name}] old library_id={old_lib_id} ({old_root}) -> {db_path}")
        print(f"    {n} tracks to migrate, {skipped} skipped (never hashed)")
        if args.dry_run:
            continue

        path.mkdir(parents=True, exist_ok=True)
        new = sqlite3.connect(db_path)
        new.executescript(SCHEMA)
        # Stamp sqlx's own migration-tracking table so it doesn't try to
        # re-apply (and fail on "table already exists") the next time
        # `muserv` starts and runs `sqlx::migrate!()` against this db.
        new.execute(
            """
            CREATE TABLE IF NOT EXISTS _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            )
            """
        )
        checksum = hashlib.sha384(BASELINE_MIGRATION.read_bytes()).digest()
        new.execute(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) "
            "VALUES (?, ?, 1, ?, 0)",
            (BASELINE_VERSION, BASELINE_DESCRIPTION, checksum),
        )
        new.execute("ATTACH DATABASE ? AS old", (str(args.old_db),))

        new.execute(
            """
            INSERT INTO tracks (id, hash, storage_path, original_filename, title, album,
                artist, album_artist, track_no, disc_no, duration_ms, year, bitrate,
                sample_rate, channels, file_size, added_at, updated_at)
            SELECT id, hash, storage_path, original_filename, title, album,
                artist, album_artist, track_no, disc_no, duration_ms, year, bitrate,
                sample_rate, channels, file_size, added_at, updated_at
            FROM old.tracks WHERE library_id = ? AND hash IS NOT NULL
            """,
            (old_lib_id,),
        )
        new.execute(
            """
            INSERT INTO tags (id, namespace, value)
            SELECT DISTINCT t.id, t.namespace, t.value
            FROM old.tags t
            JOIN old.track_tags tt ON tt.tag_id = t.id
            JOIN old.tracks tr ON tr.id = tt.track_id
            WHERE tr.library_id = ? AND tr.hash IS NOT NULL
            """,
            (old_lib_id,),
        )
        new.execute(
            """
            INSERT INTO track_tags (track_id, tag_id, added_at)
            SELECT tt.track_id, tt.tag_id, tt.added_at
            FROM old.track_tags tt
            JOIN old.tracks tr ON tr.id = tt.track_id
            WHERE tr.library_id = ? AND tr.hash IS NOT NULL
            """,
            (old_lib_id,),
        )
        new.execute(
            """
            INSERT INTO playlists (id, name, description, created_at, updated_at)
            SELECT id, name, description, created_at, updated_at
            FROM old.playlists WHERE library_id = ?
            """,
            (old_lib_id,),
        )
        new.execute(
            """
            INSERT INTO playlist_tracks (playlist_id, track_id, position, added_at)
            SELECT pt.playlist_id, pt.track_id, pt.position, pt.added_at
            FROM old.playlist_tracks pt
            JOIN old.playlists p ON p.id = pt.playlist_id
            JOIN old.tracks tr ON tr.id = pt.track_id
            WHERE p.library_id = ? AND tr.hash IS NOT NULL
            """,
            (old_lib_id,),
        )
        new.commit()

        tracks_n, playlists_n = new.execute(
            "SELECT (SELECT COUNT(*) FROM tracks), (SELECT COUNT(*) FROM playlists)"
        ).fetchone()
        print(f"    done: {tracks_n} tracks, {playlists_n} playlists in {db_path}")
        new.close()

    old.close()
    if args.dry_run:
        print("\ndry run only — re-run without --dry-run to actually write the new library.db files")
    else:
        print(
            "\ndone. old db and .storage/ were not modified — verify the new libraries look "
            "right, then start `muserv serve`. Archive/delete the old db once you're satisfied."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
