//! SQLite schema (`docs/requirements.md` §10.12/§10.20).
//!
//! The first 11 tables are the §10.12 schema, transcribed verbatim from the
//! requirements document so the two stay in lockstep. `reconstruction_run`/
//! `reconstruction_item` (§10.20's third "run" concept, alongside `scan_run`/
//! `check_run`) were added in P11 once the reconstruction feature itself needed them.

use rusqlite::{Connection, Result};

pub const SCHEMA_SQL: &str = r#"
-- 1. 横断設定
CREATE TABLE app_setting (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- 2. リムーバブルメディア識別（§10.4）
CREATE TABLE removable_media (
    id                INTEGER PRIMARY KEY,
    platform          TEXT NOT NULL CHECK (platform IN ('windows','macos','linux')),
    identifier_type   TEXT NOT NULL,
    identifier_value  TEXT NOT NULL,
    display_name      TEXT,
    first_seen_at     INTEGER NOT NULL,
    last_seen_at      INTEGER NOT NULL,
    UNIQUE (platform, identifier_type, identifier_value)
) STRICT;

-- 3. 情報取得フェーズ（§10.3/§10.8）
CREATE TABLE scan_run (
    id                 INTEGER PRIMARY KEY,
    target_type        TEXT NOT NULL CHECK (target_type IN ('folder','removable_media')),
    folder_path        TEXT,
    removable_media_id INTEGER REFERENCES removable_media(id) ON DELETE RESTRICT,
    hash_mode          TEXT NOT NULL DEFAULT 'lazy' CHECK (hash_mode IN ('lazy','eager')),
    status             TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running','completed','failed','cancelled')),
    started_at         INTEGER NOT NULL,
    completed_at       INTEGER,
    error_message      TEXT,
    CHECK (
        (target_type = 'folder'          AND folder_path IS NOT NULL AND removable_media_id IS NULL)
        OR
        (target_type = 'removable_media' AND removable_media_id IS NOT NULL AND folder_path IS NULL)
    )
) STRICT;

CREATE INDEX idx_scan_run_media  ON scan_run(removable_media_id, status, completed_at DESC);
CREATE INDEX idx_scan_run_folder ON scan_run(folder_path, status, completed_at DESC);

-- 4. 走査結果（通常ファイル・アーカイブ内エントリ, §3.3/§10.5/§10.6）
CREATE TABLE scanned_file (
    id                     INTEGER PRIMARY KEY,
    scan_run_id            INTEGER NOT NULL REFERENCES scan_run(id) ON DELETE CASCADE,
    path                   TEXT NOT NULL,
    parent_archive_file_id INTEGER REFERENCES scanned_file(id) ON DELETE CASCADE,
    archive_format         TEXT,
    archive_depth          INTEGER NOT NULL DEFAULT 0 CHECK (archive_depth >= 0),
    size                   INTEGER NOT NULL CHECK (size >= 0),
    mtime                  INTEGER,
    crc32                  INTEGER,
    md5                    BLOB,
    sha1                   BLOB,
    sha256                 BLOB,
    status                 TEXT NOT NULL DEFAULT 'ok' CHECK (status IN ('ok','error','skipped')),
    error_message          TEXT,
    scanned_at             INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_scanned_file_run        ON scanned_file(scan_run_id);
CREATE INDEX idx_scanned_file_run_path   ON scanned_file(scan_run_id, path);
CREATE INDEX idx_scanned_file_size_crc32 ON scanned_file(size, crc32);
CREATE INDEX idx_scanned_file_sha256     ON scanned_file(sha256);
CREATE INDEX idx_scanned_file_parent     ON scanned_file(parent_archive_file_id);

-- 5. お手本セット（§3.4/§8）
CREATE TABLE reference_set (
    id                          INTEGER PRIMARY KEY,
    name                        TEXT NOT NULL,
    source_format               TEXT NOT NULL,
    source_path                 TEXT,
    generated_from_scan_run_id  INTEGER REFERENCES scan_run(id) ON DELETE SET NULL,
    supersedes_reference_set_id INTEGER REFERENCES reference_set(id) ON DELETE SET NULL,
    created_at                  INTEGER NOT NULL,
    UNIQUE (supersedes_reference_set_id)
) STRICT;

CREATE INDEX idx_reference_set_name ON reference_set(name, created_at DESC);

CREATE TABLE reference_file (
    id               INTEGER PRIMARY KEY,
    reference_set_id INTEGER NOT NULL REFERENCES reference_set(id) ON DELETE CASCADE,
    path             TEXT NOT NULL,
    size             INTEGER NOT NULL CHECK (size >= 0),
    crc32            INTEGER,
    md5              BLOB,
    sha1             BLOB,
    sha256           BLOB,
    UNIQUE (reference_set_id, path)
) STRICT;

CREATE INDEX idx_reference_file_set_size ON reference_file(reference_set_id, size);
CREATE INDEX idx_reference_file_sha256   ON reference_file(sha256);

-- 6. 比較実行（§10.3）— 複数の scan_run を束ねられる
CREATE TABLE check_run (
    id                INTEGER PRIMARY KEY,
    check_type        TEXT NOT NULL CHECK (check_type IN ('integrity','duplicate')),
    reference_set_id  INTEGER REFERENCES reference_set(id) ON DELETE RESTRICT,
    status            TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running','completed','failed','cancelled')),
    started_at        INTEGER NOT NULL,
    completed_at      INTEGER,
    error_message     TEXT,
    CHECK (
        (check_type = 'integrity' AND reference_set_id IS NOT NULL)
        OR
        (check_type = 'duplicate' AND reference_set_id IS NULL)
    )
) STRICT;

CREATE TABLE check_run_source (
    id           INTEGER PRIMARY KEY,
    check_run_id INTEGER NOT NULL REFERENCES check_run(id) ON DELETE CASCADE,
    scan_run_id  INTEGER NOT NULL REFERENCES scan_run(id) ON DELETE RESTRICT,
    UNIQUE (check_run_id, scan_run_id)
) STRICT;

CREATE INDEX idx_check_run_source_check ON check_run_source(check_run_id);
CREATE INDEX idx_check_run_source_scan  ON check_run_source(scan_run_id);

-- 7. 整合性チェック結果（§3.1/§10.11）
CREATE TABLE integrity_check_result (
    id                INTEGER PRIMARY KEY,
    check_run_id      INTEGER NOT NULL REFERENCES check_run(id) ON DELETE CASCADE,
    reference_file_id INTEGER REFERENCES reference_file(id) ON DELETE CASCADE,
    scanned_file_id   INTEGER REFERENCES scanned_file(id) ON DELETE CASCADE,
    result_status     TEXT NOT NULL CHECK (result_status IN ('ok','corrupted','missing','extra','error')),
    detail            TEXT,
    CHECK (reference_file_id IS NOT NULL OR scanned_file_id IS NOT NULL)
) STRICT;

CREATE INDEX idx_integrity_result_run     ON integrity_check_result(check_run_id);
CREATE INDEX idx_integrity_result_ref     ON integrity_check_result(reference_file_id);
CREATE INDEX idx_integrity_result_scanned ON integrity_check_result(scanned_file_id);
CREATE INDEX idx_integrity_result_status  ON integrity_check_result(check_run_id, result_status);

-- 8. 重複チェック結果（§3.2）
CREATE TABLE duplicate_group (
    id           INTEGER PRIMARY KEY,
    check_run_id INTEGER NOT NULL REFERENCES check_run(id) ON DELETE CASCADE,
    sha256       BLOB NOT NULL,
    size         INTEGER NOT NULL,
    member_count INTEGER NOT NULL DEFAULT 0,
    UNIQUE (check_run_id, sha256)
) STRICT;

CREATE INDEX idx_duplicate_group_run ON duplicate_group(check_run_id);

CREATE TABLE duplicate_group_member (
    id                  INTEGER PRIMARY KEY,
    duplicate_group_id  INTEGER NOT NULL REFERENCES duplicate_group(id) ON DELETE CASCADE,
    scanned_file_id     INTEGER NOT NULL REFERENCES scanned_file(id) ON DELETE CASCADE,
    UNIQUE (duplicate_group_id, scanned_file_id)
) STRICT;

CREATE INDEX idx_dup_member_group ON duplicate_group_member(duplicate_group_id);
CREATE INDEX idx_dup_member_file  ON duplicate_group_member(scanned_file_id);

-- 9. 再構成（書き出し）実行（§10.20）— scan_run/check_run とは別の第三の実行単位
CREATE TABLE reconstruction_run (
    id                       INTEGER PRIMARY KEY,
    check_run_id             INTEGER NOT NULL REFERENCES check_run(id) ON DELETE RESTRICT,
    destination_folder_path  TEXT NOT NULL,
    status                   TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running','completed','failed','cancelled')),
    started_at               INTEGER NOT NULL,
    completed_at             INTEGER,
    error_message            TEXT
) STRICT;

CREATE INDEX idx_reconstruction_run_check ON reconstruction_run(check_run_id);

CREATE TABLE reconstruction_item (
    id                          INTEGER PRIMARY KEY,
    reconstruction_run_id       INTEGER NOT NULL REFERENCES reconstruction_run(id) ON DELETE CASCADE,
    integrity_check_result_id   INTEGER NOT NULL REFERENCES integrity_check_result(id) ON DELETE RESTRICT,
    status                      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','written','error')),
    error_message               TEXT,
    written_at                  INTEGER,
    UNIQUE (reconstruction_run_id, integrity_check_result_id)
) STRICT;

CREATE INDEX idx_reconstruction_item_run    ON reconstruction_item(reconstruction_run_id);
CREATE INDEX idx_reconstruction_item_status ON reconstruction_item(reconstruction_run_id, status);
"#;

/// Creates all tables/indexes. Assumes an empty database; callers that need
/// idempotency (re-opening an existing DB file) should check for existing tables
/// first — that migration-versioning concern is out of scope until a second schema
/// revision actually exists.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
}
