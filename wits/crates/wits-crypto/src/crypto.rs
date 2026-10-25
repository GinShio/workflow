//! Authenticated encryption sized for git's clean/smudge filters.
//!
//! Three constraints shape everything in this module.
//!
//! **The password reaches nothing but the KDF.** A packet sits in public history
//! for good, so any value in it computed from the password *without* the KDF
//! would let an offline attacker check a guess at the speed of that computation
//! instead of the KDF's — whenever the file's content can be guessed, which for
//! configuration is the normal case. The salt and IV are therefore keyed from the
//! KDF's output, and the password's one use is as the KDF's input.
//!
//! **Determinism.** A clean filter runs on every `git add`, and re-cleaning
//! unchanged content must hand git the blob it already holds, or the file reads
//! as modified forever. The IV is a keyed function of the path and the content,
//! so it is reproducible, and [`seal`] hands back the packet the index already
//! holds when that packet carries the IV this content gets — which is the only
//! thing that keeps [`SaltMode::File`], whose salt is random, stable. The price
//! is that identical content at one path is observably identical across
//! revisions.
//!
//! **One way in.** Every packet — whichever mode sealed it, and the ones sealed
//! before the IV was keyed — has the same layout, base64 over:
//!
//! ```text
//! "Salted__" (8) | salt (8) | "IVed__" (6) | iv (12) | ciphertext | tag (16)
//! ```
//!
//! and [`decrypt`] takes the salt and IV straight from it, so one path opens them
//! all. The file's path is the AEAD's additional data, binding a ciphertext to
//! its location: a blob moved to another path fails to authenticate rather than
//! decrypting there.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Magic prefix written before the salt in every packet.
pub const SALT_HEADER: &[u8] = b"Salted__";
/// Magic prefix written before the IV in every packet.
pub const IV_HEADER: &[u8] = b"IVed__";
/// Number of salt bytes embedded in each packet.
pub const SALT_SIZE: usize = 8;
/// Number of AEAD authentication-tag bytes appended to the ciphertext.
pub const TAG_SIZE: usize = 16;
/// IV/nonce length for both AES-256-GCM and ChaCha20-Poly1305.
pub const NONCE_SIZE: usize = 12;
/// Length of every derived key; both ciphers take 256-bit keys.
pub const KEY_SIZE: usize = 32;
/// The Argon2id memory, in KiB, that every packet up to now was sealed with.
pub const DEFAULT_ARGON2_MEMORY_KIB: u32 = 128 * 1024;
/// The Argon2id lane count that every packet up to now was sealed with.
pub const DEFAULT_ARGON2_LANES: u32 = 2;

/// The message the context key is MACed over to make the key behind every IV.
/// A wire format: change it and no index packet is reused again.
const IV_KEY_LABEL: &[u8] = b"wits-transcrypt iv key";

// ---------------------------------------------------------------------------
// Algorithm enumerations
// ---------------------------------------------------------------------------

/// Supported AEAD cipher algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CipherAlgorithm {
    /// AES-256-GCM (default).
    #[default]
    Aes256Gcm,
    /// ChaCha20-Poly1305.
    ChaCha20Poly1305,
}

impl CipherAlgorithm {
    /// The canonical lowercase name. It is one of the fields the IV is computed
    /// over, so it is a wire format rather than a display name.
    pub const fn as_siv_str(self) -> &'static str {
        match self {
            Self::Aes256Gcm => "aes-256-gcm",
            Self::ChaCha20Poly1305 => "chacha20-poly1305",
        }
    }
}

impl std::str::FromStr for CipherAlgorithm {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "aes-256-gcm" => Ok(Self::Aes256Gcm),
            "chacha20-poly1305" | "chacha20-poly1305-openssl" => Ok(Self::ChaCha20Poly1305),
            other => Err(CryptoError::UnsupportedCipher(other.to_owned())),
        }
    }
}

/// Supported key derivation functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KdfAlgorithm {
    /// PBKDF2 (default; 99 989 rounds for backward compatibility).
    #[default]
    Pbkdf2,
    /// Argon2id (memory-hard; default 4 time-cost iterations, 128 MiB).
    Argon2id,
}

impl KdfAlgorithm {
    /// The PBKDF2 default looks arbitrary because it is: 99 989 is the value
    /// the original tool shipped, and old repositories were encrypted with it.
    /// Changing it would orphan that data, so it stays. Argon2id is newer and
    /// has no such legacy weight; 4 passes over 128 MiB is a sane modern floor.
    pub const fn default_iterations(self) -> u32 {
        match self {
            Self::Pbkdf2 => 99_989,
            Self::Argon2id => 4,
        }
    }
}

impl std::str::FromStr for KdfAlgorithm {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pbkdf2" => Ok(Self::Pbkdf2),
            "argon2id" => Ok(Self::Argon2id),
            other => Err(CryptoError::UnsupportedKdf(other.to_owned())),
        }
    }
}

