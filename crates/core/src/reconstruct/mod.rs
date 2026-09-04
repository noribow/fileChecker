//! Reconstruction (`docs/requirements.md` §10.20): rebuilds a destination folder to
//! match a reference set, sourcing each entry from whichever previously-scanned
//! location (folder or removable medium) the priority rule below picks, and
//! reassembling reference entries that belong inside an archive container (zip/7z)
//! into a fresh, deterministically-built archive (§10.19, `archive::deterministic`)
//! rather than writing them out as loose files.
//!
//! Three steps, matching the CLI's `reconstruct plan`/`run`/`status` (§10.16):
//! - [`compute_plan`]: pure DB computation — for every reference file, pick a source
//!   (or record it as unresolved) and record the outcome as a fresh `integrity_check_
//!   result`-backed `check_run`. Never touches the filesystem beyond the scan the
//!   caller already did on the destination.
//! - [`create_run`]: turns a plan's resolved entries into a `reconstruction_run` +
//!   `reconstruction_item` rows (§10.20 — unresolved entries never get a row, so a
//!   partially-fulfillable reference set doesn't block the rest).
//! - [`run_pass`]: does the actual reading and writing for whatever's still
//!   outstanding (`pending` or previously `error`'d) in one connected-media session,
//!   reporting which removable media (if any) are still needed for what's left.
//!
//! **Source-priority rule** (§10.20, decided as a general rule for any `check_run`
//! bundling multiple `scan_run`s, though only reconstruction actually exercises it so
//! far): among every scanned file across the bundle sharing a reference file's SHA-256,
//! prefer (1) the destination's own scan, then (2) any other non-removable (folder)
//! scan, then (3) removable-media scans, most-recently-scanned first.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::archive::{self, ArchiveFormat, PasswordPolicy};
use crate::db::{repo, Connection, ReconstructionItemStatus, Result, ResultStatus, RunStatus};
use crate::hash::HashAlgorithm;

/// One reference file successfully matched to a source (§10.20's priority rule already
/// applied) while planning.
pub struct ResolvedItem {
    pub integrity_check_result_id: i64,
    pub reference_path: String,
    pub scanned_file_id: i64,
    pub scan_run_id: i64,
    pub removable_media_id: Option<i64>,
}

pub struct Plan {
    /// The fresh integrity check_run this plan's results were recorded under.
    pub check_run_id: i64,
    pub resolved: Vec<ResolvedItem>,
    pub missing: Vec<repo::ReferenceFileRow>,
}

