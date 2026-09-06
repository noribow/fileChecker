//! Result rendering for `check integrity`/`check duplicate`/`check show` (§10.16):
//! `text` (human-readable summary + detail), `json`/`csv` (machine-readable, always
//! full detail — no archive-error aggregation exists yet since archives are P7).

use filechecker_core::db::repo;
use filechecker_core::db::ResultStatus;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Text,
    Json,
    Csv,
    /// Only ever valid for `report export` (§10.16: "HTMLは大きくなるため`report
    /// export`側でのみ提供する") — `check integrity`/`check duplicate`/`check show`
    /// reject it explicitly before rendering.
    Html,
}

impl std::str::FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(Format::Text),
            "json" => Ok(Format::Json),
            "csv" => Ok(Format::Csv),
            "html" => Ok(Format::Html),
            other => Err(format!(
                "不明な--format: {other} (text|json|csv|htmlのいずれか。htmlはreport exportのみ)"
            )),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const HTML_STYLE: &str = "body{font-family:sans-serif;margin:2em}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left}th{background:#eee}";

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Every status except `ok` (§10.16: `ok` rows show as a count only unless the caller
/// explicitly asks for them via `--status ok`).
pub fn default_status_filter() -> Vec<String> {
    vec![
        "corrupted".into(),
        "missing".into(),
        "extra".into(),
        "error".into(),
    ]
}

// ---- integrity ----------------------------------------------------------------------

#[derive(Default)]
pub struct IntegrityCounts {
    pub ok: usize,
    pub corrupted: usize,
    pub missing: usize,
    pub extra: usize,
    pub error: usize,
}

impl IntegrityCounts {
    pub fn from_rows(rows: &[repo::IntegrityResultRow]) -> Self {
        let mut c = IntegrityCounts::default();
        for r in rows {
            match r.result_status {
                ResultStatus::Ok => c.ok += 1,
                ResultStatus::Corrupted => c.corrupted += 1,
                ResultStatus::Missing => c.missing += 1,
                ResultStatus::Extra => c.extra += 1,
                ResultStatus::Error => c.error += 1,
            }
        }
        c
    }
}

pub fn render_integrity(
    format: Format,
    check_run_id: i64,
    reference_set_name: &str,
    reference_set_version: u32,
    counts: &IntegrityCounts,
    rows: &[repo::IntegrityResultRow],
) -> String {
    match format {
        Format::Text => render_integrity_text(
            check_run_id,
            reference_set_name,
            reference_set_version,
            counts,
            rows,
        ),
        Format::Json => render_integrity_json(
            check_run_id,
            reference_set_name,
            reference_set_version,
            counts,
            rows,
        ),
        Format::Csv => render_integrity_csv(rows),
        Format::Html => render_integrity_html(
            check_run_id,
            reference_set_name,
            reference_set_version,
            counts,
            rows,
        ),
    }
}

fn render_integrity_text(
    check_run_id: i64,
    reference_set_name: &str,
    reference_set_version: u32,
    counts: &IntegrityCounts,
    rows: &[repo::IntegrityResultRow],
) -> String {
    let mut out = format!(
        "整合性チェック結果 (check_run: {check_run_id}, reference_set: \"{reference_set_name}\" v{reference_set_version})\n"
    );
    out.push_str(&format!("  ok        {}\n", counts.ok));
    out.push_str(&format!("  corrupted {}\n", counts.corrupted));
    out.push_str(&format!("  missing   {}\n", counts.missing));
    out.push_str(&format!("  extra     {}\n", counts.extra));
    out.push_str(&format!("  error     {}\n", counts.error));

    for status in [
        ResultStatus::Ok,
        ResultStatus::Corrupted,
        ResultStatus::Missing,
        ResultStatus::Extra,
        ResultStatus::Error,
    ] {
        let matching: Vec<&repo::IntegrityResultRow> =
            rows.iter().filter(|r| r.result_status == status).collect();
        if matching.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{}:\n", status.as_str()));
        for r in matching {
            let size = r
                .size
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".to_string());
            let detail = r.detail.as_deref().unwrap_or("");
            out.push_str(&format!("  {:<30} {:>12} {}\n", r.path, size, detail));
        }
    }
    out
}

fn render_integrity_json(
    check_run_id: i64,
    reference_set_name: &str,
    reference_set_version: u32,
    counts: &IntegrityCounts,
    rows: &[repo::IntegrityResultRow],
) -> String {
    let results: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "status": r.result_status.as_str(),
                "path": r.path,
                "size": r.size,
                "detail": r.detail,
            })
        })
        .collect();
    let value = serde_json::json!({
        "check_run_id": check_run_id,
        "reference_set": {"name": reference_set_name, "version": reference_set_version},
        "summary": {
            "ok": counts.ok,
            "corrupted": counts.corrupted,
            "missing": counts.missing,
            "extra": counts.extra,
            "error": counts.error,
        },
        "results": results,
    });
    serde_json::to_string_pretty(&value).expect("json serialization never fails here")
}

