//! Exit code table (`docs/requirements.md` §10.16). "When several apply, the higher
//! number wins" (エラーが差分より優先される) is encoded directly into the two
//! `*_exit_code` functions below rather than left to call sites to get right.

pub const SUCCESS: i32 = 0;
pub const DIFF: i32 = 1;
pub const UNVERIFIABLE: i32 = 2;
pub const FAILURE: i32 = 3;
pub const INTERACTIVE_REQUIRED: i32 = 4;
pub const USAGE_ERROR: i32 = 64;

/// §10.16's integrity-check exit code: `error` rows (couldn't verify) always win over a
/// plain diff, and `--exit-zero-on-diff` only ever downgrades a pure-diff code 1 to 0.
pub fn integrity_exit_code(
    corrupted: usize,
    missing: usize,
    extra: usize,
    error: usize,
    exit_zero_on_diff: bool,
) -> i32 {
    if error > 0 {
        return UNVERIFIABLE;
    }
    if corrupted + missing + extra > 0 {
        return if exit_zero_on_diff { SUCCESS } else { DIFF };
    }
    SUCCESS
}

/// §10.16's duplicate-check exit code: any duplicate group found is a "diff" (code 1),
/// any hash-computation error is "unverifiable" (code 2, wins over a diff).
pub fn duplicate_exit_code(group_count: usize, error_count: usize, exit_zero_on_diff: bool) -> i32 {
    if error_count > 0 {
        return UNVERIFIABLE;
    }
    if group_count > 0 {
        return if exit_zero_on_diff { SUCCESS } else { DIFF };
    }
    SUCCESS
}
