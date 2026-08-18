//! Authenticated encryption sized for git's clean/smudge filters.
//!
//! Two domain constraints shape everything in this module.
//!
//! First, **compatibility**. Repositories already hold data encrypted by the
//! earlier `transcrypt` tool, so the on-disk packet layout is fixed — we must
//! reproduce it byte for byte or those repositories become unreadable. The
//! packet is base64 over:
//!
//! ```text
//! "Salted__" (8) | salt (8) | "IVed__" (6) | iv (12) | ciphertext | tag (16)
//! ```
//!
//! Second, **determinism**. A clean filter runs on every `git add`, and if
//! encrypting unchanged content produced fresh randomness each time, git would
//! report the file as modified forever. So the default mode derives the salt
//! and IV from the content itself (a synthetic-IV construction): same input,
//! same output, no phantom diffs. The trade-off is that identical plaintext is
//! observably identical when encrypted — acceptable here, and the price of a
//! filter that doesn't fight git.
//!
//! The derivation also folds in the file path as the AEAD's additional data.
//! That binds a ciphertext to its location, so moving an encrypted blob to a
//! different path makes it fail to authenticate rather than silently decrypt —
//! it closes a file-swap avenue that pure content encryption would leave open.
//!
//! A non-deterministic random mode also exists for callers off the filter path
//! that can afford fresh randomness and want the stronger guarantee.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
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
    /// Key length in bytes (both ciphers use 256-bit keys).
    pub const fn key_size(self) -> usize {
        32
    }

    /// The canonical lowercase name used in packet metadata and SIV strings.
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

    /// Canonical name used in SIV strings.
    pub const fn as_siv_str(self) -> &'static str {
        match self {
            Self::Pbkdf2 => "pbkdf2",
            Self::Argon2id => "argon2id",
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

/// Supported cryptographic hash algorithms.
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

impl HashAlgorithm {
    // These exact spellings are baked into the SIV derivation string, which is
    // in turn baked into every existing ciphertext. They are a wire format, not
    // a display name — changing one silently breaks decryption of old data.
    pub const fn as_siv_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
            Self::Sha3_256 => "sha3256",
            Self::Sha3_384 => "sha3384",
            Self::Sha3_512 => "sha3512",
            Self::Blake2b => "blake2b",
            Self::Blake2s => "blake2s",
        }
    }
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

// ---------------------------------------------------------------------------
// SIV mode
// ---------------------------------------------------------------------------

/// Controls how the salt and IV/nonce are produced for each encryption.
#[derive(Debug, Clone)]
pub enum SivMode {
    /// **Local-deterministic** (the standard transcrypt mode).
    ///
    /// Both salt *and* IV are derived from the content via the SIV
    /// construction.  `context` is typically the file path; it is also used
    /// as the AEAD additional-data (AAD) binding the ciphertext to a specific
    /// file, preventing silent file-swap attacks.
    LocalDeterministic { context: String },
    /// Fresh random salt and IV. Strongest, but non-deterministic, so it must
    /// not be used in a filter — re-encrypting an unchanged file would yield a
    /// new ciphertext and a phantom diff. Here for callers off the filter path.
    Random,
}

// ---------------------------------------------------------------------------
// Options structs
// ---------------------------------------------------------------------------

/// Configuration for an encryption operation.
#[derive(Debug, Clone)]
pub struct EncryptOptions {
    pub cipher: CipherAlgorithm,
    pub kdf: KdfAlgorithm,
    pub hash: HashAlgorithm,
    /// Explicit iteration count; falls back to [`KdfAlgorithm::default_iterations`].
    pub iterations: Option<u32>,
    pub siv_mode: SivMode,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        Self {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Sha256,
            iterations: None,
            siv_mode: SivMode::Random,
        }
    }
}

