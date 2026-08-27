-- Move from path-based scanning to content-addressed storage (see ingest.rs):
-- tracks are now identified by a sha256 `hash` and live at `storage_path`
-- inside `<library_root>/.storage/`, copied/moved there on import rather
-- than read in place from wherever they happened to sit on disk.
--
-- This migration only *relaxes* the schema (repurposes the already-unused
-- `content_hash` column as `hash`, adds `storage_path`/`original_filename`,
-- drops the NOT NULL on `path`/`mtime`) so existing rows keep loading. It
-- does NOT touch any files or compute hashes — that happens the next time
-- `muserv import` runs (see main.rs), which updates each legacy row in
-- place (same id, so playlists/tags keep working) once it's copied that
-- track's file into `.storage`. `path`/`mtime` are left in place, unused,
-- as a breadcrumb back to each track's pre-migration location.

PRAGMA foreign_keys = OFF;

CREATE TABLE tracks_new (
    id                INTEGER PRIMARY KEY,
    library_id        INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    hash              TEXT,
    storage_path      TEXT,
    original_filename TEXT,
    path              TEXT,
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
    mtime         INTEGER,
    added_at      INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    UNIQUE (library_id, hash)
);

INSERT INTO tracks_new (
    id, library_id, hash, storage_path, original_filename, path,
    title, album, artist, album_artist,
    track_no, disc_no, duration_ms, year, bitrate, sample_rate, channels,
    file_size, mtime, added_at, updated_at
)
SELECT id, library_id, content_hash, NULL, NULL, path,
       title, album, artist, album_artist,
       track_no, disc_no, duration_ms, year, bitrate, sample_rate, channels,
       file_size, mtime, added_at, updated_at
FROM tracks;

DROP TABLE tracks;
ALTER TABLE tracks_new RENAME TO tracks;

CREATE INDEX tracks_library_idx      ON tracks(library_id);
CREATE INDEX tracks_album_idx        ON tracks(album);
CREATE INDEX tracks_artist_idx       ON tracks(artist);
CREATE INDEX tracks_album_artist_idx ON tracks(album_artist);
CREATE INDEX tracks_title_idx        ON tracks(title);

PRAGMA foreign_keys = ON;
