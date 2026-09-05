//! Registered archive passwords + master password protection (`docs/requirements.md`
//! §10.9/§10.10, P10).
//!
//! On disk this is one JSON file: a plaintext envelope (KDF salt/params, a verifier
//! hash, an AES-256-GCM nonce) around a ciphertext blob holding the actual registered
//! passwords. The encryption key is derived from a user-remembered master password via
//! Argon2id (§10.10) and exists only in memory, in an `UnlockedStore`, for as long as
//! that value lives — never written back to disk, never cached across process runs.
//!
//! **Two independent salts, on purpose**: the verifier (a self-contained Argon2id PHC
//! string used only to check whether an entered master password is correct) and the
//! actual encryption key are derived with separate random salts. Reusing one salt for
//! both would make the verifier — stored in the clear right next to the ciphertext it's
//! meant to gate — encode the same raw bytes as the encryption key itself, since
//! Argon2id(password, salt, params) is deterministic; anyone who could read the file
//! (the whole point of this feature is that they can, per §10.10's "ソースコードを公表する
//! 前提") would then be able to recover the key without ever needing the master
//! password. Two salts means the verifier reveals nothing about the key.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng as AeadOsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::password_hash::rand_core::{OsRng as RandOsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::archive::{ArchiveFormat, PasswordCandidates};

#[derive(Debug)]
pub enum SecretsError {
    Io(io::Error),
    /// `UnlockedStore::create` was asked to create a store where one already exists
    /// (§10.10's "リセット"操作 is the only sanctioned way to discard one).
    AlreadyExists,
    NotFound,
    WrongMasterPassword,
    Corrupt(String),
}

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretsError::Io(e) => write!(f, "I/Oエラー: {e}"),
            SecretsError::AlreadyExists => write!(f, "登録パスワード設定ファイルは既に存在します"),
            SecretsError::NotFound => write!(f, "登録パスワード設定ファイルが見つかりません"),
            SecretsError::WrongMasterPassword => write!(f, "マスターパスワードが違います"),
            SecretsError::Corrupt(msg) => {
                write!(f, "登録パスワード設定ファイルが壊れています: {msg}")
            }
        }
    }
}

impl std::error::Error for SecretsError {}

impl From<io::Error> for SecretsError {
    fn from(e: io::Error) -> Self {
        SecretsError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, SecretsError>;

/// One registered archive password (§10.9). `format = None` means "applies to every
/// archive format" (§10.9's "複数形式への一括設定"); `Some(fmt)` scopes it to just that
/// format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredPassword {
    pub id: String,
    pub format: Option<ArchiveFormat>,
    pub password: String,
}

const CURRENT_VERSION: u32 = 1;
const KDF_SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    /// Hex-encoded random salt for deriving the encryption key (distinct from
    /// `verifier`'s own embedded salt — see the module doc).
    kdf_salt: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    /// A full Argon2id PHC string (embeds its own salt+params), used only to check
    /// whether an entered master password is correct — never used to derive the key.
    verifier: String,
    /// Hex-encoded AES-256-GCM nonce for `ciphertext`.
    nonce: String,
    /// Hex-encoded AES-256-GCM ciphertext of the JSON-serialized `Vec<RegisteredPassword>`.
    ciphertext: String,
}

/// A password store that's been unlocked with the correct master password: the
/// derived encryption key is held only in memory, for as long as this value lives
/// (§10.10 — never written back to disk, never cached across process runs).
pub struct UnlockedStore {
    path: PathBuf,
    kdf_salt: Vec<u8>,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    verifier: String,
    key: Zeroizing<[u8; KEY_LEN]>,
    passwords: Vec<RegisteredPassword>,
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

fn argon2_with_params(m_cost: u32, t_cost: u32, p_cost: u32) -> Argon2<'static> {
    let params = argon2::Params::new(m_cost, t_cost, p_cost, Some(KEY_LEN))
        .expect("§10.10's KDF params are always a valid combination");
    Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
}