/// Configuration for a decryption operation.
#[derive(Debug, Clone, Default)]
pub struct DecryptOptions {
    pub cipher: CipherAlgorithm,
    pub kdf: KdfAlgorithm,
    pub hash: HashAlgorithm,
    /// Must match the value used during encryption.
    pub iterations: Option<u32>,
    /// When `Some`, the context bytes are used as AEAD AAD and the SIV
    /// integrity check is performed after decryption.
    pub verify_context: Option<String>,
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
    #[error("AEAD encryption failed")]
    EncryptionFailed,
    #[error("AEAD authentication failed — wrong password or tampered data")]
    AuthenticationFailed,
    #[error("SIV integrity check failed — content may have been tampered with")]
    IntegrityCheckFailed,
    #[error("missing 'Salted__' header in encrypted packet")]
    MissingSaltHeader,
    #[error("missing 'IVed__' header in encrypted packet")]
    MissingIVHeader,
    #[error("encrypted packet is too short")]
    DataTooShort,
    #[error("hash digest ({digest_len} B) too short to produce SIV material ({needed} B)")]
    DigestTooShort { digest_len: usize, needed: usize },
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("KDF error: {0}")]
    Kdf(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the finished packet as base64, ready to hand straight to git.
pub fn encrypt(
    plaintext: &[u8],
    password: &str,
    options: &EncryptOptions,
) -> Result<String, CryptoError> {
    let iters = options
        .iterations
        .unwrap_or_else(|| options.kdf.default_iterations());

    let mut salt = [0u8; SALT_SIZE];
    let mut iv = [0u8; NONCE_SIZE];

    // 1. Determine salt, IV, and AAD
    let aad: &[u8] = match &options.siv_mode {
        SivMode::LocalDeterministic { context } => {
            compute_siv_params(
                password.as_bytes(),
                plaintext,
                context.as_bytes(),
                AlgoSuite {
                    cipher: options.cipher,
                    kdf: options.kdf,
                    hash: options.hash,
                    iterations: iters,
                },
                &mut salt,
                &mut iv,
            )?;
            context.as_bytes()
        }
        SivMode::Random => {
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut salt);
            rand::thread_rng().fill_bytes(&mut iv);
            b""
        }
    };

    // 2. Derive key
    let key = derive_key(password.as_bytes(), &salt, options.hash, options.kdf, iters)?;

    // 3. AEAD encrypt — returns ciphertext || tag
    let ciphertext_with_tag = aead_encrypt(options.cipher, &key, &iv, plaintext, aad)?;

    // 4. Assemble packet
    let mut packet = Vec::with_capacity(
        SALT_HEADER.len() + SALT_SIZE + IV_HEADER.len() + NONCE_SIZE + ciphertext_with_tag.len(),
    );
    packet.extend_from_slice(SALT_HEADER);
    packet.extend_from_slice(&salt);
    packet.extend_from_slice(IV_HEADER);
    packet.extend_from_slice(&iv);
    packet.extend_from_slice(&ciphertext_with_tag);

    // 5. Base64-encode
    Ok(BASE64.encode(&packet))
}

