//! Archive reading (`docs/requirements.md` §10.5/§10.6, P7): zip and 7z, including
//! their zstd-compressed variants, both required for integrity/duplicate checking
//! (§3.3). This module only knows how to list an archive's entries and extract one
//! entry's bytes — the recursive nesting policy (max depth, §10.6) and how a nested
//! entry's identity maps to a `scanned_file` row live in `scan`/`duplicate`/`integrity`,
//! which are the modules that actually walk the tree.
//!
//! **Known asymmetry between the two formats** (accepted for P7, revisit only if it
//! proves a real problem): zip entries are read through a true streaming path, so an
//! entry whose actual decompressed bytes exceed its declared size is caught mid-read
//! (§10.6's "宣言サイズを超えて展開されるケース...エラー扱い", enforced as it happens) and
//! large legitimate entries never need to sit fully in memory. `sevenz-rust2`'s
//! convenience extraction API (`ArchiveReader::read_file`) only offers whole-entry
//! extraction, so the 7z path buffers an entry fully before the same size check runs
//! post hoc rather than aborting mid-stream, and a very large legitimate 7z entry is
//! held entirely in memory while it's read.

use std::collections::HashMap;
use std::io::{self, Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use crate::hash::{hash_file, hash_reader, HashAlgorithm, HashValues};
use crate::retry::{is_retryable_fs_error, retry_io};

/// Default recursion depth limit (§10.6, overridable via the `archive_max_depth`
/// `app_setting`).
pub const DEFAULT_MAX_DEPTH: i64 = 3;

/// Default per-entry decompressed-size cap in bytes: 2 TiB (§10.6, overridable via the
/// `archive_entry_size_limit_bytes` `app_setting`).
pub const DEFAULT_ENTRY_SIZE_LIMIT: u64 = 2 * 1024 * 1024 * 1024 * 1024;

/// The two size-safety knobs from §10.6, read from `app_setting` with the defaults
/// above when unset (schema.rs's own comment anticipates exactly these two keys).
pub struct ArchiveConfig {
    pub max_depth: i64,
    pub entry_size_limit: u64,
}

impl ArchiveConfig {
    pub fn from_settings(conn: &crate::db::Connection) -> crate::db::Result<Self> {
        let max_depth = crate::db::repo::get_app_setting(conn, "archive_max_depth")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_DEPTH);
        let entry_size_limit =
            crate::db::repo::get_app_setting(conn, "archive_entry_size_limit_bytes")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_ENTRY_SIZE_LIMIT);
        Ok(Self {
            max_depth,
            entry_size_limit,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
}

impl ArchiveFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::SevenZ => "7z",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "zip" => Some(ArchiveFormat::Zip),
            "7z" => Some(ArchiveFormat::SevenZ),
            _ => None,
        }
    }

    /// Detects the format from a file's extension (§10.5's two supported formats).
    /// Extension-only sniffing, not magic bytes — a documented simplification; a
    /// mislabeled file is simply reported as an open failure rather than misdetected.
    pub fn detect(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "zip" => Some(ArchiveFormat::Zip),
            "7z" => Some(ArchiveFormat::SevenZ),
            _ => None,
        }
    }
}

/// Supplies candidate passwords to try for a given archive format (§10.9's "圧縮ファイル
///形式ごとの個別設定、および複数形式への一括設定"). Implemented by `secrets::UnlockedStore`
/// — kept as a trait here so `archive` itself doesn't need to know about the password
/// store's on-disk format or master-password machinery, only "a source of strings to
/// try for this format".
/// `Send + Sync` because hashing runs candidates in parallel across archive-nested
/// entries (rayon, §4) — the same `&dyn PasswordCandidates` is shared across threads.
pub trait PasswordCandidates: Send + Sync {
    /// Passwords to try, in the order they should be tried, for `format`. Typically
    /// format-specific entries first (more likely correct) then entries registered for
    /// every format.
    fn candidates(&self, format: ArchiveFormat) -> Vec<String>;
}

