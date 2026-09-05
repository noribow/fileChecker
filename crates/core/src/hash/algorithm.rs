/// One of the four hash algorithms File Checker implements (`docs/requirements.md` §10.1).
///
/// CRC32/MD5/SHA-1/SHA-256 are all implemented because external reference-set files
/// (CSV/XML from other tools) may use any of them and File Checker cannot choose the
/// algorithm on their behalf. SHA-256 is additionally the standard for the native
/// JSON reference-set format and the final duplicate-check confirmation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgorithm {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgorithm {
    /// All algorithms File Checker supports, in a stable order.
    pub const ALL: [HashAlgorithm; 4] = [
        HashAlgorithm::Crc32,
        HashAlgorithm::Md5,
        HashAlgorithm::Sha1,
        HashAlgorithm::Sha256,
    ];
}