pub fn decrypt(
    ciphertext_b64: &str,
    password: &str,
    options: &DecryptOptions,
) -> Result<Vec<u8>, CryptoError> {
    // 1. Base64-decode
    let packet = BASE64.decode(ciphertext_b64.trim())?;

    let mut offset = 0;

    // 2. Parse salt
    if !packet
        .get(offset..)
        .is_some_and(|s| s.starts_with(SALT_HEADER))
    {
        return Err(CryptoError::MissingSaltHeader);
    }
    offset += SALT_HEADER.len();

    if packet.len() < offset + SALT_SIZE {
        return Err(CryptoError::DataTooShort);
    }
    let salt: [u8; SALT_SIZE] = packet[offset..offset + SALT_SIZE].try_into().unwrap();
    offset += SALT_SIZE;

    // 3. Parse IV
    if !packet
        .get(offset..)
        .is_some_and(|s| s.starts_with(IV_HEADER))
    {
        return Err(CryptoError::MissingIVHeader);
    }
    offset += IV_HEADER.len();

    if packet.len() < offset + NONCE_SIZE {
        return Err(CryptoError::DataTooShort);
    }
    let iv: [u8; NONCE_SIZE] = packet[offset..offset + NONCE_SIZE].try_into().unwrap();
    offset += NONCE_SIZE;

    // 4. Remaining bytes = ciphertext || tag
    let ciphertext_with_tag = &packet[offset..];
    if ciphertext_with_tag.len() < TAG_SIZE {
        return Err(CryptoError::DataTooShort);
    }

    // 5. Derive key
    let iters = options
        .iterations
        .unwrap_or_else(|| options.kdf.default_iterations());
    let key = derive_key(password.as_bytes(), &salt, options.hash, options.kdf, iters)?;

    // 6. AEAD decrypt
    let aad: &[u8] = options
        .verify_context
        .as_deref()
        .map(str::as_bytes)
        .unwrap_or(b"");
    let plaintext = aead_decrypt(options.cipher, &key, &iv, ciphertext_with_tag, aad)?;

    // The AEAD tag already proves the ciphertext wasn't tampered with under
    // this key. This extra step proves something subtly different: that the IV
    // in the packet is the one the *content itself* dictates. Re-deriving it
    // and comparing catches an IV that was swapped for another validly-keyed
    // packet's. We compare only the IV, never the salt, so the same check holds
    // whether the salt came from the content or from a global seed.
    if let Some(context) = &options.verify_context {
        let mut expected_salt = [0u8; SALT_SIZE];
        let mut expected_iv = [0u8; NONCE_SIZE];
        compute_siv_params(
            password.as_bytes(),
            &plaintext,
            context.as_bytes(),
            AlgoSuite {
                cipher: options.cipher,
                kdf: options.kdf,
                hash: options.hash,
                iterations: iters,
            },
            &mut expected_salt,
            &mut expected_iv,
        )?;
        if iv != expected_iv {
            return Err(CryptoError::IntegrityCheckFailed);
        }
    }

    Ok(plaintext)
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
// SIV parameter computation
// ---------------------------------------------------------------------------

/// The four parameters that get serialized into the SIV derivation string, and
/// so define a ciphertext's algorithm identity. Grouped together because they
/// always travel together and have to be reproduced exactly to decrypt.
#[derive(Clone, Copy)]
struct AlgoSuite {
    cipher: CipherAlgorithm,
    kdf: KdfAlgorithm,
    hash: HashAlgorithm,
    iterations: u32,
}

/// Derives the salt and IV deterministically from the content plus the
/// algorithm parameters. The field framing below (length-prefix, value, NUL
/// separator) is part of the fixed wire format — it exists so that, say, a
/// password ending in the bytes of the next field can't be confused for a
/// different input that hashes the same way.
fn compute_siv_params(
    password: &[u8],
    data: &[u8],
    context: &[u8],
    algo: AlgoSuite,
    out_salt: &mut [u8; SALT_SIZE],
    out_iv: &mut [u8; NONCE_SIZE],
) -> Result<(), CryptoError> {
    let algo_params = format!(
        "{}:{}:{}:{}",
        algo.hash.as_siv_str(),
        algo.cipher.as_siv_str(),
        algo.iterations,
        algo.kdf.as_siv_str(),
    );

    // Helper that feeds all fields into a `digest::Update` implementor. The
    // `digest` traits are used via `sha2`'s re-export so we don't need a direct
    // dependency on the `digest` crate (all RustCrypto hashes share it).
    fn feed<H: sha2::digest::Update>(h: &mut H, algo: &[u8], pwd: &[u8], ctx: &[u8], data: &[u8]) {
        let sep = b"\x00";
        h.update(&(algo.len() as u32).to_be_bytes());
        h.update(algo);
        h.update(sep);
        h.update(&(pwd.len() as u32).to_be_bytes());
        h.update(pwd);
        h.update(sep);
        h.update(&(ctx.len() as u32).to_be_bytes());
        h.update(ctx);
        h.update(sep);
        h.update(data);
    }

    // Helper that extracts iv || salt from the leading bytes of a digest.
    fn extract(
        digest_bytes: &[u8],
        out_iv: &mut [u8; NONCE_SIZE],
        out_salt: &mut [u8; SALT_SIZE],
    ) -> Result<(), CryptoError> {
        let needed = NONCE_SIZE + SALT_SIZE;
        if digest_bytes.len() < needed {
            return Err(CryptoError::DigestTooShort {
                digest_len: digest_bytes.len(),
                needed,
            });
        }
        out_iv.copy_from_slice(&digest_bytes[..NONCE_SIZE]);
        out_salt.copy_from_slice(&digest_bytes[NONCE_SIZE..NONCE_SIZE + SALT_SIZE]);
        Ok(())
    }

    let ap = algo_params.as_bytes();

    // Every RustCrypto hash implements the same `digest::Digest`/`Update` traits
    // (re-exported identically by sha2/sha3/blake2), so one macro covers all
    // eight: build the hasher, feed the framed fields, split out iv‖salt.
    use sha2::digest::Digest;
    macro_rules! siv {
        ($ty:ty) => {{
            let mut h = <$ty>::new();
            feed(&mut h, ap, password, context, data);
            extract(h.finalize().as_slice(), out_iv, out_salt)
        }};
    }
    match algo.hash {
        HashAlgorithm::Sha256 => siv!(sha2::Sha256),
        HashAlgorithm::Sha384 => siv!(sha2::Sha384),
        HashAlgorithm::Sha512 => siv!(sha2::Sha512),
        HashAlgorithm::Sha3_256 => siv!(sha3::Sha3_256),
        HashAlgorithm::Sha3_384 => siv!(sha3::Sha3_384),
        HashAlgorithm::Sha3_512 => siv!(sha3::Sha3_512),
        HashAlgorithm::Blake2b => siv!(blake2::Blake2b512),
        HashAlgorithm::Blake2s => siv!(blake2::Blake2s256),
    }
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

fn derive_key(
    password: &[u8],
    salt: &[u8; SALT_SIZE],
    hash: HashAlgorithm,
    kdf: KdfAlgorithm,
    iters: u32,
) -> Result<Vec<u8>, CryptoError> {
    let key_len = CipherAlgorithm::Aes256Gcm.key_size(); // Both ciphers use 32 B
    let mut key = vec![0u8; key_len];

    match kdf {
        KdfAlgorithm::Pbkdf2 => {
            // Easy to trip on: `pbkdf2_hmac` wants the bare hash type and wraps
            // it in HMAC for you. Handing it `Hmac<H>` compiles but double-wraps
            // and produces the wrong key, so pass `H` directly.
            use pbkdf2::pbkdf2_hmac;

            macro_rules! kdf {
                ($ty:ty) => {
                    pbkdf2_hmac::<$ty>(password, salt, iters, &mut key)
                };
            }
            match hash {
                HashAlgorithm::Sha256 => kdf!(sha2::Sha256),
                HashAlgorithm::Sha384 => kdf!(sha2::Sha384),
                HashAlgorithm::Sha512 => kdf!(sha2::Sha512),
                HashAlgorithm::Sha3_256 => kdf!(sha3::Sha3_256),
                HashAlgorithm::Sha3_384 => kdf!(sha3::Sha3_384),
                HashAlgorithm::Sha3_512 => kdf!(sha3::Sha3_512),
                HashAlgorithm::Blake2b | HashAlgorithm::Blake2s => {
                    // BLAKE2's buffer kind doesn't satisfy the trait bound
                    // pbkdf2_hmac requires, so this combination can't be
                    // expressed at all. BLAKE2 still works fine for SIV hashing;
                    // it just can't be the PBKDF2 PRF. Fail loudly rather than
                    // silently substituting a different hash.
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

            // 128 MiB / 2 lanes are fixed parameters of the historical format,
            // not tunables — they have to match what old data was sealed with.
            let params = Params::new(131_072, iters, 2, Some(key_len))
                .map_err(|e| CryptoError::Kdf(e.to_string()))?;

            Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
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

    // -----------------------------------------------------------------------
    // SIV determinism
    // -----------------------------------------------------------------------

    #[test]
    fn siv_is_deterministic_for_same_inputs() {
        let (mut s1, mut iv1) = ([0u8; SALT_SIZE], [0u8; NONCE_SIZE]);
        let (mut s2, mut iv2) = ([0u8; SALT_SIZE], [0u8; NONCE_SIZE]);
        let algo = AlgoSuite {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Sha256,
            iterations: 100,
        };
        compute_siv_params(
            b"password",
            b"hello world",
            b"path/to/file.txt",
            algo,
            &mut s1,
            &mut iv1,
        )
        .unwrap();
        compute_siv_params(
            b"password",
            b"hello world",
            b"path/to/file.txt",
            algo,
            &mut s2,
            &mut iv2,
        )
        .unwrap();
        assert_eq!(s1, s2);
        assert_eq!(iv1, iv2);
    }

    #[test]
    fn siv_changes_when_data_changes() {
        let (mut s1, mut iv1) = ([0u8; SALT_SIZE], [0u8; NONCE_SIZE]);
        let (mut s2, mut iv2) = ([0u8; SALT_SIZE], [0u8; NONCE_SIZE]);
        let algo = AlgoSuite {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Sha256,
            iterations: 100,
        };
        compute_siv_params(b"password", b"data1", b"ctx", algo, &mut s1, &mut iv1).unwrap();
        compute_siv_params(b"password", b"data2", b"ctx", algo, &mut s2, &mut iv2).unwrap();
        assert_ne!(s1, s2);
        assert_ne!(iv1, iv2);
    }

    // -----------------------------------------------------------------------
    // KDF
    // -----------------------------------------------------------------------

    #[test]
    fn pbkdf2_produces_correct_key_length() {
        let key = derive_key(
            b"pass",
            &[1u8; 8],
            HashAlgorithm::Sha256,
            KdfAlgorithm::Pbkdf2,
            100,
        )
        .unwrap();
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn argon2id_produces_correct_key_length() {
        let key = derive_key(
            b"pass",
            &[1u8; 8],
            HashAlgorithm::Sha256,
            KdfAlgorithm::Argon2id,
            1,
        )
        .unwrap();
        assert_eq!(key.len(), 32);
    }

    // -----------------------------------------------------------------------
    // Encrypt / decrypt roundtrips
    // -----------------------------------------------------------------------

    fn roundtrip(
        plaintext: &[u8],
        cipher: CipherAlgorithm,
        kdf: KdfAlgorithm,
        hash: HashAlgorithm,
        iters: u32,
        context: &str,
    ) {
        let enc_opts = EncryptOptions {
            cipher,
            kdf,
            hash,
            iterations: Some(iters),
            siv_mode: SivMode::LocalDeterministic {
                context: context.to_owned(),
            },
        };
        let ct = encrypt(plaintext, "strong_pass", &enc_opts).unwrap();

        let dec_opts = DecryptOptions {
            cipher,
            kdf,
            hash,
            iterations: Some(iters),
            verify_context: Some(context.to_owned()),
        };
        let pt = decrypt(&ct, "strong_pass", &dec_opts).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn roundtrip_aes256gcm_pbkdf2_sha256() {
        roundtrip(
            b"Secret message",
            CipherAlgorithm::Aes256Gcm,
            KdfAlgorithm::Pbkdf2,
            HashAlgorithm::Sha256,
            100,
            "docs/secret.txt",
        );
    }

    #[test]
    fn roundtrip_chacha20poly1305_argon2id_blake2b() {
        roundtrip(
            b"Another secret",
            CipherAlgorithm::ChaCha20Poly1305,
            KdfAlgorithm::Argon2id,
            HashAlgorithm::Blake2b,
            1,
            "data/file.bin",
        );
    }

    #[test]
    fn roundtrip_empty_plaintext() {
        roundtrip(
            b"",
            CipherAlgorithm::Aes256Gcm,
            KdfAlgorithm::Pbkdf2,
            HashAlgorithm::Sha256,
            10,
            "empty.txt",
        );
    }

    #[test]
    fn random_mode_roundtrip() {
        let enc_opts = EncryptOptions {
            siv_mode: SivMode::Random,
            iterations: Some(10),
            ..Default::default()
        };
        let ct = encrypt(b"hello random", "pass", &enc_opts).unwrap();
        let dec_opts = DecryptOptions {
            iterations: Some(10),
            ..Default::default()
        };
        let pt = decrypt(&ct, "pass", &dec_opts).unwrap();
        assert_eq!(pt, b"hello random");
    }

    #[test]
    fn deterministic_mode_is_idempotent() {
        let opts = EncryptOptions {
            iterations: Some(10),
            siv_mode: SivMode::LocalDeterministic {
                context: "file.txt".to_owned(),
            },
            ..Default::default()
        };
        let ct1 = encrypt(b"same content", "pass", &opts).unwrap();
        let ct2 = encrypt(b"same content", "pass", &opts).unwrap();
        assert_eq!(ct1, ct2);
    }

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

    #[test]
    fn wrong_password_fails_with_auth_error() {
        let enc_opts = EncryptOptions {
            iterations: Some(10),
            siv_mode: SivMode::LocalDeterministic {
                context: "f.txt".to_owned(),
            },
            ..Default::default()
        };
        let ct = encrypt(b"secret", "correct", &enc_opts).unwrap();
        let dec_opts = DecryptOptions {
            iterations: Some(10),
            verify_context: Some("f.txt".to_owned()),
            ..Default::default()
        };
        let err = decrypt(&ct, "wrong", &dec_opts).unwrap_err();
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn wrong_context_fails_authentication() {
        let enc_opts = EncryptOptions {
            iterations: Some(10),
            siv_mode: SivMode::LocalDeterministic {
                context: "file1.txt".to_owned(),
            },
            ..Default::default()
        };
        let ct = encrypt(b"data", "pass", &enc_opts).unwrap();
        let dec_opts = DecryptOptions {
            iterations: Some(10),
            verify_context: Some("file2.txt".to_owned()), // wrong context → wrong AAD
            ..Default::default()
        };
        let err = decrypt(&ct, "pass", &dec_opts).unwrap_err();
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let enc_opts = EncryptOptions {
            iterations: Some(10),
            siv_mode: SivMode::LocalDeterministic {
                context: "f.txt".to_owned(),
            },
            ..Default::default()
        };
        let ct_b64 = encrypt(b"important data", "pass", &enc_opts).unwrap();

        // Decode, flip a byte in the ciphertext body, re-encode
        let mut raw = BASE64.decode(&ct_b64).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = BASE64.encode(&raw);

        let dec_opts = DecryptOptions {
            iterations: Some(10),
            verify_context: Some("f.txt".to_owned()),
            ..Default::default()
        };
        let err = decrypt(&tampered, "pass", &dec_opts).unwrap_err();
        assert!(matches!(err, CryptoError::AuthenticationFailed));
    }

    #[test]
    fn missing_salt_header_returns_error() {
        let garbage = BASE64.encode(b"WrongHeader12345678IVed__012345678901ciphertext");
        let dec_opts = DecryptOptions {
            iterations: Some(10),
            ..Default::default()
        };
        let err = decrypt(&garbage, "pass", &dec_opts).unwrap_err();
        assert!(matches!(err, CryptoError::MissingSaltHeader));
    }

    #[test]
    fn missing_iv_header_returns_error() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"Salted__");
        raw.extend_from_slice(b"12345678"); // salt
        raw.extend_from_slice(b"BadIVHdr"); // wrong IV header
        let encoded = BASE64.encode(&raw);
        let dec_opts = DecryptOptions {
            iterations: Some(10),
            ..Default::default()
        };
        let err = decrypt(&encoded, "pass", &dec_opts).unwrap_err();
        assert!(matches!(err, CryptoError::MissingIVHeader));
    }

    // -----------------------------------------------------------------------
    // Cross-implementation compatibility
    // -----------------------------------------------------------------------

    // This base64 was produced by the original Python `transcrypt`
    // (workflow/core/crypto.py), not by this code. It is the anchor that proves
    // we still speak the exact same packet format: salt/IV framing, the SIV
    // derivation string, and the AEAD parameters. If any of those drift, one of
    // the two tests below fails — and a silent drift would mean every existing
    // encrypted repository becomes unreadable. AES-256-GCM / PBKDF2 / SHA-256,
    // deterministic, 1000 iterations, context "docs/note.txt".
    const REFERENCE_VECTOR: &str =
        "U2FsdGVkX19eEmo+67ljNUlWZWRfX6BVVr6P6FZp1v1RY6ZPe7lRhl2X2XJSyQVUbJnvvA7+VrppiD0+mFsmSxZEOQ==";
    const REFERENCE_PLAINTEXT: &[u8] = b"hello transcrypt\n";
    const REFERENCE_PASSWORD: &str = "test-passphrase";
    const REFERENCE_CONTEXT: &str = "docs/note.txt";

    fn reference_encrypt_options() -> EncryptOptions {
        EncryptOptions {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Sha256,
            iterations: Some(1000),
            siv_mode: SivMode::LocalDeterministic {
                context: REFERENCE_CONTEXT.to_owned(),
            },
        }
    }

    #[test]
    fn decrypts_vector_from_the_reference_implementation() {
        let opts = DecryptOptions {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Sha256,
            iterations: Some(1000),
            verify_context: Some(REFERENCE_CONTEXT.to_owned()),
        };
        let plaintext = decrypt(REFERENCE_VECTOR, REFERENCE_PASSWORD, &opts).unwrap();
        assert_eq!(plaintext, REFERENCE_PLAINTEXT);
    }

    #[test]
    fn re_encrypting_reproduces_the_reference_vector_byte_for_byte() {
        let ciphertext = encrypt(
            REFERENCE_PLAINTEXT,
            REFERENCE_PASSWORD,
            &reference_encrypt_options(),
        )
        .unwrap();
        assert_eq!(ciphertext, REFERENCE_VECTOR);
    }

    // -----------------------------------------------------------------------
    // Error contracts
    // -----------------------------------------------------------------------

    #[test]
    fn rejects_input_that_is_not_base64() {
        let err = decrypt("this is not base64!", "pass", &DecryptOptions::default()).unwrap_err();
        assert!(matches!(err, CryptoError::Base64(_)));
    }

    #[test]
    fn pbkdf2_paired_with_blake2_is_refused() {
        // BLAKE2 is fine for SIV hashing but can't drive PBKDF2; the failure
        // must be explicit rather than a silent substitution to another hash.
        let opts = EncryptOptions {
            cipher: CipherAlgorithm::Aes256Gcm,
            kdf: KdfAlgorithm::Pbkdf2,
            hash: HashAlgorithm::Blake2b,
            iterations: Some(10),
            siv_mode: SivMode::LocalDeterministic {
                context: "f.txt".to_owned(),
            },
        };
        let err = encrypt(b"data", "pass", &opts).unwrap_err();
        assert!(matches!(err, CryptoError::UnsupportedHash(_)));
    }

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