/// How to handle a password-protected archive/entry (§10.7).
pub enum PasswordPolicy<'a> {
    /// §10.7 mode 1: never attempt decryption; a password-protected entry is an error.
    Reject,
    /// §10.7 mode 2: try every candidate this source returns for the format being
    /// opened, in order, until one succeeds; if none do (or there are none), it's the
    /// same error as `Reject`.
    TryRegistered(&'a dyn PasswordCandidates),
}

/// True for the specific "this archive/entry needs a password we don't have" failure —
/// as opposed to "wrong password" being indistinguishable from "corrupted" for zip
/// (§10.7 doesn't require telling those apart: both end up `error`, never `missing` or
/// `corrupted`, per §10.11).
fn zip_needs_password(err: &zip::result::ZipError) -> bool {
    matches!(
        err,
        zip::result::ZipError::UnsupportedArchive(zip::result::ZipError::PASSWORD_REQUIRED)
    )
}

fn sevenz_needs_password(err: &sevenz_rust2::Error) -> bool {
    matches!(
        err,
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_)
    )
}

pub struct ArchiveEntryMeta {
    pub name: String,
    pub size: u64,
}

/// Lists an archive's file entries (directories excluded) without decompressing any
/// entry content — cheap enough to run during the scan/info-gathering phase (§10.3).
///
/// `policy` only matters for a 7z whose *header* itself is password-encrypted (zip's
/// central directory, unlike its entry content, is never encrypted, so listing a zip
/// never needs a password regardless of `policy`).
pub fn list_entries<R: Read + Seek>(
    format: ArchiveFormat,
    reader: R,
    policy: &PasswordPolicy,
) -> io::Result<Vec<ArchiveEntryMeta>> {
    match format {
        ArchiveFormat::Zip => {
            let mut archive = zip::ZipArchive::new(reader).map_err(to_io_error)?;
            let mut entries = Vec::with_capacity(archive.len());
            for i in 0..archive.len() {
                // `by_index_raw`, not `by_index`: this only needs each entry's
                // metadata (name/size), never its content, so it must not trip the
                // password-required check that `by_index` applies even just to open a
                // `ZipFile` handle — an encrypted entry's *name and size* are still
                // plain central-directory metadata (§10.7 only concerns content).
                let f = archive.by_index_raw(i).map_err(to_io_error)?;
                if f.is_dir() {
                    continue;
                }
                entries.push(ArchiveEntryMeta {
                    name: f.name().to_string(),
                    size: f.size(),
                });
            }
            Ok(entries)
        }
        ArchiveFormat::SevenZ => {
            let mut reader = reader;
            let mut last_err = None;
            for password in sevenz_password_attempts(policy) {
                if reader.seek(io::SeekFrom::Start(0)).is_err() {
                    break;
                }
                match sevenz_rust2::ArchiveReader::new(&mut reader, to_sevenz_password(&password)) {
                    Ok(archive) => {
                        return Ok(archive
                            .archive()
                            .files
                            .iter()
                            .filter(|f| !f.is_directory && f.has_stream)
                            .map(|f| ArchiveEntryMeta {
                                name: f.name.clone(),
                                size: f.size,
                            })
                            .collect());
                    }
                    Err(e) if sevenz_needs_password(&e) => {
                        last_err = Some(to_io_error(e));
                    }
                    Err(e) => return Err(to_io_error(e)),
                }
            }
            Err(last_err.unwrap_or_else(|| io::Error::other("パスワード保護された7zファイルです")))
        }
    }
}

