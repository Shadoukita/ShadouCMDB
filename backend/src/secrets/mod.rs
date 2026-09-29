//! Secrets the server must be able to read back (TOTP seeds, the OIDC client
//! secrets and LDAP bind passwords of identity providers), encrypted at rest
//! under a key kept outside the database (GH#189, design SHAA-484; GH#199,
//! design SHAA-490).
//!
//! The master key is 32 random bytes, base64 in the file named by
//! `ENCRYPTION_KEY_FILE`. It never touches the database or a backup. Each
//! [`Purpose`] gets its own AES-256-GCM subkey (HKDF-SHA256 with the purpose's
//! info string), so a ciphertext of one purpose never opens as another. The
//! key id, also derived with HKDF, is stored next to each ciphertext: a wrong
//! key is detected up front, and a rotation (`ENCRYPTION_KEY_PREVIOUS_FILE`)
//! knows which rows still need re-encrypting. [`sealed`] lists the tables
//! holding such ciphertexts and re-encrypts them at start-up.
//!
//! Stored layout: `nonce(12) || ciphertext || tag(16)`, with a fresh random
//! nonce per encryption.

pub mod cli;
#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod provider_db_tests;
pub mod sealed;

use std::fmt;
use std::path::Path;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf;

use crate::config::EncryptionConfig;

/// Bytes a sealed value adds to its plaintext: the nonce and the GCM tag.
pub const OVERHEAD: usize = NONCE_LEN + 16;

const KEY_LEN: usize = 32;
const KEY_ID_INFO: &[u8] = b"shadoucmdb/key-id/v1";

/// What a subkey encrypts. Each has its own HKDF info string, hence its own key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `user_totp.secret`
    TotpSecret,
    /// `identity_providers.client_secret_enc` and `.bind_password_enc`
    IdentityProviderSecret,
}

impl Purpose {
    const ALL: [Purpose; 2] = [Purpose::TotpSecret, Purpose::IdentityProviderSecret];

    fn info(self) -> &'static [u8] {
        match self {
            Purpose::TotpSecret => b"shadoucmdb/totp-secret/v1",
            Purpose::IdentityProviderSecret => b"shadoucmdb/identity-provider-secret/v1",
        }
    }
}

/// Identifies a master key without revealing it: the first 4 bytes of an HKDF
/// output, stored as `integer` and shown as 8 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(pub i32);

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0 as u32)
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({self})")
    }
}

/// A ciphertext and the key it was made with, ready to store.
pub struct Sealed {
    pub key_id: KeyId,
    pub bytes: Vec<u8>,
}

/// A decrypted secret; wiped from memory when dropped. No `Debug`.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Secret(bytes)
    }
}

impl std::ops::Deref for Secret {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// The 32 bytes of a master key; wiped from memory when dropped. No `Debug`,
/// no `Clone`: it is only borrowed, to derive the subkeys and the key id.
pub struct MasterKey([u8; KEY_LEN]);

impl Drop for MasterKey {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Why a stored value did not decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Encrypted with a key that is not configured.
    UnknownKey(KeyId),
    /// The key is right but authentication failed: the value was altered or
    /// copied from another row.
    Invalid,
}

struct Key {
    id: KeyId,
    subkeys: Vec<(Purpose, LessSafeKey)>,
}

impl Key {
    fn derive(master: &MasterKey) -> Key {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(&master.0);
        let mut id = [0u8; 4];
        prk.expand(&[KEY_ID_INFO], Len(4)).and_then(|okm| okm.fill(&mut id)).expect("HKDF output of 4 bytes");
        let subkeys = Purpose::ALL
            .iter()
            .map(|&p| {
                let info = [p.info()];
                let okm = prk.expand(&info, &AES_256_GCM).expect("HKDF output of one AES-256 key");
                (p, LessSafeKey::new(UnboundKey::from(okm)))
            })
            .collect();
        Key { id: KeyId(i32::from_be_bytes(id)), subkeys }
    }