fn render_integrity_csv(rows: &[repo::IntegrityResultRow]) -> String {
    let mut out = String::from("status,path,size,detail\n");
    for r in rows {
        let size = r.size.map(|s| s.to_string()).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{}\n",
            csv_field(r.result_status.as_str()),
            csv_field(&r.path),
            size,
            csv_field(r.detail.as_deref().unwrap_or(""))
        ));
    }
    out
}

fn render_integrity_html(
    check_run_id: i64,
    reference_set_name: &str,
    reference_set_version: u32,
    counts: &IntegrityCounts,
    rows: &[repo::IntegrityResultRow],
) -> String {
    let mut out = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>整合性チェック結果 (check_run {check_run_id})</title><style>{HTML_STYLE}</style></head><body>\n"
    );
    out.push_str(&format!(
        "<h1>整合性チェック結果 (check_run: {check_run_id}, reference_set: \"{}\" v{reference_set_version})</h1>\n",
        html_escape(reference_set_name)
    ));
    out.push_str("<ul>\n");
    out.push_str(&format!("<li>ok: {}</li>\n", counts.ok));
    out.push_str(&format!("<li>corrupted: {}</li>\n", counts.corrupted));
    out.push_str(&format!("<li>missing: {}</li>\n", counts.missing));
    out.push_str(&format!("<li>extra: {}</li>\n", counts.extra));
    out.push_str(&format!("<li>error: {}</li>\n", counts.error));
    out.push_str("</ul>\n");
    out.push_str("<table><thead><tr><th>status</th><th>path</th><th>size</th><th>detail</th></tr></thead><tbody>\n");
    for r in rows {
        let size = r
            .size
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            html_escape(r.result_status.as_str()),
            html_escape(&r.path),
            html_escape(&size),
            html_escape(r.detail.as_deref().unwrap_or(""))
        ));
    }
    out.push_str("</tbody></table>\n</body></html>\n");
    out
}

// ---- duplicate ------------------------------------------------------------------------

pub struct DuplicateCounts {
    pub group_count: usize,
    pub duplicate_file_count: usize,
    pub reclaimable_bytes: i64,
    /// Files excluded from grouping because hashing failed (§10.11). `None` when
    /// redisplaying a past `check_run` (`check show`/`report export`): that count isn't
    /// persisted anywhere queryable per check_run, only reported live by
    /// `run_duplicate_check`'s own return value.
    pub error_count: Option<usize>,
}

pub fn render_duplicate(
    format: Format,
    check_run_id: i64,
    counts: &DuplicateCounts,
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    match format {
        Format::Text => render_duplicate_text(check_run_id, counts, groups),
        Format::Json => render_duplicate_json(check_run_id, counts, groups),
        Format::Csv => render_duplicate_csv(groups),
        Format::Html => render_duplicate_html(check_run_id, counts, groups),
    }
}

