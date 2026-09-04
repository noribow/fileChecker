//! Enum representations of the DB `CHECK`-constrained TEXT columns (§10.12).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetType {
    Folder,
    RemovableMedia,
}

impl TargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetType::Folder => "folder",
            TargetType::RemovableMedia => "removable_media",
        }
    }
}

/// `scan_run.hash_mode` (§10.3/§10.8): `lazy` defers SHA-256 to the comparison phase
/// for regular folders; `eager` computes every needed hash in the single connected
/// pass for removable media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashMode {
    Lazy,
    Eager,
}

impl HashMode {
    pub fn as_str(self) -> &'static str {
        match self {
            HashMode::Lazy => "lazy",
            HashMode::Eager => "eager",
        }
    }
}

/// Shared by `scan_run.status` and `check_run.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

/// `scanned_file.status` (§10.11: distinct from `integrity_check_result.result_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Ok,
    Error,
    Skipped,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FileStatus::Ok => "ok",
            FileStatus::Error => "error",
            FileStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckType {
    Integrity,
    Duplicate,
}

impl CheckType {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckType::Integrity => "integrity",
            CheckType::Duplicate => "duplicate",
        }
    }
}

/// `integrity_check_result.result_status`, the 5-way distinction from §10.11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultStatus {
    Ok,
    Corrupted,
    Missing,
    Extra,
    Error,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultStatus::Ok => "ok",
            ResultStatus::Corrupted => "corrupted",
            ResultStatus::Missing => "missing",
            ResultStatus::Extra => "extra",
            ResultStatus::Error => "error",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(ResultStatus::Ok),
            "corrupted" => Some(ResultStatus::Corrupted),
            "missing" => Some(ResultStatus::Missing),
            "extra" => Some(ResultStatus::Extra),
            "error" => Some(ResultStatus::Error),
            _ => None,
        }
    }
}