    fn subkey(&self, purpose: Purpose) -> &LessSafeKey {
        &self.subkeys.iter().find(|(p, _)| *p == purpose).expect("a subkey per purpose").1
    }
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// The active key, and during a rotation the previous one. No `Debug` of keys.
pub struct Keyring {
    active: Key,
    previous: Option<Key>,
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("active", &self.active.id)
            .field("previous", &self.previous.as_ref().map(|k| k.id))
            .finish()
    }
}

impl Keyring {
    /// Reads `ENCRYPTION_KEY_FILE` (required) and `ENCRYPTION_KEY_PREVIOUS_FILE`.
    pub fn load(cfg: &EncryptionConfig) -> anyhow::Result<Keyring> {
        let Some(path) = &cfg.key_file else { bail!("{}", missing_key_message()) };
        let active = Key::derive(&read_key_file("ENCRYPTION_KEY_FILE", path)?);
        let previous = match &cfg.previous_key_file {
            Some(p) => Some(Key::derive(&read_key_file("ENCRYPTION_KEY_PREVIOUS_FILE", p)?)),
            None => None,
        };
        let previous = previous.filter(|p| {
            let same = p.id == active.id;
            if same {
                tracing::warn!(
                    key = %active.id,
                    "ENCRYPTION_KEY_PREVIOUS_FILE holds the same key as ENCRYPTION_KEY_FILE; ignoring it"
                );
            }
            !same
        });
        Ok(Keyring { active, previous })
    }

    #[cfg(test)]
    pub fn from_keys(active: &MasterKey, previous: Option<&MasterKey>) -> Keyring {
        Keyring { active: Key::derive(active), previous: previous.map(Key::derive) }
    }

    /// A random key, for tests.
    #[cfg(test)]
    pub fn random() -> Keyring {
        Keyring::from_keys(&new_key(), None)
    }

    /// One random key shared by the test servers, so a test can read what they stored.
    #[cfg(test)]
    pub fn for_tests() -> std::sync::Arc<Keyring> {
        static RING: std::sync::OnceLock<std::sync::Arc<Keyring>> = std::sync::OnceLock::new();
        RING.get_or_init(|| std::sync::Arc::new(Keyring::random())).clone()
    }

    pub fn active_id(&self) -> KeyId {
        self.active.id
    }

    pub fn previous_id(&self) -> Option<KeyId> {
        self.previous.as_ref().map(|k| k.id)
    }

    /// Encrypts under the active key with a fresh nonce. `ad` binds the value to
    /// where it is stored (table, row, column): elsewhere it does not open.
    pub fn seal(&self, purpose: Purpose, ad: &[u8], plaintext: &[u8]) -> Sealed {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("OS random number generator");
        let mut buf = Vec::with_capacity(OVERHEAD + plaintext.len());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(plaintext);
        let mut body = buf.split_off(NONCE_LEN);
        self.active
            .subkey(purpose)
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(ad), &mut body)
            .expect("AES-GCM sealing a short value");
        buf.extend_from_slice(&body);
        wipe(&mut body);
        Sealed { key_id: self.active.id, bytes: buf }
    }

    /// Decrypts a value sealed under `key_id` (the active or the previous key).
    pub fn open(&self, purpose: Purpose, key_id: KeyId, ad: &[u8], sealed: &[u8]) -> Result<Secret, OpenError> {
        let key = [Some(&self.active), self.previous.as_ref()]
            .into_iter()
            .flatten()
            .find(|k| k.id == key_id)
            .ok_or(OpenError::UnknownKey(key_id))?;
        if sealed.len() < OVERHEAD {
            return Err(OpenError::Invalid);
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| OpenError::Invalid)?;
        let mut body = body.to_vec();
        match key.subkey(purpose).open_in_place(nonce, Aad::from(ad), &mut body) {
            Ok(plain) => {
                let len = plain.len();
                body.truncate(len);
                Ok(Secret(body))
            }
            Err(_) => {
                wipe(&mut body);
                Err(OpenError::Invalid)
            }
        }
    }
}

/// The key ids configured, without keeping the keys: for the commands that
/// only compare ids (`restore`, `mfa reset-undecryptable`). `None`: no
/// `ENCRYPTION_KEY_FILE`.
pub fn configured_key_ids(cfg: &EncryptionConfig) -> anyhow::Result<Option<(KeyId, Option<KeyId>)>> {
    if cfg.key_file.is_none() {
        return Ok(None);
    }
    let ring = Keyring::load(cfg)?;
    Ok(Some((ring.active_id(), ring.previous_id())))
}

/// Why `serve` does not start without a key, and how to make one.
pub fn missing_key_message() -> String {
    format!(
        "ENCRYPTION_KEY_FILE is not set. Since version {}, ShadouCMDB encrypts authenticator secrets and identity \
         provider secrets with a key kept outside the database. Create one with \"shadoucmdb generate-encryption-key --out <path>\", set \
         ENCRYPTION_KEY_FILE in the env file, and back the key up separately from database backups. See \
         docs/security/hardening.md#encryption-key.",
        env!("CARGO_PKG_VERSION")
    )
}

/// 32 bytes from the OS random number generator.
pub fn new_key() -> MasterKey {
    let mut key = MasterKey([0u8; KEY_LEN]);
    getrandom::fill(&mut key.0).expect("OS random number generator");
    key
}