fn render_duplicate_text(
    check_run_id: i64,
    counts: &DuplicateCounts,
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    let mut out = format!("重複チェック結果 (check_run: {check_run_id})\n");
    out.push_str(&format!("  グループ数           {}\n", counts.group_count));
    out.push_str(&format!(
        "  重複ファイル数        {}\n",
        counts.duplicate_file_count
    ));
    out.push_str(&format!(
        "  削減可能サイズ見込み  {} bytes\n",
        counts.reclaimable_bytes
    ));
    match counts.error_count {
        Some(n) => out.push_str(&format!("  ハッシュエラー        {n}\n")),
        None => {
            out.push_str("  ハッシュエラー        —（過去のcheck_runは件数を保持していません）\n")
        }
    }

    for (group, members) in groups {
        out.push_str(&format!(
            "\nグループ (size={}, member={}, sha256={}):\n",
            group.size,
            group.member_count,
            hex_encode(&group.sha256)
        ));
        for m in members {
            out.push_str(&format!("  {} (scan_run {})\n", m.path, m.scan_run_id));
        }
    }
    out
}

fn render_duplicate_json(
    check_run_id: i64,
    counts: &DuplicateCounts,
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    let groups_json: Vec<serde_json::Value> = groups
        .iter()
        .map(|(g, members)| {
            let members_json: Vec<serde_json::Value> = members
                .iter()
                .map(|m| serde_json::json!({"path": m.path, "scan_run_id": m.scan_run_id}))
                .collect();
            serde_json::json!({
                "id": g.id,
                "sha256": hex_encode(&g.sha256),
                "size": g.size,
                "member_count": g.member_count,
                "members": members_json,
            })
        })
        .collect();
    let value = serde_json::json!({
        "check_run_id": check_run_id,
        "summary": {
            "group_count": counts.group_count,
            "duplicate_file_count": counts.duplicate_file_count,
            "reclaimable_bytes": counts.reclaimable_bytes,
            "error_count": counts.error_count,
        },
        "groups": groups_json,
    });
    serde_json::to_string_pretty(&value).expect("json serialization never fails here")
}

fn render_duplicate_csv(
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    let mut out = String::from("group_id,sha256,size,member_count,path,scan_run_id\n");
    for (g, members) in groups {
        let sha256 = hex_encode(&g.sha256);
        for m in members {
            out.push_str(&format!(
                "{},{},{},{},{},{}\n",
                g.id,
                sha256,
                g.size,
                g.member_count,
                csv_field(&m.path),
                m.scan_run_id
            ));
        }
    }
    out
}

fn render_duplicate_html(
    check_run_id: i64,
    counts: &DuplicateCounts,
    groups: &[(repo::DuplicateGroupRow, Vec<repo::DuplicateGroupMemberRow>)],
) -> String {
    let mut out = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>重複チェック結果 (check_run {check_run_id})</title><style>{HTML_STYLE}</style></head><body>\n"
    );
    out.push_str(&format!(
        "<h1>重複チェック結果 (check_run: {check_run_id})</h1>\n"
    ));
    out.push_str("<ul>\n");
    out.push_str(&format!("<li>グループ数: {}</li>\n", counts.group_count));
    out.push_str(&format!(
        "<li>重複ファイル数: {}</li>\n",
        counts.duplicate_file_count
    ));
    out.push_str(&format!(
        "<li>削減可能サイズ見込み: {} bytes</li>\n",
        counts.reclaimable_bytes
    ));
    match counts.error_count {
        Some(n) => out.push_str(&format!("<li>ハッシュエラー: {n}</li>\n")),
        None => {
            out.push_str("<li>ハッシュエラー: —（過去のcheck_runは件数を保持していません）</li>\n")
        }
    }
    out.push_str("</ul>\n");
    out.push_str("<table><thead><tr><th>group</th><th>sha256</th><th>size</th><th>member_count</th><th>path</th><th>scan_run_id</th></tr></thead><tbody>\n");
    for (group, members) in groups {
        let sha256 = hex_encode(&group.sha256);
        for m in members {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                group.id,
                html_escape(&sha256),
                group.size,
                group.member_count,
                html_escape(&m.path),
                m.scan_run_id
            ));
        }
    }
    out.push_str("</tbody></table>\n</body></html>\n");
    out
}

/// Keeps only rows whose status name is in `selected` (§10.16's `--status` filter).
pub fn filter_by_status(
    rows: &[repo::IntegrityResultRow],
    selected: &[String],
) -> Vec<repo::IntegrityResultRow> {
    rows.iter()
        .filter(|r| selected.iter().any(|s| s == r.result_status.as_str()))
        .cloned()
        .collect()
}