fn derive_key(
    master_password: &str,
    kdf_salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Zeroizing<[u8; KEY_LEN]> {
    let argon2 = argon2_with_params(m_cost, t_cost, p_cost);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(master_password.as_bytes(), kdf_salt, key.as_mut())
        .expect("fixed-size output, valid salt length");
    key
}

fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut AeadOsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("encryption with a freshly generated nonce never fails");
    (nonce.to_vec(), ciphertext)
}

fn decrypt(key: &[u8; KEY_LEN], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| SecretsError::Corrupt("復号に失敗しました（改ざんまたは破損）".to_string()))
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, contents)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

impl UnlockedStore {
    /// Creates a brand-new, empty password store at `path`, protected by
    /// `master_password`. Errors with `AlreadyExists` if a file is already there —
    /// §10.10's "リセット"操作 (`reset_store`) is the only sanctioned way to discard an
    /// existing store, never a silent overwrite.
    pub fn create(path: &Path, master_password: &str) -> Result<Self> {
        if path.exists() {
            return Err(SecretsError::AlreadyExists);
        }
        let store = Self::new_locked_state(path.to_path_buf(), master_password, Vec::new())?;
        store.save()?;
        Ok(store)
    }

    /// Opens the store at `path` and verifies `master_password` against it
    /// (`WrongMasterPassword` if it doesn't match), decrypting the registered
    /// passwords into memory.
    pub fn unlock(path: &Path, master_password: &str) -> Result<Self> {
        let bytes = fs::read(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                SecretsError::NotFound
            } else {
                SecretsError::Io(e)
            }
        })?;
        let file: StoreFile = serde_json::from_slice(&bytes)
            .map_err(|e| SecretsError::Corrupt(format!("JSON解析に失敗: {e}")))?;
        if file.version != CURRENT_VERSION {
            return Err(SecretsError::Corrupt(format!(
                "未対応のバージョンです: {}",
                file.version
            )));
        }

        let hash = PasswordHash::new(&file.verifier)
            .map_err(|e| SecretsError::Corrupt(format!("verifierの解析に失敗: {e}")))?;
        if hash
            .verify_password(&[&Argon2::default()], master_password)
            .is_err()
        {
            return Err(SecretsError::WrongMasterPassword);
        }

        let kdf_salt = hex_decode(&file.kdf_salt)
            .map_err(|_| SecretsError::Corrupt("kdf_saltの16進デコードに失敗".to_string()))?;
        let nonce = hex_decode(&file.nonce)
            .map_err(|_| SecretsError::Corrupt("nonceの16進デコードに失敗".to_string()))?;
        let ciphertext = hex_decode(&file.ciphertext)
            .map_err(|_| SecretsError::Corrupt("ciphertextの16進デコードに失敗".to_string()))?;

        let key = derive_key(
            master_password,
            &kdf_salt,
            file.m_cost,
            file.t_cost,
            file.p_cost,
        );
        let plaintext = decrypt(&key, &nonce, &ciphertext)?;
        let passwords: Vec<RegisteredPassword> = serde_json::from_slice(&plaintext)
            .map_err(|e| SecretsError::Corrupt(format!("復号後のJSON解析に失敗: {e}")))?;

        Ok(Self {
            path: path.to_path_buf(),
            kdf_salt,
            m_cost: file.m_cost,
            t_cost: file.t_cost,
            p_cost: file.p_cost,
            verifier: file.verifier,
            key,
            passwords,
        })
    }

    /// Discards `path` entirely (§10.10's "リセット"操作 — マスターパスワードを忘れた場合の唯一の
    /// 復旧手段). A no-op, not an error, if there was nothing there.
    pub fn reset(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretsError::Io(e)),
        }
    }

    fn new_locked_state(
        path: PathBuf,
        master_password: &str,
        passwords: Vec<RegisteredPassword>,
    ) -> Result<Self> {
        let params = argon2::Params::default();
        let mut kdf_salt = vec![0u8; KDF_SALT_LEN];
        RandOsRng.fill_bytes(&mut kdf_salt);
        let verifier = make_verifier(master_password)?;
        let key = derive_key(
            master_password,
            &kdf_salt,
            params.m_cost(),
            params.t_cost(),
            params.p_cost(),
        );
        Ok(Self {
            path,
            kdf_salt,
            m_cost: params.m_cost(),
            t_cost: params.t_cost(),
            p_cost: params.p_cost(),
            verifier,
            key,
            passwords,
        })
    }

    /// Registers a new password, returning its generated id. Doesn't write to disk by
    /// itself — call `save` (directly, or via `change_master_password` which saves as
    /// part of rotating the key) once you're done making changes.
    pub fn add(&mut self, format: Option<ArchiveFormat>, password: String) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.passwords.push(RegisteredPassword {
            id: id.clone(),
            format,
            password,
        });
        id
    }

    /// Removes the registered password with this id, if any. Returns whether one was
    /// found and removed. Doesn't write to disk by itself — see `add`.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.passwords.len();
        self.passwords.retain(|p| p.id != id);
        self.passwords.len() != before
    }

    pub fn list(&self) -> &[RegisteredPassword] {
        &self.passwords
    }

    /// Re-encrypts (with a fresh nonce) and writes the current in-memory state to
    /// `path`, atomically (write-then-rename, so a crash mid-write can't corrupt the
    /// previous good file).
    pub fn save(&self) -> Result<()> {
        let plaintext =
            serde_json::to_vec(&self.passwords).expect("Vec<RegisteredPassword> always serializes");
        let (nonce, ciphertext) = encrypt(&self.key, &plaintext);
        let file = StoreFile {
            version: CURRENT_VERSION,
            kdf_salt: hex_encode(&self.kdf_salt),
            m_cost: self.m_cost,
            t_cost: self.t_cost,
            p_cost: self.p_cost,
            verifier: self.verifier.clone(),
            nonce: hex_encode(&nonce),
            ciphertext: hex_encode(&ciphertext),
        };
        let bytes = serde_json::to_vec_pretty(&file).expect("StoreFile always serializes");
        write_atomic(&self.path, &bytes)?;
        Ok(())
    }

    /// Rotates the master password (§10.10): re-derives a new salt/verifier/key from
    /// `new_master_password` and immediately re-encrypts and saves under it. The old
    /// master password isn't re-checked here — the caller already had to `unlock` with
    /// it (or `create`) to have an `UnlockedStore` at all.
    pub fn change_master_password(&mut self, new_master_password: &str) -> Result<()> {
        let params = argon2::Params::default();
        let mut kdf_salt = vec![0u8; KDF_SALT_LEN];
        RandOsRng.fill_bytes(&mut kdf_salt);
        let verifier = make_verifier(new_master_password)?;
        let key = derive_key(
            new_master_password,
            &kdf_salt,
            params.m_cost(),
            params.t_cost(),
            params.p_cost(),
        );
        self.kdf_salt = kdf_salt;
        self.m_cost = params.m_cost();
        self.t_cost = params.t_cost();
        self.p_cost = params.p_cost();
        self.verifier = verifier;
        self.key = key;
        self.save()
    }
}

