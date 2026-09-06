use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::Sha256;

use super::HashAlgorithm;

/// Results of hashing a byte stream with a subset of the four supported algorithms.
/// Fields are `None` for algorithms that weren't requested.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HashValues {
    pub crc32: Option<u32>,
    pub md5: Option<[u8; 16]>,
    pub sha1: Option<[u8; 20]>,
    pub sha256: Option<[u8; 32]>,
}

/// Feeds each chunk of a byte stream into every requested hasher in parallel, so a file
/// is read once regardless of how many algorithms are needed from it (`docs/requirements.md`
/// §10.8: avoid re-reading the same file once per algorithm).
pub struct MultiHasher {
    crc32: Option<crc32fast::Hasher>,
    md5: Option<Md5>,
    sha1: Option<Sha1>,
    sha256: Option<Sha256>,
}

impl MultiHasher {
    pub fn new(algorithms: &[HashAlgorithm]) -> Self {
        let mut hasher = MultiHasher {
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
        };
        for algorithm in algorithms {
            match algorithm {
                HashAlgorithm::Crc32 => hasher.crc32 = Some(crc32fast::Hasher::new()),
                HashAlgorithm::Md5 => hasher.md5 = Some(Md5::new()),
                HashAlgorithm::Sha1 => hasher.sha1 = Some(Sha1::new()),
                HashAlgorithm::Sha256 => hasher.sha256 = Some(Sha256::new()),
            }
        }
        hasher
    }

    pub fn update(&mut self, chunk: &[u8]) {
        if let Some(h) = self.crc32.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = self.md5.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = self.sha1.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = self.sha256.as_mut() {
            h.update(chunk);
        }
    }

    pub fn finalize(self) -> HashValues {
        HashValues {
            crc32: self.crc32.map(|h| h.finalize()),
            md5: self.md5.map(|h| h.finalize().into()),
            sha1: self.sha1.map(|h| h.finalize().into()),
            sha256: self.sha256.map(|h| h.finalize().into()),
        }
    }
}