/// Supported cryptographic hash algorithms — the PBKDF2 PRF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashAlgorithm {
    /// SHA-256 (default).
    #[default]
    Sha256,
    Sha384,
    Sha512,
    Sha3_256,
    Sha3_384,
    Sha3_512,
    /// BLAKE2b (512-bit output).
    Blake2b,
    /// BLAKE2s (256-bit output).
    Blake2s,
}

impl std::str::FromStr for HashAlgorithm {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalised = s.to_lowercase().replace(['-', '_'], "");
        match normalised.as_str() {
            "sha256" => Ok(Self::Sha256),
            "sha384" => Ok(Self::Sha384),
            "sha512" => Ok(Self::Sha512),
            "sha3256" | "sha3-256" => Ok(Self::Sha3_256),
            "sha3384" | "sha3-384" => Ok(Self::Sha3_384),
            "sha3512" | "sha3-512" => Ok(Self::Sha3_512),
            "blake2b" | "blake2b512" => Ok(Self::Blake2b),
            "blake2s" | "blake2s256" => Ok(Self::Blake2s),
            other => Err(CryptoError::UnsupportedHash(other.to_owned())),
        }
    }
}

/// Where a filter's packet salt comes from, and so which key the AEAD runs
/// under. The IV is the same in both modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SaltMode {
    /// A fresh random salt for every content version, so every version is
    /// sealed under its own key. Stable only through [`seal`] handing back the
    /// index's packet, and one KDF dearer than `Context` for every change.
    #[default]
    File,
    /// The context salt itself: one key for the whole context, and a seal that
    /// is deterministic on its own.
    Context,
}

impl SaltMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Context => "context",
        }
    }
}

impl std::str::FromStr for SaltMode {
    type Err = CryptoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "file" => Ok(Self::File),
            "context" => Ok(Self::Context),
            other => Err(CryptoError::UnsupportedSaltMode(other.to_owned())),
        }
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// A KDF with every parameter it takes: what turns a password and a salt into
/// a key. Packets do not record it, so a packet opens only under the parameters
/// that sealed it — changing them belongs with rotating the password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub kdf: KdfAlgorithm,
    /// The PBKDF2 PRF. Argon2id has its own hash and ignores it.
    pub hash: HashAlgorithm,
    /// PBKDF2 rounds, or Argon2id passes.
    pub iterations: u32,
    /// Argon2id memory in KiB. PBKDF2 ignores it.
    pub memory_kib: u32,
    /// Argon2id lanes. PBKDF2 ignores it.
    pub lanes: u32,
}

impl KdfParams {
    /// `kdf` over `hash` at the parameters every packet up to now was sealed with.
    pub const fn defaults(kdf: KdfAlgorithm, hash: HashAlgorithm) -> Self {
        Self {
            kdf,
            hash,
            iterations: kdf.default_iterations(),
            memory_kib: DEFAULT_ARGON2_MEMORY_KIB,
            lanes: DEFAULT_ARGON2_LANES,
        }
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::defaults(KdfAlgorithm::Pbkdf2, HashAlgorithm::Sha256)
    }
}

/// How a filter seals, apart from the password and the file itself.
#[derive(Debug, Clone)]
pub struct SealOptions {
    pub cipher: CipherAlgorithm,
    pub kdf: KdfParams,
    pub mode: SaltMode,
    /// The context's public salt: what the key behind every IV is derived
    /// with, and in [`SaltMode::Context`] the packet salt itself.
    pub context_salt: [u8; SALT_SIZE],
}

/// How to open a packet. The salt and IV come from the packet; everything else
/// has to match what sealed it.
#[derive(Debug, Clone, Default)]
pub struct DecryptOptions {
    pub cipher: CipherAlgorithm,
    pub kdf: KdfParams,
    /// The path a filter sealed the packet for — its AEAD additional data — or
    /// `None` for a packet [`encrypt`] sealed off the filter path.
    pub path: Option<String>,
}