fn make_verifier(master_password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut RandOsRng);
    Argon2::default()
        .hash_password(master_password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| SecretsError::Corrupt(format!("verifier生成に失敗: {e}")))
}

impl PasswordCandidates for UnlockedStore {
    fn candidates(&self, format: ArchiveFormat) -> Vec<String> {
        // Format-specific entries first (more likely to be the intended one), then
        // entries registered for every format (§10.9's "複数形式への一括設定").
        let mut result: Vec<String> = self
            .passwords
            .iter()
            .filter(|p| p.format == Some(format))
            .map(|p| p.password.clone())
            .collect();
        result.extend(
            self.passwords
                .iter()
                .filter(|p| p.format.is_none())
                .map(|p| p.password.clone()),
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_unlock_round_trips_with_the_correct_master_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");

        let mut store = UnlockedStore::create(&path, "correct horse battery staple").unwrap();
        let id = store.add(Some(ArchiveFormat::Zip), "zip-only-password".to_string());
        store.add(None, "applies-to-everything".to_string());
        store.save().unwrap();
        drop(store);

        let reopened = UnlockedStore::unlock(&path, "correct horse battery staple").unwrap();
        assert_eq!(reopened.list().len(), 2);
        assert!(reopened.list().iter().any(|p| p.id == id));

        let zip_candidates = reopened.candidates(ArchiveFormat::Zip);
        assert_eq!(
            zip_candidates,
            vec!["zip-only-password", "applies-to-everything"]
        );
        let sevenz_candidates = reopened.candidates(ArchiveFormat::SevenZ);
        assert_eq!(sevenz_candidates, vec!["applies-to-everything"]);
    }

    #[test]
    fn unlock_rejects_the_wrong_master_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        UnlockedStore::create(&path, "the real master password").unwrap();

        let result = UnlockedStore::unlock(&path, "a guess");
        assert!(matches!(result, Err(SecretsError::WrongMasterPassword)));
    }