/// Extracts one entry's bytes by name. `declared_size` is the size recorded for this
/// entry by `list_entries` (persisted on the entry's own `scanned_file.size`); actual
/// decompressed output exceeding it is treated as corruption/tampering (§10.6) and
/// returned as an error, not silently truncated or accepted.
///
/// Both formats first try with no password at all — an entry/archive that isn't
/// actually encrypted reads the same as before P10 regardless of `policy`. Only once
/// that specifically fails because a password is needed does `policy` matter: `Reject`
/// (§10.7 mode 1) turns that into a plain error; `TryRegistered` (mode 2) retries with
/// each candidate `policy` offers for `format` until one works, erroring the same way
/// as `Reject` if none do.
pub fn read_entry_bytes<R: Read + Seek>(
    format: ArchiveFormat,
    reader: R,
    entry_name: &str,
    declared_size: u64,
    policy: &PasswordPolicy,
) -> io::Result<Vec<u8>> {
    match format {
        ArchiveFormat::Zip => {
            let mut archive = zip::ZipArchive::new(reader).map_err(to_io_error)?;
            // Deliberately two separate statements, not one match with a
            // `by_name_decrypt` retry nested inside an `Err` arm: a match scrutinee's
            // borrow of `archive` (here, from `by_name`) lives for the whole match
            // statement, so a second mutable borrow of `archive` (for
            // `by_name_decrypt`) can't happen inside one of its own arms. Extracting
            // just the owned `ZipError` out of the first match ends that borrow
            // cleanly before the retry loop below needs its own.
            let first_err = match archive.by_name(entry_name) {
                Ok(file) => return read_bounded(file, declared_size),
                Err(e) => e,
            };
            if !zip_needs_password(&first_err) {
                return Err(to_io_error(first_err));
            }
            let PasswordPolicy::TryRegistered(source) = policy else {
                return Err(to_io_error(first_err));
            };
            for candidate in source.candidates(ArchiveFormat::Zip) {
                if let Ok(file) = archive.by_name_decrypt(entry_name, candidate.as_bytes()) {
                    return read_bounded(file, declared_size);
                }
            }
            Err(to_io_error(first_err))
        }
        ArchiveFormat::SevenZ => {
            let mut reader = reader;
            let mut last_err = None;
            for password in sevenz_password_attempts(policy) {
                if reader.seek(io::SeekFrom::Start(0)).is_err() {
                    break;
                }
                let archive = match sevenz_rust2::ArchiveReader::new(
                    &mut reader,
                    to_sevenz_password(&password),
                ) {
                    Ok(archive) => archive,
                    Err(e) if sevenz_needs_password(&e) => {
                        last_err = Some(to_io_error(e));
                        continue;
                    }
                    Err(e) => return Err(to_io_error(e)),
                };
                let mut archive = archive;
                match archive.read_file(entry_name) {
                    Ok(bytes) => {
                        if bytes.len() as u64 > declared_size {
                            return Err(io::Error::other(format!(
                                "展開結果が宣言サイズ({declared_size}バイト)を超過しました（{}バイト）",
                                bytes.len()
                            )));
                        }
                        return Ok(bytes);
                    }
                    Err(e) if sevenz_needs_password(&e) => {
                        last_err = Some(to_io_error(e));
                    }
                    Err(e) => return Err(to_io_error(e)),
                }
            }
            Err(last_err.unwrap_or_else(|| io::Error::other("パスワード保護された7zファイルです")))
        }
    }
}

/// The password attempts to make for a 7z archive/entry, in order: no password first
/// (`None`, so an unencrypted 7z never even looks at `policy`), then — only relevant if
/// that's rejected — every candidate `policy` offers for `ArchiveFormat::SevenZ` when
/// it's `TryRegistered` (nothing further to try for `Reject`).
fn sevenz_password_attempts(policy: &PasswordPolicy) -> Vec<Option<String>> {
    let mut attempts = vec![None];
    if let PasswordPolicy::TryRegistered(source) = policy {
        attempts.extend(
            source
                .candidates(ArchiveFormat::SevenZ)
                .into_iter()
                .map(Some),
        );
    }
    attempts
}

fn to_sevenz_password(candidate: &Option<String>) -> sevenz_rust2::Password {
    match candidate {
        None => sevenz_rust2::Password::empty(),
        Some(s) => sevenz_rust2::Password::from(s.as_str()),
    }
}