/// What [`seal`] made of a plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sealed {
    /// The `previous` packet already seals this plaintext: hand it back byte
    /// for byte, so git sees the blob it already holds.
    Unchanged,
    /// A new packet, base64, without a trailing newline.
    Packet(String),
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can arise during encryption or decryption.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("unsupported cipher: {0}")]
    UnsupportedCipher(String),
    #[error("unsupported KDF: {0}")]
    UnsupportedKdf(String),
    #[error("unsupported hash algorithm: {0}")]
    UnsupportedHash(String),
    #[error("unsupported salt mode: {0} (expected file or context)")]
    UnsupportedSaltMode(String),
    #[error("AEAD encryption failed")]
    EncryptionFailed,
    #[error("AEAD authentication failed — wrong password, wrong path, or tampered data")]
    AuthenticationFailed,
    #[error("missing 'Salted__' header in encrypted packet")]
    MissingSaltHeader,
    #[error("missing 'IVed__' header in encrypted packet")]
    MissingIVHeader,
    #[error("encrypted packet is too short")]
    DataTooShort,
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("KDF error: {0}")]
    Kdf(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Seal `plaintext` for the file at `path`, as a filter does — or, when
/// `previous` (the packet the index already holds there) carries the IV this
/// plaintext gets, report [`Sealed::Unchanged`].
///
/// The IV is keyed from the context key, so a matching one means the same path,
/// content, password, context salt, KDF parameters, and cipher; that is what
/// makes handing `previous` back safe, and anything else — a missing or
/// non-packet `previous` included — seals afresh. Both modes apply the rule, so a
/// context that switches mode re-seals nothing unchanged.
pub fn seal(
    plaintext: &[u8],
    password: &str,
    path: &str,
    previous: Option<&[u8]>,
    options: &SealOptions,
) -> Result<Sealed, CryptoError> {
    let context_key = derive_key(password.as_bytes(), &options.context_salt, &options.kdf)?;
    let iv = synthetic_iv(&context_key, options.cipher, path, plaintext);
    if previous
        .and_then(|bytes| Packet::parse(bytes).ok())
        .is_some_and(|packet| packet.iv == iv)
    {
        return Ok(Sealed::Unchanged);
    }

    let (salt, key) = match options.mode {
        SaltMode::Context => (options.context_salt, context_key),
        SaltMode::File => {
            let salt = random_salt();
            (salt, derive_key(password.as_bytes(), &salt, &options.kdf)?)
        }
    };
    let body = aead_encrypt(options.cipher, &key, &iv, plaintext, path.as_bytes())?;
    Ok(Sealed::Packet(assemble(&salt, &iv, &body)))
}

/// Seal off the filter path: a fresh random salt and IV, and no path binding.
/// Never for a filter — re-encrypting unchanged content yields a new packet
/// every time, which git reads as a modification.
pub fn encrypt(
    plaintext: &[u8],
    password: &str,
    cipher: CipherAlgorithm,
    kdf: &KdfParams,
) -> Result<String, CryptoError> {
    let salt = random_salt();
    let mut iv = [0u8; NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let key = derive_key(password.as_bytes(), &salt, kdf)?;
    let body = aead_encrypt(cipher, &key, &iv, plaintext, b"")?;
    Ok(assemble(&salt, &iv, &body))
}

/// Open a packet: one sealed by [`seal`] in either mode, by [`encrypt`], or
/// before the IV was keyed — all of them carry their salt and IV.
///
/// There is deliberately no check that the IV is the one the content dictates.
/// The AEAD already rejects a packet whose IV was changed, so such a check
/// could only ever refuse a packet a different sealing rule produced.
pub fn decrypt(
    ciphertext_b64: &str,
    password: &str,
    options: &DecryptOptions,
) -> Result<Vec<u8>, CryptoError> {
    let packet = Packet::parse(ciphertext_b64.as_bytes())?;
    let key = derive_key(password.as_bytes(), &packet.salt, &options.kdf)?;
    let aad = options.path.as_deref().map(str::as_bytes).unwrap_or(b"");
    aead_decrypt(options.cipher, &key, &packet.iv, &packet.body, aad)
}

/// A cheap "is this even one of ours?" check for the git filters.
///
/// A file matched by `.gitattributes` is not necessarily encrypted: it may be
/// plaintext committed before the filter existed, or a binary blob, or anything
/// else. The filters use this to decide whether to attempt decryption at all,
/// so that a non-packet is passed through untouched rather than aborting git's
/// checkout or diff. We only peek at the header — proving it actually decrypts
/// is decryption's job.
pub fn is_encrypted(data: &[u8]) -> bool {
    BASE64
        .decode(data.trim_ascii())
        .map(|decoded| decoded.starts_with(SALT_HEADER))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------------

/// A packet's fields, split out of its base64.
struct Packet {
    salt: [u8; SALT_SIZE],
    iv: [u8; NONCE_SIZE],
    /// The ciphertext followed by its tag.
    body: Vec<u8>,
}

impl Packet {
    fn parse(base64: &[u8]) -> Result<Self, CryptoError> {
        let raw = BASE64.decode(base64.trim_ascii())?;
        let rest = raw
            .strip_prefix(SALT_HEADER)
            .ok_or(CryptoError::MissingSaltHeader)?;
        if rest.len() < SALT_SIZE {
            return Err(CryptoError::DataTooShort);
        }
        let (salt, rest) = rest.split_at(SALT_SIZE);
        let rest = rest
            .strip_prefix(IV_HEADER)
            .ok_or(CryptoError::MissingIVHeader)?;
        if rest.len() < NONCE_SIZE + TAG_SIZE {
            return Err(CryptoError::DataTooShort);
        }
        let (iv, body) = rest.split_at(NONCE_SIZE);
        Ok(Self {
            salt: salt.try_into().expect("split at SALT_SIZE"),
            iv: iv.try_into().expect("split at NONCE_SIZE"),
            body: body.to_vec(),
        })
    }
}

fn assemble(salt: &[u8; SALT_SIZE], iv: &[u8; NONCE_SIZE], body: &[u8]) -> String {
    let mut packet = Vec::with_capacity(
        SALT_HEADER.len() + SALT_SIZE + IV_HEADER.len() + NONCE_SIZE + body.len(),
    );
    packet.extend_from_slice(SALT_HEADER);
    packet.extend_from_slice(salt);
    packet.extend_from_slice(IV_HEADER);
    packet.extend_from_slice(iv);
    packet.extend_from_slice(body);
    BASE64.encode(&packet)
}

fn random_salt() -> [u8; SALT_SIZE] {
    let mut salt = [0u8; SALT_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    salt
}

// ---------------------------------------------------------------------------
// The synthetic IV
// ---------------------------------------------------------------------------

/// The IV a filter seals `plaintext` at `path` under: keyed BLAKE2b over the
/// cipher's name, the path, and the content, keyed from the context key so that
/// no password guess can be checked against it without running the KDF. The
/// cipher is in it so that a context switching cipher without a rotation never
/// reuses one key and nonce under two AEADs.
///
/// The fields are framed `u32be(len) ‖ bytes ‖ 0x00` so that a path ending in
/// bytes that look like content cannot read as a different input; the content,
/// last, needs no frame. Labels and framing are a wire format.
fn synthetic_iv(
    context_key: &[u8; KEY_SIZE],
    cipher: CipherAlgorithm,
    path: &str,
    plaintext: &[u8],
) -> [u8; NONCE_SIZE] {
    let iv_key = keyed_blake2b(context_key, &[IV_KEY_LABEL]);
    let cipher = cipher.as_siv_str().as_bytes();
    let path = path.as_bytes();
    let digest = keyed_blake2b(
        &iv_key,
        &[
            &(cipher.len() as u32).to_be_bytes(),
            cipher,
            b"\x00",
            &(path.len() as u32).to_be_bytes(),
            path,
            b"\x00",
            plaintext,
        ],
    );
    let mut iv = [0u8; NONCE_SIZE];
    iv.copy_from_slice(&digest[..NONCE_SIZE]);
    iv
}

/// BLAKE2b keyed with `key`, 32 bytes out, over the concatenation of `parts`.
fn keyed_blake2b(key: &[u8; KEY_SIZE], parts: &[&[u8]]) -> [u8; KEY_SIZE] {
    use blake2::digest::{consts::U32, FixedOutput, KeyInit, Update};

    let mut mac = <blake2::Blake2bMac<U32> as KeyInit>::new_from_slice(key)
        .expect("a 32-byte key is within BLAKE2b's 64-byte limit");
    for part in parts {
        Update::update(&mut mac, part);
    }
    let mut out = [0u8; KEY_SIZE];
    out.copy_from_slice(&FixedOutput::finalize_fixed(mac));
    out
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

fn derive_key(
    password: &[u8],
    salt: &[u8; SALT_SIZE],
    params: &KdfParams,
) -> Result<[u8; KEY_SIZE], CryptoError> {
    let mut key = [0u8; KEY_SIZE];

    match params.kdf {
        KdfAlgorithm::Pbkdf2 => {
            // Easy to trip on: `pbkdf2_hmac` wants the bare hash type and wraps
            // it in HMAC for you. Handing it `Hmac<H>` compiles but double-wraps
            // and produces the wrong key, so pass `H` directly.
            use pbkdf2::pbkdf2_hmac;

            macro_rules! kdf {
                ($ty:ty) => {
                    pbkdf2_hmac::<$ty>(password, salt, params.iterations, &mut key)
                };
            }
            match params.hash {
                HashAlgorithm::Sha256 => kdf!(sha2::Sha256),
                HashAlgorithm::Sha384 => kdf!(sha2::Sha384),
                HashAlgorithm::Sha512 => kdf!(sha2::Sha512),
                HashAlgorithm::Sha3_256 => kdf!(sha3::Sha3_256),
                HashAlgorithm::Sha3_384 => kdf!(sha3::Sha3_384),
                HashAlgorithm::Sha3_512 => kdf!(sha3::Sha3_512),
                HashAlgorithm::Blake2b | HashAlgorithm::Blake2s => {
                    // BLAKE2's buffer kind doesn't satisfy the trait bound
                    // pbkdf2_hmac requires, so this combination can't be
                    // expressed at all — BLAKE2 just can't be the PBKDF2 PRF.
                    // Fail loudly rather than silently substituting a different
                    // hash.
                    return Err(CryptoError::UnsupportedHash(
                        "BLAKE2 cannot be used as the PBKDF2 PRF; \
                         choose sha256/sha512/sha3-256 or switch to argon2id"
                            .to_owned(),
                    ));
                }
            }
        }

        KdfAlgorithm::Argon2id => {
            use argon2::{Algorithm, Argon2, Params, Version};

            let argon = Params::new(
                params.memory_kib,
                params.iterations,
                params.lanes,
                Some(KEY_SIZE),
            )
            .map_err(|e| CryptoError::Kdf(e.to_string()))?;

            Argon2::new(Algorithm::Argon2id, Version::V0x13, argon)
                .hash_password_into(password, salt, &mut key)
                .map_err(|e| CryptoError::Kdf(e.to_string()))?;
        }
    }

    Ok(key)
}

// ---------------------------------------------------------------------------
// AEAD helpers
// ---------------------------------------------------------------------------

// AES-256-GCM and ChaCha20-Poly1305 share one `aead` trait surface (both ciphers
// re-export the same `aead` crate), so the encrypt/decrypt bodies are generic
// over the AEAD and the per-cipher `match` is a one-line dispatch. The two verbs
// stay separate only to carry their distinct error variants.
use aes_gcm::aead::{self, Aead, KeyInit, Payload};

fn aead_seal<A: KeyInit + Aead>(
    key: &[u8],
    iv: &[u8; NONCE_SIZE],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = A::new_from_slice(key).map_err(|_| CryptoError::EncryptionFailed)?;
    cipher
        .encrypt(
            aead::Nonce::<A>::from_slice(iv),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::EncryptionFailed)
}

fn aead_open<A: KeyInit + Aead>(
    key: &[u8],
    iv: &[u8; NONCE_SIZE],
    ciphertext_with_tag: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = A::new_from_slice(key).map_err(|_| CryptoError::AuthenticationFailed)?;
    cipher
        .decrypt(
            aead::Nonce::<A>::from_slice(iv),
            Payload {
                msg: ciphertext_with_tag,
                aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)
}

fn aead_encrypt(
    cipher: CipherAlgorithm,
    key: &[u8],
    iv: &[u8; NONCE_SIZE],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    match cipher {
        CipherAlgorithm::Aes256Gcm => aead_seal::<aes_gcm::Aes256Gcm>(key, iv, plaintext, aad),
        CipherAlgorithm::ChaCha20Poly1305 => {
            aead_seal::<chacha20poly1305::ChaCha20Poly1305>(key, iv, plaintext, aad)
        }
    }
}

fn aead_decrypt(
    cipher: CipherAlgorithm,
    key: &[u8],
    iv: &[u8; NONCE_SIZE],
    ciphertext_with_tag: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    match cipher {
        CipherAlgorithm::Aes256Gcm => {
            aead_open::<aes_gcm::Aes256Gcm>(key, iv, ciphertext_with_tag, aad)
        }
        CipherAlgorithm::ChaCha20Poly1305 => {
            aead_open::<chacha20poly1305::ChaCha20Poly1305>(key, iv, ciphertext_with_tag, aad)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; SALT_SIZE] = *b"ctxsalt!";

    /// Argon2id small enough to keep the suite fast; the construction does not
    /// care how expensive the KDF is.
    const FAST_ARGON2: KdfParams = KdfParams {
        kdf: KdfAlgorithm::Argon2id,
        hash: HashAlgorithm::Blake2b,
        iterations: 1,
        memory_kib: 8 * 1024,
        lanes: 1,
    };
    const FAST_PBKDF2: KdfParams = KdfParams {
        kdf: KdfAlgorithm::Pbkdf2,
        hash: HashAlgorithm::Sha256,
        iterations: 10,
        memory_kib: DEFAULT_ARGON2_MEMORY_KIB,
        lanes: DEFAULT_ARGON2_LANES,
    };

    fn options(cipher: CipherAlgorithm, kdf: KdfParams, mode: SaltMode) -> SealOptions {
        SealOptions {
            cipher,
            kdf,
            mode,
            context_salt: SALT,
        }
    }

    fn sealed(outcome: Sealed) -> String {
        match outcome {
            Sealed::Packet(packet) => packet,
            Sealed::Unchanged => panic!("expected a fresh packet"),
        }
    }

    fn open(packet: &str, password: &str, path: &str, options: &SealOptions) -> Vec<u8> {
        decrypt(
            packet,
            password,
            &DecryptOptions {
                cipher: options.cipher,
                kdf: options.kdf,
                path: Some(path.to_owned()),
            },
        )
        .unwrap()
    }

    fn fields(packet: &str) -> Packet {
        Packet::parse(packet.as_bytes()).unwrap()
    }

    // -----------------------------------------------------------------------
    // The construction, against an independent implementation
    // -----------------------------------------------------------------------

    // These two packets were computed by an independent Python implementation
    // of the construction — argon2-cffi, `hashlib.blake2b`, and `cryptography`
    // following the design's derivation, not this code: `context` mode,
    // password "kat-test-passphrase", context salt 0011223344556677, path
    // "secret/kat.txt", plaintext "known answer\n". Reproducing them byte for
    // byte pins the KDF use, the IV key label, the IV framing, and the AEAD.
    const KAT_SALT: [u8; SALT_SIZE] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
    const KAT_PASSWORD: &str = "kat-test-passphrase";
    const KAT_PATH: &str = "secret/kat.txt";
    const KAT_PLAINTEXT: &[u8] = b"known answer\n";

    #[test]
    fn reproduces_the_independent_chacha20_argon2id_vector() {
        let options = SealOptions {
            context_salt: KAT_SALT,
            ..options(
                CipherAlgorithm::ChaCha20Poly1305,
                FAST_ARGON2,
                SaltMode::Context,
            )
        };
        let packet = sealed(seal(KAT_PLAINTEXT, KAT_PASSWORD, KAT_PATH, None, &options).unwrap());
        assert_eq!(
            packet,
            "U2FsdGVkX18AESIzRFVmd0lWZWRfX8g3lagbjv0AG7CMbh86mHQNRp4cbL27LsXDbAuYXTEoMrcUsNA4HRlt"
        );
    }

    #[test]
    fn reproduces_the_independent_aes_pbkdf2_vector() {
        let options = SealOptions {
            context_salt: KAT_SALT,
            ..options(
                CipherAlgorithm::Aes256Gcm,
                KdfParams {
                    iterations: 1000,
                    ..FAST_PBKDF2
                },
                SaltMode::Context,
            )
        };
        let packet = sealed(seal(KAT_PLAINTEXT, KAT_PASSWORD, KAT_PATH, None, &options).unwrap());
        assert_eq!(
            packet,
            "U2FsdGVkX18AESIzRFVmd0lWZWRfX1rk+kKsI10lvzFZBymPPIAToFfEiS2OjYk5oHqSYNF00+akoVdjLrRQ"
        );
    }

    // -----------------------------------------------------------------------
    // Packets sealed before the IV was keyed still open
    // -----------------------------------------------------------------------

    // This base64 was produced by the original Python `transcrypt`
    // (workflow/core/crypto.py), not by this code: AES-256-GCM / PBKDF2 /
    // SHA-256, 1000 iterations, path "docs/note.txt".
    const REFERENCE_VECTOR: &str =
        "U2FsdGVkX19eEmo+67ljNUlWZWRfX6BVVr6P6FZp1v1RY6ZPe7lRhl2X2XJSyQVUbJnvvA7+VrppiD0+mFsmSxZEOQ==";

    #[test]
    fn decrypts_vector_from_the_reference_implementation() {
        let opts = DecryptOptions {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfParams {
                iterations: 1000,
                ..KdfParams::defaults(KdfAlgorithm::Pbkdf2, HashAlgorithm::Sha256)
            },
            path: Some("docs/note.txt".to_owned()),
        };
        let plaintext = decrypt(REFERENCE_VECTOR, "test-passphrase", &opts).unwrap();
        assert_eq!(plaintext, b"hello transcrypt\n");
    }

    // Sealed by wits before the IV was keyed — salt and IV cut from an unkeyed
    // hash over the password — with ChaCha20-Poly1305 / Argon2id at its fixed
    // 128 MiB over 2 lanes, 1 pass, password "legacy-test-passphrase", path
    // "secret/legacy.txt". It opens through the one decryption path.
    const LEGACY_ARGON2ID_VECTOR: &str = "U2FsdGVkX1+2RKPTiuszSElWZWRfXxNQm7BWIU7mwMeD4oXRBIDXtpTzDT5M5xPTuyfoYhhYCkC4GskPkyKht3ZiubGcXHwfBZDMtyKOjzpO";

    #[test]
    fn opens_a_packet_sealed_before_the_iv_was_keyed() {
        let opts = DecryptOptions {
            cipher: CipherAlgorithm::ChaCha20Poly1305,
            kdf: KdfParams {
                iterations: 1,
                ..KdfParams::defaults(KdfAlgorithm::Argon2id, HashAlgorithm::Blake2b)
            },
            path: Some("secret/legacy.txt".to_owned()),
        };
        let plaintext = decrypt(LEGACY_ARGON2ID_VECTOR, "legacy-test-passphrase", &opts).unwrap();
        assert_eq!(plaintext, b"sealed before the iv was keyed\n");
    }

    // -----------------------------------------------------------------------
    // Sealing
    // -----------------------------------------------------------------------

    #[test]
    fn seals_and_opens_in_both_modes_and_ciphers() {
        for cipher in [
            CipherAlgorithm::Aes256Gcm,
            CipherAlgorithm::ChaCha20Poly1305,
        ] {
            for mode in [SaltMode::File, SaltMode::Context] {
                for kdf in [FAST_ARGON2, FAST_PBKDF2] {
                    let options = options(cipher, kdf, mode);
                    let packet =
                        sealed(seal(b"Secret message", "pass", "a/b.txt", None, &options).unwrap());
                    assert_eq!(
                        open(&packet, "pass", "a/b.txt", &options),
                        b"Secret message"
                    );
                }
            }
        }
    }

    #[test]
    fn context_mode_is_deterministic() {
        let options = options(
            CipherAlgorithm::ChaCha20Poly1305,
            FAST_ARGON2,
            SaltMode::Context,
        );
        let a = sealed(seal(b"same", "pass", "f.txt", None, &options).unwrap());
        let b = sealed(seal(b"same", "pass", "f.txt", None, &options).unwrap());
        assert_eq!(a, b);
        assert_eq!(fields(&a).salt, SALT);
    }

    #[test]
    fn file_mode_salts_differ_but_the_iv_does_not() {
        let options = options(
            CipherAlgorithm::ChaCha20Poly1305,
            FAST_ARGON2,
            SaltMode::File,
        );
        let a = sealed(seal(b"same", "pass", "f.txt", None, &options).unwrap());
        let b = sealed(seal(b"same", "pass", "f.txt", None, &options).unwrap());
        assert_ne!(a, b, "a fresh random salt per seal");
        assert_ne!(fields(&a).salt, fields(&b).salt);
        assert_eq!(
            fields(&a).iv,
            fields(&b).iv,
            "the IV depends on content alone"
        );
    }

    #[test]
    fn both_modes_give_one_iv() {
        let file = options(CipherAlgorithm::Aes256Gcm, FAST_PBKDF2, SaltMode::File);
        let context = SealOptions {
            mode: SaltMode::Context,
            ..file.clone()
        };
        let a = sealed(seal(b"x", "pass", "f.txt", None, &file).unwrap());
        let b = sealed(seal(b"x", "pass", "f.txt", None, &context).unwrap());
        assert_eq!(fields(&a).iv, fields(&b).iv);
    }

    #[test]
    fn the_iv_is_keyed_from_the_context_key() {
        // The same password, path, and content under another context salt: a
        // different context key, so a different IV.
        let a = options(CipherAlgorithm::Aes256Gcm, FAST_PBKDF2, SaltMode::Context);
        let b = SealOptions {
            context_salt: *b"othersal",
            ..a.clone()
        };
        let pa = sealed(seal(b"x", "pass", "f.txt", None, &a).unwrap());
        let pb = sealed(seal(b"x", "pass", "f.txt", None, &b).unwrap());
        assert_ne!(fields(&pa).iv, fields(&pb).iv);
    }

    #[test]
    fn hands_back_the_index_packet_for_unchanged_content() {
        let options = options(
            CipherAlgorithm::ChaCha20Poly1305,
            FAST_ARGON2,
            SaltMode::File,
        );
        let previous = sealed(seal(b"v1\n", "pass", "f.txt", None, &options).unwrap());
        let again = seal(
            b"v1\n",
            "pass",
            "f.txt",
            Some(format!("{previous}\n").as_bytes()),
            &options,
        );
        assert_eq!(again.unwrap(), Sealed::Unchanged);
    }

    #[test]
    fn reseals_whatever_differs_from_the_index_packet() {
        let base = options(
            CipherAlgorithm::ChaCha20Poly1305,
            FAST_ARGON2,
            SaltMode::File,
        );
        let previous = sealed(seal(b"v1\n", "pass", "f.txt", None, &base).unwrap());
        let previous = Some(previous.as_bytes());

        let fresh = |plaintext: &[u8], password, path, options: &SealOptions| {
            seal(plaintext, password, path, previous, options).unwrap()
        };
        assert!(matches!(
            fresh(b"v2\n", "pass", "f.txt", &base),
            Sealed::Packet(_)
        ));
        assert!(matches!(
            fresh(b"v1\n", "other", "f.txt", &base),
            Sealed::Packet(_)
        ));
        assert!(matches!(
            fresh(b"v1\n", "pass", "g.txt", &base),
            Sealed::Packet(_)
        ));
        let other_salt = SealOptions {
            context_salt: *b"othersal",
            ..base.clone()
        };
        assert!(matches!(
            fresh(b"v1\n", "pass", "f.txt", &other_salt),
            Sealed::Packet(_)
        ));
        let other_cipher = SealOptions {
            cipher: CipherAlgorithm::Aes256Gcm,
            ..base.clone()
        };
        assert!(matches!(
            fresh(b"v1\n", "pass", "f.txt", &other_cipher),
            Sealed::Packet(_)
        ));
    }

    #[test]
    fn ignores_an_index_entry_that_is_not_a_packet() {
        let options = options(CipherAlgorithm::Aes256Gcm, FAST_PBKDF2, SaltMode::File);
        let outcome = seal(b"v1\n", "pass", "f.txt", Some(b"v1\n"), &options).unwrap();
        assert!(matches!(outcome, Sealed::Packet(_)));
    }

    #[test]
    fn random_mode_roundtrip() {
        let ct = encrypt(
            b"hello random",
            "pass",
            CipherAlgorithm::Aes256Gcm,
            &FAST_PBKDF2,
        )
        .unwrap();
        let opts = DecryptOptions {
            kdf: FAST_PBKDF2,
            ..Default::default()
        };
        assert_eq!(decrypt(&ct, "pass", &opts).unwrap(), b"hello random");
    }

    // -----------------------------------------------------------------------
    // KDF
    // -----------------------------------------------------------------------

    #[test]
    fn both_kdfs_produce_a_full_key() {
        for params in [FAST_PBKDF2, FAST_ARGON2] {
            assert_eq!(
                derive_key(b"pass", &[1u8; 8], &params).unwrap().len(),
                KEY_SIZE
            );
        }
    }

    #[test]
    fn argon2id_rejects_impossible_parameters() {
        let params = KdfParams {
            memory_kib: 1,
            ..FAST_ARGON2
        };
        assert!(matches!(
            derive_key(b"pass", &[1u8; 8], &params),
            Err(CryptoError::Kdf(_))
        ));
    }

    #[test]
    fn pbkdf2_paired_with_blake2_is_refused() {
        // BLAKE2 can't drive PBKDF2; the failure must be explicit rather than a
        // silent substitution to another hash.
        let options = options(
            CipherAlgorithm::Aes256Gcm,
            KdfParams {
                hash: HashAlgorithm::Blake2b,
                ..FAST_PBKDF2
            },
            SaltMode::File,
        );
        let err = seal(b"data", "pass", "f.txt", None, &options).unwrap_err();
        assert!(matches!(err, CryptoError::UnsupportedHash(_)));
    }

    // -----------------------------------------------------------------------
    // What fails to open
    // -----------------------------------------------------------------------

    fn sealed_packet() -> (String, SealOptions) {
        let options = options(CipherAlgorithm::Aes256Gcm, FAST_PBKDF2, SaltMode::File);
        let packet = sealed(seal(b"secret", "correct", "f.txt", None, &options).unwrap());
        (packet, options)
    }

    fn try_open(packet: &str, password: &str, path: &str, options: &SealOptions) -> CryptoError {
        decrypt(
            packet,
            password,
            &DecryptOptions {
                cipher: options.cipher,
                kdf: options.kdf,
                path: Some(path.to_owned()),
            },
        )
        .unwrap_err()
    }

    #[test]
    fn wrong_password_fails_with_auth_error() {
        let (packet, options) = sealed_packet();
        let err = try_open(&packet, "wrong", "f.txt", &options);
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn wrong_path_fails_authentication() {
        let (packet, options) = sealed_packet();
        let err = try_open(&packet, "correct", "g.txt", &options);
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let (packet, options) = sealed_packet();
        let mut raw = BASE64.decode(&packet).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let err = try_open(&BASE64.encode(&raw), "correct", "f.txt", &options);
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn a_changed_iv_fails_authentication() {
        // What a re-check of the IV would have caught, the AEAD already does.
        let (packet, options) = sealed_packet();
        let mut raw = BASE64.decode(&packet).unwrap();
        raw[SALT_HEADER.len() + SALT_SIZE + IV_HEADER.len()] ^= 0x01;
        let err = try_open(&BASE64.encode(&raw), "correct", "f.txt", &options);
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn missing_salt_header_returns_error() {
        let garbage = BASE64.encode(b"WrongHeader12345678IVed__012345678901ciphertext");
        let err = decrypt(&garbage, "pass", &DecryptOptions::default()).unwrap_err();
        assert!(matches!(err, CryptoError::MissingSaltHeader));
    }

    #[test]
    fn missing_iv_header_returns_error() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"Salted__");
        raw.extend_from_slice(b"12345678"); // salt
        raw.extend_from_slice(b"BadIVHdr"); // wrong IV header
        let err = decrypt(&BASE64.encode(&raw), "pass", &DecryptOptions::default()).unwrap_err();
        assert!(matches!(err, CryptoError::MissingIVHeader));
    }

    #[test]
    fn rejects_input_that_is_not_base64() {
        let err = decrypt("this is not base64!", "pass", &DecryptOptions::default()).unwrap_err();
        assert!(matches!(err, CryptoError::Base64(_)));
    }

    // -----------------------------------------------------------------------
    // Names and detection
    // -----------------------------------------------------------------------

    #[test]
    fn algorithm_names_parse_and_reject_cleanly() {
        use std::str::FromStr;
        assert_eq!(
            CipherAlgorithm::from_str("aes-256-gcm").unwrap(),
            CipherAlgorithm::Aes256Gcm
        );
        assert_eq!(
            HashAlgorithm::from_str("sha3-256").unwrap(),
            HashAlgorithm::Sha3_256
        );
        assert_eq!(SaltMode::from_str("Context").unwrap(), SaltMode::Context);
        assert!(matches!(
            CipherAlgorithm::from_str("rot13"),
            Err(CryptoError::UnsupportedCipher(_))
        ));
        assert!(matches!(
            KdfAlgorithm::from_str("scrypt"),
            Err(CryptoError::UnsupportedKdf(_))
        ));
        assert!(matches!(
            HashAlgorithm::from_str("md5"),
            Err(CryptoError::UnsupportedHash(_))
        ));
        assert!(matches!(
            SaltMode::from_str("path"),
            Err(CryptoError::UnsupportedSaltMode(_))
        ));
    }

    #[test]
    fn is_encrypted_recognises_only_our_packets() {
        // A real packet (trailing newline tolerated, as git may add one).
        assert!(is_encrypted(REFERENCE_VECTOR.as_bytes()));
        let mut with_newline = REFERENCE_VECTOR.as_bytes().to_vec();
        with_newline.push(b'\n');
        assert!(is_encrypted(&with_newline));

        // Plaintext, binary, and valid-but-unrelated base64 are all "not ours".
        assert!(!is_encrypted(b"# just a config comment\n"));
        assert!(!is_encrypted(&[0xff, 0xfe, 0x00, 0x42])); // not UTF-8 / not base64
        assert!(!is_encrypted(
            &BASE64.encode(b"some other data").into_bytes()
        ));
    }
}