    #[test]
    fn unlock_of_a_missing_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let result = UnlockedStore::unlock(&path, "anything");
        assert!(matches!(result, Err(SecretsError::NotFound)));
    }

    #[test]
    fn create_over_an_existing_store_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        UnlockedStore::create(&path, "first").unwrap();
        let result = UnlockedStore::create(&path, "second");
        assert!(matches!(result, Err(SecretsError::AlreadyExists)));
    }

    #[test]
    fn remove_deletes_a_registered_password_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let mut store = UnlockedStore::create(&path, "master").unwrap();
        let id = store.add(None, "secret".to_string());

        assert!(store.remove(&id));
        assert!(store.list().is_empty());
        assert!(!store.remove(&id));
    }

    #[test]
    fn change_master_password_lets_the_new_one_unlock_and_the_old_one_stops_working() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let mut store = UnlockedStore::create(&path, "old master").unwrap();
        store.add(None, "kept-password".to_string());
        store.save().unwrap();

        store.change_master_password("new master").unwrap();
        drop(store);

        let with_new = UnlockedStore::unlock(&path, "new master").unwrap();
        assert_eq!(with_new.list().len(), 1);
        assert_eq!(with_new.list()[0].password, "kept-password");

        let with_old = UnlockedStore::unlock(&path, "old master");
        assert!(matches!(with_old, Err(SecretsError::WrongMasterPassword)));
    }

    #[test]
    fn reset_discards_the_store_and_is_a_no_op_when_nothing_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        UnlockedStore::create(&path, "master").unwrap();
        assert!(path.exists());

        UnlockedStore::reset(&path).unwrap();
        assert!(!path.exists());

        // Resetting an already-absent store is not an error (§10.10).
        UnlockedStore::reset(&path).unwrap();
    }

    #[test]
    fn a_tampered_ciphertext_is_reported_as_corrupt_not_silently_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let mut store = UnlockedStore::create(&path, "master").unwrap();
        store.add(None, "secret".to_string());
        store.save().unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let mut file: StoreFile = serde_json::from_str(&raw).unwrap();
        // Flip one hex character in the ciphertext — still valid hex, still the right
        // length, but the AES-GCM authentication tag will no longer match.
        let mut bytes = file.ciphertext.into_bytes();
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
        file.ciphertext = String::from_utf8(bytes).unwrap();
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let result = UnlockedStore::unlock(&path, "master");
        assert!(matches!(result, Err(SecretsError::Corrupt(_))));
    }
}