/// The file content for `key`: base64 and a newline.
pub fn encode_key(key: &MasterKey) -> String {
    format!("{}\n", STANDARD.encode(key.0))
}

/// The key id of `key`.
pub fn key_id(key: &MasterKey) -> KeyId {
    Key::derive(key).id
}

fn read_key_file(var: &str, path: &Path) -> anyhow::Result<MasterKey> {
    let meta = std::fs::metadata(path).with_context(|| format!("{var}: cannot read {}", path.display()))?;
    check_permissions(var, path, &meta)?;
    let mut content = std::fs::read(path).with_context(|| format!("{var}: cannot read {}", path.display()))?;
    let key = parse_key(&content).map_err(|e| anyhow::anyhow!("{var}: {e} ({})", path.display()));
    wipe(&mut content);
    key
}

/// The base64 of exactly 32 bytes. Surrounding whitespace, CR/LF and a UTF-8
/// BOM (Notepad adds them) are ignored. The error never repeats the content.
pub fn parse_key(content: &[u8]) -> Result<MasterKey, &'static str> {
    const EXPECTED: &str = "expected the base64 of 32 bytes";
    let content = content.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(content);
    let text = content.trim_ascii();
    let mut decoded = STANDARD.decode(text).map_err(|_| EXPECTED)?;
    // Copied straight into the key, so no loose copy of the bytes is left behind.
    let key = (decoded.len() == KEY_LEN).then(|| {
        let mut key = MasterKey([0u8; KEY_LEN]);
        key.0.copy_from_slice(&decoded);
        key
    });
    wipe(&mut decoded);
    key.ok_or(EXPECTED)
}

/// Unix: the key file must not be readable by everyone, nor writable by group
/// or others (`mode & 0o026`). Group read is fine (`0640 root:shadoucmdb`).
/// Windows has no check: its ACL is set as documented.
#[cfg_attr(not(unix), allow(unused_variables))]
fn check_permissions(var: &str, path: &Path, meta: &std::fs::Metadata) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o026 != 0 {
            bail!(
                "{var}: {} has mode {mode:04o}, so other users can read or change the key; restrict it, e.g. \
                 \"chmod 600 {}\" (or 640 with the service account's group)",
                path.display(),
                path.display()
            );
        }
    }
    Ok(())
}

