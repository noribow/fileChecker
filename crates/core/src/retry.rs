//! Retry policy for file-level I/O (`docs/requirements.md` §10.17).
//!
//! Transient I/O failures (read failure, device busy) get retried a fixed 3 times
//! with exponential backoff before being recorded as an error. Permission errors are
//! not retried — the same-run permission state is very unlikely to change, so retrying
//! only wastes time. This module only implements the generic retry loop; callers
//! (scanning, hashing) decide what counts as a permission error for their operation.

use std::io;
use std::time::Duration;

/// Delays between retry attempts, in order. Three retries after the initial attempt
/// (four attempts total), per §10.17's "3回まで、指数バックオフ（200ms→400ms→800ms）".
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// Runs `op`, retrying up to 3 additional times with exponential backoff if it fails
/// with an error `is_retryable` accepts. A non-retryable error (e.g. permission
/// denied) is returned immediately on the first failure.
pub fn retry_io<T>(
    mut op: impl FnMut() -> io::Result<T>,
    is_retryable: impl Fn(&io::Error) -> bool,
) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) if attempt < RETRY_DELAYS.len() && is_retryable(&err) => {
                std::thread::sleep(RETRY_DELAYS[attempt]);
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Default retryability classification for filesystem metadata/read operations:
/// permission errors are never retried, everything else is treated as potentially
/// transient (§10.17 groups "read failure, device busy" as retryable; distinguishing
/// finer-grained causes is left to future refinement if it proves necessary).
pub fn is_retryable_fs_error(err: &io::Error) -> bool {
    err.kind() != io::ErrorKind::PermissionDenied
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn permission_denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    fn other_error() -> io::Error {
        io::Error::other("device busy")
    }

    #[test]
    fn succeeds_immediately_without_retrying() {
        let calls = Cell::new(0);
        let result = retry_io(
            || {
                calls.set(calls.get() + 1);
                Ok::<_, io::Error>(42)
            },
            is_retryable_fs_error,
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn retries_transient_errors_then_succeeds() {
        let calls = Cell::new(0);
        let result = retry_io(
            || {
                let n = calls.get() + 1;
                calls.set(n);
                if n < 3 {
                    Err(other_error())
                } else {
                    Ok(99)
                }
            },
            is_retryable_fs_error,
        );
        assert_eq!(result.unwrap(), 99);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn gives_up_after_three_retries() {
        let calls = Cell::new(0);
        let result = retry_io(
            || {
                calls.set(calls.get() + 1);
                Err::<i32, _>(other_error())
            },
            is_retryable_fs_error,
        );
        assert!(result.is_err());
        // 1 initial attempt + 3 retries = 4 total.
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn permission_errors_are_not_retried() {
        let calls = Cell::new(0);
        let result = retry_io(
            || {
                calls.set(calls.get() + 1);
                Err::<i32, _>(permission_denied())
            },
            is_retryable_fs_error,
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }
}
