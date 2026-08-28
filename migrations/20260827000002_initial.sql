-- Baseline schema for a single library's own database. Every library gets
-- its own copy of this schema in its own `library.db` file (see
-- libraries::open_all) — there is no shared `libraries` table and no
-- `library_id` column anywhere, since a library's identity is simply which
-- db file a query runs against.
--
-- Tracks are content-addressed: `hash` (sha256) identifies a file's
-- content, `storage_path` is where it lives under this library's
-- `.storage/` folder, populated by `muserv import` (see ingest.rs).

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
