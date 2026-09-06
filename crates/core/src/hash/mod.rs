//! Hash computation primitives (`docs/requirements.md` §10.1/§10.2/§10.8).
//!
//! This module only computes hashes from a byte stream; it does not know about files,
//! the staged size->CRC32->SHA-256 duplicate-check filter, or the regular-folder vs.
//! removable-media timing difference (§10.3/§10.8) — those are orchestration concerns
//! that belong to the scanning layer built on top of this one.

mod algorithm;
mod multi;

pub use algorithm::HashAlgorithm;
pub use multi::{HashValues, MultiHasher};

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::retry::{is_retryable_fs_error, retry_io};

/// Chunk size used when streaming a reader into one or more hashers.
pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Computes the requested hash algorithms from `reader` in a single pass, feeding each
/// chunk read into every requested hasher simultaneously (§10.8's single-pass multi-hash
/// design). Algorithms not present in `algorithms` are left as `None` in the result.
pub fn hash_reader<R: Read>(mut reader: R, algorithms: &[HashAlgorithm]) -> io::Result<HashValues> {
    let mut hasher = MultiHasher::new(algorithms);
    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Computes only CRC32 from `reader`. Used as the first confirmation step of the
/// regular-folder staged filter (size -> CRC32 -> SHA-256, §10.2).
pub fn compute_crc32<R: Read>(reader: R) -> io::Result<u32> {
    let values = hash_reader(reader, &[HashAlgorithm::Crc32])?;
    Ok(values.crc32.expect("crc32 was requested"))
}

/// Computes only SHA-256 from `reader`. Used as the final confirmation step of the
/// regular-folder staged filter (§10.2) and as the standard algorithm for native JSON
/// reference-set generation (§10.1).
pub fn compute_sha256<R: Read>(reader: R) -> io::Result<[u8; 32]> {
    let values = hash_reader(reader, &[HashAlgorithm::Sha256])?;
    Ok(values.sha256.expect("sha256 was requested"))
}

/// Opens `path` and computes the requested algorithms, retrying transient I/O failures
/// per §10.17 (a failed attempt re-opens the file from the start rather than resuming a
/// partially-hashed stream). Every phase that hashes a file it already identified via a
/// `scanned_file`/`reference_file` row (duplicate check, reference-set generation,
/// integrity check) goes through this instead of repeating the open+retry boilerplate.
pub fn hash_file(path: &Path, algorithms: &[HashAlgorithm]) -> io::Result<HashValues> {
    retry_io(
        || hash_reader(File::open(path)?, algorithms),
        is_retryable_fs_error,
    )
}

/// Convenience wrapper over [`hash_file`] for the duplicate-check CRC32 stage (§10.2).
pub fn hash_file_crc32(path: &Path) -> io::Result<u32> {
    Ok(hash_file(path, &[HashAlgorithm::Crc32])?
        .crc32
        .expect("crc32 was requested"))
}

/// Convenience wrapper over [`hash_file`] for SHA-256, the standard/final-confirmation
/// algorithm (§10.1/§10.2).
pub fn hash_file_sha256(path: &Path) -> io::Result<[u8; 32]> {
    Ok(hash_file(path, &[HashAlgorithm::Sha256])?
        .sha256
        .expect("sha256 was requested"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use digest::Digest;
    use std::io::Cursor;

    // Known test vectors for the empty string and "abc", cross-checked against each
    // algorithm's published reference values.
    const EMPTY_CRC32: u32 = 0x0000_0000;
    const EMPTY_MD5: &str = "d41d8cd98f00b204e9800998ecf8427e";
    const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    const ABC_CRC32: u32 = 0x352441c2;
    const ABC_MD5: &str = "900150983cd24fb0d6963f7d28e17f72";
    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn known_vectors_empty_input() {
        let values = hash_reader(Cursor::new(b""), &HashAlgorithm::ALL).unwrap();
        assert_eq!(values.crc32.unwrap(), EMPTY_CRC32);
        assert_eq!(hex::encode(values.md5.unwrap()), EMPTY_MD5);
        assert_eq!(hex::encode(values.sha1.unwrap()), EMPTY_SHA1);
        assert_eq!(hex::encode(values.sha256.unwrap()), EMPTY_SHA256);
    }

    #[test]
    fn known_vectors_abc() {
        let values = hash_reader(Cursor::new(b"abc"), &HashAlgorithm::ALL).unwrap();
        assert_eq!(values.crc32.unwrap(), ABC_CRC32);
        assert_eq!(hex::encode(values.md5.unwrap()), ABC_MD5);
        assert_eq!(hex::encode(values.sha1.unwrap()), ABC_SHA1);
        assert_eq!(hex::encode(values.sha256.unwrap()), ABC_SHA256);
    }

    #[test]
    fn unrequested_algorithms_are_none() {
        let values = hash_reader(Cursor::new(b"abc"), &[HashAlgorithm::Sha256]).unwrap();
        assert!(values.crc32.is_none());
        assert!(values.md5.is_none());
        assert!(values.sha1.is_none());
        assert!(values.sha256.is_some());
    }

    #[test]
    fn convenience_functions_match_multi_hash() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let multi = hash_reader(Cursor::new(data), &HashAlgorithm::ALL).unwrap();

        assert_eq!(
            compute_crc32(Cursor::new(data)).unwrap(),
            multi.crc32.unwrap()
        );
        assert_eq!(
            compute_sha256(Cursor::new(data)).unwrap(),
            multi.sha256.unwrap()
        );
    }

    /// Exercises chunk-boundary handling: sizes exactly at, one byte below, and one byte
    /// above `DEFAULT_CHUNK_SIZE`, plus a couple of chunks' worth of data. A naive
    /// implementation could drop or duplicate bytes at a boundary.
    #[test]
    fn chunk_boundaries_produce_consistent_hashes() {
        for len in [
            DEFAULT_CHUNK_SIZE - 1,
            DEFAULT_CHUNK_SIZE,
            DEFAULT_CHUNK_SIZE + 1,
            DEFAULT_CHUNK_SIZE * 2 + 12345,
        ] {
            // Deterministic pseudo-random-ish content so we're not just hashing zeros.
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();

            let single_pass = hash_reader(Cursor::new(&data), &HashAlgorithm::ALL).unwrap();

            // Cross-check against directly hashing the whole buffer with each library's
            // own one-shot API, independent of our chunked reader loop.
            let expected_crc32 = crc32fast::hash(&data);
            let expected_sha256: [u8; 32] = sha2::Sha256::digest(&data).into();

            assert_eq!(single_pass.crc32.unwrap(), expected_crc32, "len={len}");
            assert_eq!(single_pass.sha256.unwrap(), expected_sha256, "len={len}");
        }
    }

    #[test]
    fn simultaneous_multi_hash_matches_individual_computation() {
        let data: Vec<u8> = (0..DEFAULT_CHUNK_SIZE * 3 + 777)
            .map(|i| (i % 199) as u8)
            .collect();

        let combined = hash_reader(Cursor::new(&data), &HashAlgorithm::ALL).unwrap();
        let crc32_only = compute_crc32(Cursor::new(&data)).unwrap();
        let sha256_only = compute_sha256(Cursor::new(&data)).unwrap();
        let md5_only = hash_reader(Cursor::new(&data), &[HashAlgorithm::Md5])
            .unwrap()
            .md5
            .unwrap();
        let sha1_only = hash_reader(Cursor::new(&data), &[HashAlgorithm::Sha1])
            .unwrap()
            .sha1
            .unwrap();

        assert_eq!(combined.crc32.unwrap(), crc32_only);
        assert_eq!(combined.sha256.unwrap(), sha256_only);
        assert_eq!(combined.md5.unwrap(), md5_only);
        assert_eq!(combined.sha1.unwrap(), sha1_only);
    }
}