impl Plan {
    /// Distinct removable media any resolved entry sources from — what the caller
    /// needs connected, at some point, to fully execute this plan (§10.20's "必要と
    /// なる外部メディア" report).
    pub fn required_removable_media(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .resolved
            .iter()
            .filter_map(|r| r.removable_media_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// Computes a fulfillment plan for `reference_set_id` against every scanned file
/// across `scan_run_ids` (which the caller has already assembled — typically an
/// existing check_run's bundled sources plus a freshly-scanned destination folder,
/// see the CLI's `reconstruct plan`), recording the outcome as a new integrity
/// `check_run` (§10.20's "充当計画の算出"). `destination_scan_run_id` (which must be
/// one of `scan_run_ids`) is what earns the priority rule's top rank.
///
/// Candidate matching is by SHA-256 (§10.20 sources by *content*, not by where a file
/// happens to sit — the whole point is finding a correct copy regardless of its own
/// path). A folder-sourced candidate that hasn't been hashed yet (§10.2/§10.3's lazy
/// path defers this to a comparison phase — reconstruction planning is one) gets
/// hashed here and the result persisted back onto its `scanned_file` row, exactly like
/// the duplicate/integrity comparison phases already do; a removable-media candidate
/// always already has one (§10.8's eager mode), so its medium never needs to be
/// connected just to plan.
pub fn compute_plan(
    conn: &mut Connection,
    reference_set_id: i64,
    scan_run_ids: &[i64],
    destination_scan_run_id: i64,
    policy: &PasswordPolicy,
    started_at: i64,
) -> Result<Plan> {
    let check_run_id = repo::insert_check_run_integrity(conn, reference_set_id, started_at)?;
    for &id in scan_run_ids {
        repo::insert_check_run_source(conn, check_run_id, id)?;
    }

    let mut candidates = repo::list_scanned_files_for_reconstruction(conn, scan_run_ids)?;
    hash_missing_candidates(conn, &mut candidates, policy)?;

    let mut by_sha256: HashMap<&[u8], Vec<&repo::ReconstructionScannedFile>> = HashMap::new();
    for c in &candidates {
        if let Some(sha) = c.sha256.as_deref() {
            by_sha256.entry(sha).or_default().push(c);
        }
    }

    let reference_files = repo::list_reference_files(conn, reference_set_id)?;
    // A reference entry that is itself some *other* reference entry's container
    // ("game.zip", when "game.zip/a.bin" is also in the set) is never sourced
    // directly — it only ever exists as the reassembled result of its own contents,
    // so matching it against a whole-file candidate would be meaningless (nothing
    // will byte-for-byte equal our own freshly-serialized container) and it must not
    // be reported `missing` just because no such candidate exists.
    let container_paths: std::collections::HashSet<String> = reference_files
        .iter()
        .filter_map(|rf| split_container(&rf.path).map(|(container, _)| container.to_string()))
        .collect();

    let mut resolved = Vec::new();
    let mut missing = Vec::new();

    let tx = conn.transaction()?;
    for rf in reference_files {
        if container_paths.contains(rf.path.as_str()) {
            continue;
        }
        let winner = rf
            .sha256
            .as_deref()
            .and_then(|sha| by_sha256.get(sha))
            .and_then(|group| pick_winner(group, destination_scan_run_id));
        match winner {
            Some(w) => {
                let icr_id = repo::insert_integrity_check_result(
                    &tx,
                    check_run_id,
                    Some(rf.id),
                    Some(w.id),
                    ResultStatus::Ok,
                    None,
                )?;
                resolved.push(ResolvedItem {
                    integrity_check_result_id: icr_id,
                    reference_path: rf.path,
                    scanned_file_id: w.id,
                    scan_run_id: w.scan_run_id,
                    removable_media_id: w.removable_media_id,
                });
            }
            None => {
                repo::insert_integrity_check_result(
                    &tx,
                    check_run_id,
                    Some(rf.id),
                    None,
                    ResultStatus::Missing,
                    None,
                )?;
                missing.push(rf);
            }
        }
    }
    tx.commit()?;
    repo::finish_check_run(conn, check_run_id, RunStatus::Completed, started_at)?;

    Ok(Plan {
        check_run_id,
        resolved,
        missing,
    })
}

/// Computes SHA-256 (in parallel, §4) for every candidate that doesn't already have
/// one, persisting each result back onto its `scanned_file` row so later plans (or a
/// duplicate/integrity check over the same scan) never redo the work. A candidate
/// whose hash fails to compute (e.g. a folder file removed/corrupted since the scan)
/// just stays un-matchable — that's reflected in its own reference file ending up
/// `missing`, not a fatal error for the whole plan (§10.11's skip-and-continue spirit).
fn hash_missing_candidates(
    conn: &mut Connection,
    candidates: &mut [repo::ReconstructionScannedFile],
    policy: &PasswordPolicy,
) -> Result<()> {
    let by_id: HashMap<i64, repo::ReconstructionScannedFile> =
        candidates.iter().cloned().map(|c| (c.id, c)).collect();
    let need_hash: Vec<i64> = candidates
        .iter()
        .filter(|c| c.sha256.is_none())
        .map(|c| c.id)
        .collect();
    let hashed: Vec<(i64, std::io::Result<[u8; 32]>)> = need_hash
        .into_par_iter()
        .map(|id| {
            let (root, hops) = archive::resolve_hops(id, &by_id);
            let result = archive::hash_entry(&root, &hops, &[HashAlgorithm::Sha256], policy)
                .map(|v| v.sha256.expect("sha256 was requested"));
            (id, result)
        })
        .collect();

    for (id, result) in hashed {
        if let Ok(sha256) = result {
            repo::update_scanned_file_sha256(conn, id, &sha256)?;
            if let Some(c) = candidates.iter_mut().find(|c| c.id == id) {
                c.sha256 = Some(sha256.to_vec());
            }
        }
    }
    Ok(())
}

/// The §10.20 priority rule: rank 0 (destination) beats rank 1 (any other
/// non-removable/folder scan) beats rank 2 (removable media); within rank 2, the most
/// recently completed scan wins.
fn pick_winner<'a>(
    candidates: &[&'a repo::ReconstructionScannedFile],
    destination_scan_run_id: i64,
) -> Option<&'a repo::ReconstructionScannedFile> {
    candidates.iter().copied().min_by_key(|c| {
        let rank = if c.scan_run_id == destination_scan_run_id {
            0
        } else if c.removable_media_id.is_none() {
            1
        } else {
            2
        };
        (rank, std::cmp::Reverse(c.scan_completed_at.unwrap_or(0)))
    })
}

/// Creates a `reconstruction_run` for `plan`'s resolved entries (§10.20) — one
/// `reconstruction_item` per resolved reference file, `missing` entries excluded
/// entirely so they never block the rest.
pub fn create_run(
    conn: &mut Connection,
    check_run_id: i64,
    destination_folder_path: &str,
    resolved: &[ResolvedItem],
    started_at: i64,
) -> Result<i64> {
    let run_id =
        repo::insert_reconstruction_run(conn, check_run_id, destination_folder_path, started_at)?;
    let tx = conn.transaction()?;
    for item in resolved {
        repo::insert_reconstruction_item(&tx, run_id, item.integrity_check_result_id)?;
    }
    tx.commit()?;
    Ok(run_id)
}

pub struct PassSummary {
    pub written_count: usize,
    pub error_count: usize,
    /// Removable media (`removable_media.id`) at least one still-outstanding item
    /// needs — none of these were connected (per `connected_media`) during this pass.
    pub still_needed_removable_media: Vec<i64>,
}

enum Availability {
    Available,
    NotConnected(i64),
}

fn check_availability(
    by_id: &HashMap<i64, repo::ReconstructionScannedFile>,
    scanned_file_id: i64,
) -> Availability {
    let row = &by_id[&scanned_file_id];
    match row.removable_media_id {
        Some(media_id) if row.folder_path.is_none() => Availability::NotConnected(media_id),
        _ => Availability::Available,
    }
}

/// Splits a reference/scan path like `"data.zip/a.txt"` into its immediate archive
/// container (`"data.zip"`) and the entry name within it (`"a.txt"`), if the path is
/// archive-nested at all — `None` for a plain, non-nested path.
///
/// Only single-level nesting is reconstructed (documented v1 limitation): an entry
/// inside a *nested* archive (`"outer.zip/inner.zip/leaf.txt"`) groups under
/// `"outer.zip"` with entry name `"inner.zip/leaf.txt"` rather than genuinely
/// rebuilding `"inner.zip"` as its own archive first and nesting that — reconstructing
/// doubly-nested archives isn't supported yet.
fn split_container(path: &str) -> Option<(&str, &str)> {
    let mut consumed = 0usize;
    for segment in path.split('/') {
        let end = consumed + segment.len();
        if ArchiveFormat::detect(Path::new(segment)).is_some() {
            return if end == path.len() {
                None // the path itself is the archive, not something nested within one
            } else {
                Some((&path[..end], &path[end + 1..]))
            };
        }
        consumed = end + 1;
    }
    None
}

/// Runs one pass over everything still outstanding (`pending`, or a prior `error` —
/// both are retried every pass, giving §10.20's "そのメディアで必要な全ファイルの試行が
/// 終わった時点で失敗分のみ再試行" for free as long as the caller invokes this again for
/// the same connected medium) for `reconstruction_run_id`. `connected_media` maps
/// `removable_media_id` to its current mount path for every removable medium the
/// caller has confirmed is connected right now; anything sourced from a medium not in
/// this map is left outstanding and reported in `still_needed_removable_media`
/// instead of being attempted.
pub fn run_pass(
    conn: &mut Connection,
    reconstruction_run_id: i64,
    destination_folder_path: &Path,
    policy: &PasswordPolicy,
    connected_media: &HashMap<i64, PathBuf>,
    now: i64,
) -> Result<PassSummary> {
    let run = repo::get_reconstruction_run(conn, reconstruction_run_id)?
        .expect("caller passes a reconstruction_run_id it just looked up or created");
    let outstanding: Vec<repo::ReconstructionItemRow> =
        repo::list_reconstruction_items(conn, reconstruction_run_id, None)?
            .into_iter()
            .filter(|i| i.status != ReconstructionItemStatus::Written)
            .collect();

    let scan_run_ids = repo::list_check_run_source_scan_run_ids(conn, run.check_run_id)?;
    let mut scanned = repo::list_scanned_files_for_reconstruction(conn, &scan_run_ids)?;
    for row in &mut scanned {
        if let Some(media_id) = row.removable_media_id {
            row.folder_path = connected_media
                .get(&media_id)
                .map(|p| p.to_string_lossy().into_owned());
        }
    }
    let by_id: HashMap<i64, repo::ReconstructionScannedFile> =
        scanned.into_iter().map(|s| (s.id, s)).collect();

    let mut loose = Vec::new();
    let mut containers: HashMap<&str, Vec<&repo::ReconstructionItemRow>> = HashMap::new();
    for item in &outstanding {
        match split_container(&item.reference_path) {
            Some((container, _)) => containers.entry(container).or_default().push(item),
            None => loose.push(item),
        }
    }

    let mut written_count = 0usize;
    let mut error_count = 0usize;
    let mut still_needed = BTreeSet::new();

    for item in loose {
        match check_availability(&by_id, item.scanned_file_id) {
            Availability::NotConnected(media_id) => {
                still_needed.insert(media_id);
            }
            Availability::Available => {
                match write_loose(&by_id, item, destination_folder_path, policy) {
                    Ok(()) => {
                        repo::mark_reconstruction_item_written(conn, item.id, now)?;
                        written_count += 1;
                    }
                    Err(e) => {
                        repo::mark_reconstruction_item_error(conn, item.id, &e.to_string())?;
                        error_count += 1;
                    }
                }
            }
        }
    }

    for (container_path, items) in containers {
        let mut blocked_on = Vec::new();
        for item in &items {
            if let Availability::NotConnected(media_id) =
                check_availability(&by_id, item.scanned_file_id)
            {
                blocked_on.push(media_id);
            }
        }
        if !blocked_on.is_empty() {
            still_needed.extend(blocked_on);
            continue;
        }
        match write_container(
            &by_id,
            &items,
            destination_folder_path,
            container_path,
            policy,
        ) {
            Ok(()) => {
                for item in items {
                    repo::mark_reconstruction_item_written(conn, item.id, now)?;
                    written_count += 1;
                }
            }
            Err(e) => {
                let message = e.to_string();
                for item in items {
                    repo::mark_reconstruction_item_error(conn, item.id, &message)?;
                    error_count += 1;
                }
            }
        }
    }

    let counts = repo::count_reconstruction_items(conn, reconstruction_run_id)?;
    if counts.pending == 0 && counts.error == 0 {
        repo::finish_reconstruction_run(conn, reconstruction_run_id, RunStatus::Completed, now)?;
    }

    Ok(PassSummary {
        written_count,
        error_count,
        still_needed_removable_media: still_needed.into_iter().collect(),
    })
}

fn write_loose(
    by_id: &HashMap<i64, repo::ReconstructionScannedFile>,
    item: &repo::ReconstructionItemRow,
    destination_folder_path: &Path,
    policy: &PasswordPolicy,
) -> std::io::Result<()> {
    let (root, hops) = archive::resolve_hops(item.scanned_file_id, by_id);
    let bytes = archive::read_entry_content(&root, &hops, policy)?;
    let dest_path = destination_folder_path.join(&item.reference_path);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // §10.24/7.4: unconditional overwrite, no backup/quarantine of whatever was there.
    std::fs::write(&dest_path, &bytes)
}

fn write_container(
    by_id: &HashMap<i64, repo::ReconstructionScannedFile>,
    items: &[&repo::ReconstructionItemRow],
    destination_folder_path: &Path,
    container_path: &str,
    policy: &PasswordPolicy,
) -> std::io::Result<()> {
    let mut buffered: Vec<(String, Vec<u8>)> = Vec::with_capacity(items.len());
    for item in items {
        let (_, entry_name) =
            split_container(&item.reference_path).expect("grouped under this container above");
        let (root, hops) = archive::resolve_hops(item.scanned_file_id, by_id);
        let bytes = archive::read_entry_content(&root, &hops, policy)?;
        buffered.push((entry_name.to_string(), bytes));
    }
    let entries: Vec<archive::deterministic::Entry> = buffered
        .iter()
        .map(|(name, data)| archive::deterministic::Entry { name, data })
        .collect();

    let bytes = match ArchiveFormat::detect(Path::new(container_path)) {
        Some(ArchiveFormat::Zip) => archive::deterministic::write_torrentzip(&entries),
        Some(ArchiveFormat::SevenZ) => archive::deterministic::write_rv7z(&entries),
        None => {
            return Err(std::io::Error::other(format!(
                "認識できないコンテナ形式です: {container_path}"
            )))
        }
    };

    let dest_path = destination_folder_path.join(container_path);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest_path, &bytes)
}