/// Overwrites key material before it is freed.
fn wipe(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned &mut u8.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    const AD: &[u8] = b"shadoucmdb:test:v1:row";

    #[test]
    fn round_trip_differs_from_the_plaintext() {
        let ring = Keyring::random();
        let seed = [7u8; 20];
        let sealed = ring.seal(Purpose::TotpSecret, AD, &seed);
        assert_eq!(sealed.bytes.len(), 48);
        assert_eq!(sealed.key_id, ring.active_id());
        assert!(!sealed.bytes.windows(seed.len()).any(|w| w == seed), "the seed does not appear in the ciphertext");
        let opened = ring.open(Purpose::TotpSecret, sealed.key_id, AD, &sealed.bytes).unwrap();
        assert_eq!(&*opened, &seed);
        // A fresh nonce every time.
        assert_ne!(ring.seal(Purpose::TotpSecret, AD, &seed).bytes, sealed.bytes);
    }

    #[test]
    fn another_row_a_flipped_byte_or_another_key_does_not_open() {
        let ring = Keyring::random();
        let sealed = ring.seal(Purpose::TotpSecret, AD, &[1u8; 20]);
        let other_row = ring.open(Purpose::TotpSecret, sealed.key_id, b"shadoucmdb:test:v1:other", &sealed.bytes);
        assert_eq!(other_row.err(), Some(OpenError::Invalid));
        for i in [0, 12, 47] {
            let mut tampered = sealed.bytes.clone();
            tampered[i] ^= 1;
            assert_eq!(ring.open(Purpose::TotpSecret, sealed.key_id, AD, &tampered).err(), Some(OpenError::Invalid));
        }
        assert_eq!(
            ring.open(Purpose::TotpSecret, sealed.key_id, AD, &sealed.bytes[..20]).err(),
            Some(OpenError::Invalid)
        );
        let other = Keyring::random();
        assert_eq!(
            other.open(Purpose::TotpSecret, sealed.key_id, AD, &sealed.bytes).err(),
            Some(OpenError::UnknownKey(sealed.key_id))
        );
    }

    #[test]
    fn a_ciphertext_of_one_purpose_does_not_open_as_another() {
        let ring = Keyring::random();
        let totp = ring.seal(Purpose::TotpSecret, AD, b"secret");
        assert_eq!(
            ring.open(Purpose::IdentityProviderSecret, totp.key_id, AD, &totp.bytes).err(),
            Some(OpenError::Invalid)
        );
        let idp = ring.seal(Purpose::IdentityProviderSecret, AD, b"secret");
        assert_eq!(ring.open(Purpose::TotpSecret, idp.key_id, AD, &idp.bytes).err(), Some(OpenError::Invalid));
        assert_eq!(&*ring.open(Purpose::IdentityProviderSecret, idp.key_id, AD, &idp.bytes).unwrap(), b"secret");
    }

    #[test]
    fn the_previous_key_still_opens() {
        let (a, b) = (new_key(), new_key());
        let old = Keyring::from_keys(&a, None);
        let sealed = old.seal(Purpose::TotpSecret, AD, b"seed");
        let rotating = Keyring::from_keys(&b, Some(&a));
        assert_eq!(rotating.previous_id(), Some(old.active_id()));
        assert_eq!(&*rotating.open(Purpose::TotpSecret, sealed.key_id, AD, &sealed.bytes).unwrap(), b"seed");
        assert_eq!(rotating.seal(Purpose::TotpSecret, AD, b"seed").key_id, key_id(&b));
    }

    #[test]
    fn key_id_is_stable_per_key_and_differs_between_keys() {
        let k = MasterKey([42u8; 32]);
        assert_eq!(key_id(&k), key_id(&k));
        assert_ne!(key_id(&k), key_id(&MasterKey([43u8; 32])));
        assert_eq!(format!("{}", KeyId(-1)), "ffffffff");
        assert_eq!(format!("{}", KeyId(0x0102_0304)), "01020304");
        // Pinned (HKDF-SHA256, empty salt, computed independently): the id of a
        // key must not change between releases, or every database looks foreign.
        assert_eq!(key_id(&MasterKey([0u8; 32])).to_string(), "71809ca9");
    }

    #[test]
    fn key_file_parsing() {
        let key = new_key();
        let b64 = STANDARD.encode(key.0);
        for content in [
            b64.clone(),
            format!("{b64}\n"),
            format!("{b64}\r\n"),
            format!("  {b64}  \n"),
            format!("\u{FEFF}{b64}\r\n"),
        ] {
            assert_eq!(parse_key(content.as_bytes()).map(|k| k.0), Ok(key.0), "{content:?}");
        }
        for bad in [
            STANDARD.encode([0u8; 31]),
            STANDARD.encode([0u8; 33]),
            "not base64 at all!".to_owned(),
            String::new(),
            format!("{b64}{b64}"),
        ] {
            let Err(err) = parse_key(bad.as_bytes()) else { panic!("{bad:?} parsed") };
            assert_eq!(err, "expected the base64 of 32 bytes");
        }
    }

    /// GH#224: the master key cannot be printed or copied, and is wiped when dropped.
    #[test]
    fn the_master_key_has_no_debug_no_clone_and_is_wiped_on_drop() {
        // Ambiguous, so a compile error, if `MasterKey` implements `Debug` or `Clone`.
        trait AmbiguousIfImpl<A> {
            fn check() {}
        }
        impl<T> AmbiguousIfImpl<()> for T {}
        impl<T: fmt::Debug> AmbiguousIfImpl<u8> for T {}
        impl<T: Clone> AmbiguousIfImpl<u16> for T {}
        <MasterKey as AmbiguousIfImpl<_>>::check();

        assert!(std::mem::needs_drop::<MasterKey>());
        let mut key = std::mem::ManuallyDrop::new(new_key());
        assert_ne!(key.0, [0u8; KEY_LEN]);
        // SAFETY: dropped once; afterwards only its plain bytes are read.
        unsafe { std::mem::ManuallyDrop::drop(&mut key) };
        assert_eq!(key.0, [0u8; KEY_LEN]);
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shadoucmdb-key-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("encryption.key");
        std::fs::write(&path, encode_key(&new_key())).unwrap();
        let cfg = EncryptionConfig { key_file: Some(path.clone()), previous_key_file: None };
        for (mode, ok) in [(0o600, true), (0o640, true), (0o400, true), (0o644, false), (0o660, false), (0o602, false)]
        {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let r = Keyring::load(&cfg);
            assert_eq!(r.is_ok(), ok, "mode {mode:o}: {r:?}");
            if let Err(e) = r {
                let e = e.to_string();
                assert!(e.contains("ENCRYPTION_KEY_FILE") && e.contains("chmod 600"), "{e}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_key_names_the_variable() {
        let err = Keyring::load(&EncryptionConfig::default()).unwrap_err().to_string();
        assert!(err.starts_with("ENCRYPTION_KEY_FILE is not set"), "{err}");
        assert!(err.contains("generate-encryption-key"), "{err}");
    }
}