/// Reads all of `reader` into memory, erroring as soon as more than `limit` bytes have
/// been produced — the streaming form of the §10.6 declared-size check.
fn read_bounded<R: Read>(mut reader: R, limit: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len() as u64 + n as u64 > limit {
            return Err(io::Error::other(format!(
                "展開結果が宣言サイズ({limit}バイト)を超過しました"
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

fn to_io_error<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// Minimal view of a `scanned_file` row needed to resolve an archive-nested entry's
/// bytes back to a filesystem path plus a chain of archive hops. Implemented by both
/// `repo::ScannedFileForDuplicate` and `repo::ScannedFileForIntegrity` (in `db::repo`,
/// where the fields naturally live) so `resolve_hops` is written once despite the two
/// comparison phases fetching different row shapes.
pub trait ScannedEntry {
    fn path(&self) -> &str;
    fn size(&self) -> i64;
    fn folder_path(&self) -> &str;
    fn parent_archive_file_id(&self) -> Option<i64>;
    fn archive_format(&self) -> Option<&str>;
}

/// One step of descending from a container archive into a nested entry.
pub struct EntryHop {
    pub container_format: ArchiveFormat,
    pub entry_name: String,
    pub declared_size: u64,
}

/// Walks `parent_archive_file_id` from `leaf_id` up to its root real file using only
/// the already-fetched `by_id` map (no extra DB round trips), returning that root's
/// filesystem path plus the ordered chain of archive hops needed to reach the leaf. An
/// empty hop list means `leaf_id` is itself a depth-0 regular file.
pub fn resolve_hops<T: ScannedEntry>(
    leaf_id: i64,
    by_id: &HashMap<i64, T>,
) -> (PathBuf, Vec<EntryHop>) {
    let mut hops = Vec::new();
    let mut current = &by_id[&leaf_id];
    while let Some(parent_id) = current.parent_archive_file_id() {
        let parent = &by_id[&parent_id];
        // `path` is built as `parent.path + "/" + own_name` at scan time
        // (scan::archive_walk), so this is always a valid slice boundary.
        let own_name = current.path()[parent.path().len() + 1..].to_string();
        let parent_format = ArchiveFormat::parse_str(
            parent
                .archive_format()
                .expect("a nested entry's parent is always recorded as an archive"),
        )
        .expect("archive_format always holds a format ArchiveFormat::parse_str recognizes");
        hops.push(EntryHop {
            container_format: parent_format,
            entry_name: own_name,
            declared_size: current.size() as u64,
        });
        current = parent;
    }
    hops.reverse();
    let root_path = Path::new(current.folder_path()).join(current.path());
    (root_path, hops)
}

/// Hashes a `scanned_file` (regular or archive-nested) identified by `root_path` +
/// `hops` from `resolve_hops`. An empty `hops` means `root_path` itself is the target,
/// hashed the same retrying, single-open way as any other plain file. `policy` (§10.7)
/// applies uniformly at every nesting level — a nested archive-in-an-archive doesn't
/// get a different password policy per level, only per format within a level.
pub fn hash_entry(
    root_path: &Path,
    hops: &[EntryHop],
    algorithms: &[HashAlgorithm],
    policy: &PasswordPolicy,
) -> io::Result<HashValues> {
    if hops.is_empty() {
        return hash_file(root_path, algorithms);
    }
    let file = retry_io(|| std::fs::File::open(root_path), is_retryable_fs_error)?;
    let mut bytes = read_entry_bytes(
        hops[0].container_format,
        file,
        &hops[0].entry_name,
        hops[0].declared_size,
        policy,
    )?;
    for hop in &hops[1..] {
        bytes = read_entry_bytes(
            hop.container_format,
            Cursor::new(bytes),
            &hop.entry_name,
            hop.declared_size,
            policy,
        )?;
    }
    hash_reader(Cursor::new(bytes), algorithms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Cursor, Write};

    fn build_zip(entries: &[(&str, &[u8], zip::CompressionMethod)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, method) in entries {
            let options = zip::write::SimpleFileOptions::default().compression_method(*method);
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn build_7z(entries: &[(&str, &[u8])], zstd: bool) -> Vec<u8> {
        let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        if zstd {
            writer.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::new(
                sevenz_rust2::EncoderMethod::ZSTD,
            )]);
        }
        for (name, data) in entries {
            writer
                .push_archive_entry(
                    sevenz_rust2::ArchiveEntry::new_file(name),
                    Some(Cursor::new(*data)),
                )
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_lists_and_reads_stored_deflated_and_zstd_entries() {
        let a: &[u8] = b"hello world, stored";
        let b: &[u8] = b"hello world, deflated, deflated, deflated, deflated";
        let c: &[u8] = b"hello world, zstd compressed, zstd compressed, zstd";
        let bytes = build_zip(&[
            ("a.txt", a, zip::CompressionMethod::Stored),
            ("b.txt", b, zip::CompressionMethod::Deflated),
            ("c.txt", c, zip::CompressionMethod::Zstd),
        ]);

        let entries = list_entries(
            ArchiveFormat::Zip,
            Cursor::new(bytes.clone()),
            &PasswordPolicy::Reject,
        )
        .unwrap();
        let sizes: HashMap<String, u64> = entries.into_iter().map(|e| (e.name, e.size)).collect();
        assert_eq!(sizes["a.txt"], a.len() as u64);
        assert_eq!(sizes["b.txt"], b.len() as u64);
        assert_eq!(sizes["c.txt"], c.len() as u64);

        for (name, expected) in [("a.txt", a), ("b.txt", b), ("c.txt", c)] {
            let got = read_entry_bytes(
                ArchiveFormat::Zip,
                Cursor::new(bytes.clone()),
                name,
                expected.len() as u64,
                &PasswordPolicy::Reject,
            )
            .unwrap();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn zip_read_aborts_when_declared_size_is_understated() {
        let data: &[u8] = b"this entry is longer than the declared size claims";
        let bytes = build_zip(&[("a.txt", data, zip::CompressionMethod::Stored)]);
        let result = read_entry_bytes(
            ArchiveFormat::Zip,
            Cursor::new(bytes),
            "a.txt",
            5,
            &PasswordPolicy::Reject,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sevenz_lists_and_reads_lzma2_and_zstd_entries() {
        let a: &[u8] = b"seven zip default lzma2 content";
        let bytes_lzma2 = build_7z(&[("a.txt", a)], false);
        let entries = list_entries(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes_lzma2.clone()),
            &PasswordPolicy::Reject,
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, a.len() as u64);
        let got = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes_lzma2),
            "a.txt",
            a.len() as u64,
            &PasswordPolicy::Reject,
        )
        .unwrap();
        assert_eq!(got, a);

        let b: &[u8] = b"seven zip zstd compressed content, zstd compressed content";
        let bytes_zstd = build_7z(&[("b.txt", b)], true);
        let got = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes_zstd),
            "b.txt",
            b.len() as u64,
            &PasswordPolicy::Reject,
        )
        .unwrap();
        assert_eq!(got, b);
    }

    #[test]
    fn sevenz_read_rejects_understated_declared_size() {
        let data: &[u8] = b"this entry is longer than the declared size claims, much longer";
        let bytes = build_7z(&[("a.txt", data)], false);
        let result = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes),
            "a.txt",
            5,
            &PasswordPolicy::Reject,
        );
        assert!(result.is_err());
    }

    #[test]
    fn detects_format_from_extension_case_insensitively() {
        assert_eq!(
            ArchiveFormat::detect(Path::new("photos/album.ZIP")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::detect(Path::new("photos/album.7z")),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(ArchiveFormat::detect(Path::new("photos/a.jpg")), None);
    }

    #[test]
    fn opening_a_corrupted_archive_fails() {
        let garbage = b"not a real archive".to_vec();
        assert!(list_entries(
            ArchiveFormat::Zip,
            Cursor::new(garbage.clone()),
            &PasswordPolicy::Reject
        )
        .is_err());
        assert!(list_entries(
            ArchiveFormat::SevenZ,
            Cursor::new(garbage),
            &PasswordPolicy::Reject
        )
        .is_err());
    }

    /// A fixed, order-preserving list of passwords to try — a test double for
    /// `secrets::UnlockedStore` that doesn't need a real password store on disk.
    struct FixedCandidates(Vec<&'static str>);

    impl PasswordCandidates for FixedCandidates {
        fn candidates(&self, _format: ArchiveFormat) -> Vec<String> {
            self.0.iter().map(|s| s.to_string()).collect()
        }
    }

    fn build_encrypted_zip(entries: &[(&str, &[u8])], password: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            let options = zip::write::SimpleFileOptions::default()
                .with_aes_encryption(zip::AesMode::Aes256, password);
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_password_protected_entry_is_an_error_under_reject_policy() {
        let bytes = build_encrypted_zip(&[("secret.txt", b"top secret")], "correct horse");
        let result = read_entry_bytes(
            ArchiveFormat::Zip,
            Cursor::new(bytes),
            "secret.txt",
            10,
            &PasswordPolicy::Reject,
        );
        assert!(result.is_err());
    }

    #[test]
    fn zip_password_protected_entry_decrypts_with_the_matching_registered_password() {
        let data: &[u8] = b"top secret";
        let bytes = build_encrypted_zip(&[("secret.txt", data)], "correct horse");
        // The wrong password comes first, on purpose: a real store may hold several
        // registered passwords, and the right one isn't necessarily tried first.
        let candidates = FixedCandidates(vec!["wrong guess", "correct horse"]);
        let policy = PasswordPolicy::TryRegistered(&candidates);
        let got = read_entry_bytes(
            ArchiveFormat::Zip,
            Cursor::new(bytes),
            "secret.txt",
            data.len() as u64,
            &policy,
        )
        .unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn zip_password_protected_entry_fails_when_no_registered_password_matches() {
        let bytes = build_encrypted_zip(&[("secret.txt", b"top secret")], "correct horse");
        let candidates = FixedCandidates(vec!["wrong guess", "also wrong"]);
        let policy = PasswordPolicy::TryRegistered(&candidates);
        let result = read_entry_bytes(
            ArchiveFormat::Zip,
            Cursor::new(bytes),
            "secret.txt",
            10,
            &policy,
        );
        assert!(result.is_err());
    }

    fn build_encrypted_7z(entries: &[(&str, &[u8])], password: &str) -> Vec<u8> {
        let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        let aes_options = sevenz_rust2::encoder_options::AesEncoderOptions::new(
            sevenz_rust2::Password::from(password),
        );
        writer.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::from(aes_options)]);
        for (name, data) in entries {
            writer
                .push_archive_entry(
                    sevenz_rust2::ArchiveEntry::new_file(name),
                    Some(Cursor::new(*data)),
                )
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn sevenz_password_protected_content_is_an_error_under_reject_policy() {
        let bytes = build_encrypted_7z(&[("secret.txt", b"top secret")], "correct horse");
        let result = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes),
            "secret.txt",
            10,
            &PasswordPolicy::Reject,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sevenz_password_protected_content_decrypts_with_the_matching_registered_password() {
        let data: &[u8] = b"top secret";
        let bytes = build_encrypted_7z(&[("secret.txt", data)], "correct horse");
        let candidates = FixedCandidates(vec!["wrong guess", "correct horse"]);
        let policy = PasswordPolicy::TryRegistered(&candidates);
        let got = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes),
            "secret.txt",
            data.len() as u64,
            &policy,
        )
        .unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn sevenz_password_protected_content_fails_when_no_registered_password_matches() {
        let bytes = build_encrypted_7z(&[("secret.txt", b"top secret")], "correct horse");
        let candidates = FixedCandidates(vec!["wrong guess"]);
        let policy = PasswordPolicy::TryRegistered(&candidates);
        let result = read_entry_bytes(
            ArchiveFormat::SevenZ,
            Cursor::new(bytes),
            "secret.txt",
            10,
            &policy,
        );
        assert!(result.is_err());
    }
}
