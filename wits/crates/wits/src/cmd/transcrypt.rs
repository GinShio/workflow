//! `wits transcrypt` — transparent file encryption, wired into git's filter system.
//!
//! Git lets a repository declare, per path, a *clean* filter (run on the way
//! into the index) and a *smudge* filter (run on the way out to the working
//! tree). Pointing both at this command turns committing a secret into storing
//! ciphertext and checking it out into recovering plaintext, with nothing in
//! the normal `git add` / `git checkout` workflow to remember. `textconv` is
//! the same idea for `git diff`, so diffs read as plaintext.
//!
//! The one design decision worth understanding is what happens to a teammate
//! who clones the repo without the password. The naive thing — error out — would
//! break their checkout entirely. Instead `smudge` degrades to passing the
//! encrypted bytes through untouched: they get an unreadable-but-harmless blob
//! and a working `git`, rather than a wedged repository. A packet that the
//! configured password does not open is a different story and fails loudly: the
//! key is wrong, and a checkout that quietly kept the ciphertext would hide it.
//!
//! Sealing reads the index. A context's default salt mode seals every content
//! version under a fresh random salt, so re-cleaning an unchanged file would
//! give git a new blob each time; clean hands back the packet the index already
//! holds instead, whenever that packet seals this same content.
//!
//! These subcommands are meant to be invoked by git, not by hand. Configure
//! them in `.git/config` and `.gitattributes`; see
//! `docs/commands/transcrypt.rst`.

use std::io::{Read, Write};

use anyhow::{anyhow, bail, Context};
use clap::Args;

use wits_util::config::Resolver;
use wits_util::crypto::{
    self, CipherAlgorithm, DecryptOptions, HashAlgorithm, KdfAlgorithm, KdfParams, SaltMode,
    SealOptions, Sealed, SALT_SIZE,
};
use wits_util::git::Repository;

#[derive(Debug, Args)]
pub struct TranscryptArgs {
    /// Selects one of several independent secret sets in the same repository.
    #[arg(short = 'C', long, value_name = "CONTEXT", default_value = "default")]
    pub context: String,

    #[command(subcommand)]
    pub action: TranscryptAction,
}

#[derive(Debug, clap::Subcommand)]
pub enum TranscryptAction {
    /// Report the resolved configuration and where each value came from.
    Status,
    /// Clean filter: plaintext on stdin, base64 ciphertext on stdout.
    Clean {
        #[arg(value_name = "FILE")]
        file: Option<String>,
    },
    /// Smudge filter: base64 ciphertext on stdin, plaintext on stdout.
    Smudge {
        #[arg(value_name = "FILE")]
        file: Option<String>,
    },
    /// textconv: decrypt a file on disk to stdout for `git diff`.
    Textconv {
        #[arg(value_name = "FILE")]
        file: String,
    },
}

pub fn run(args: &TranscryptArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repository::new(&cwd);
    let resolver = Resolver::new(Some(&repo), "wits.transcrypt", Some(args.context.as_str()));

    match &args.action {
        TranscryptAction::Status => status(&resolver, &args.context),
        TranscryptAction::Clean { file } => clean(&resolver, &repo, file.as_deref()),
        TranscryptAction::Smudge { file } => smudge(&resolver, file.as_deref()),
        TranscryptAction::Textconv { file } => textconv(&resolver, file),
    }
}

fn status(resolver: &Resolver<'_>, context: &str) -> anyhow::Result<()> {
    println!("wits transcrypt status (context: {context})\n");
    for key in [
        "password",
        "cipher",
        "digest",
        "kdf",
        "iterations",
        "memory",
        "lanes",
        "salt",
        "salt-mode",
    ] {
        match resolver.get(key) {
            Some(rv) => {
                let shown = if key == "password" { "***" } else { &rv.value };
                println!("  {key:<12} = {shown:<20}  [{}]", rv.source);
            }
            None => println!("  {key:<12} = (not set)"),
        }
    }
    Ok(())
}

/// The password and how it becomes a key: everything opening a packet needs.
struct Secret {
    password: String,
    cipher: CipherAlgorithm,
    kdf: KdfParams,
}

/// The configured secret, or `None` when no password is configured — the one
/// state smudge treats as benign.
///
/// A misconfigured algorithm or parameter is an error, never a fallback to the
/// default: encrypting under other settings than intended is exactly the kind
/// of quiet wrong-key hazard this tool exists to avoid. An *unset* one takes the
/// default every packet up to now was sealed with.
fn resolve_secret(resolver: &Resolver<'_>) -> anyhow::Result<Option<Secret>> {
    let Some(password) = resolver.get("password").map(|rv| rv.value) else {
        return Ok(None);
    };
    let cipher: CipherAlgorithm = resolver
        .get_or_default("cipher", "aes-256-gcm")
        .value
        .parse()
        .context("transcrypt.cipher")?;
    let hash: HashAlgorithm = resolver
        .get_or_default("digest", "sha256")
        .value
        .parse()
        .context("transcrypt.digest")?;
    let kdf: KdfAlgorithm = resolver
        .get_or_default("kdf", "pbkdf2")
        .value
        .parse()
        .context("transcrypt.kdf")?;

    let mut params = KdfParams::defaults(kdf, hash);
    if let Some(iterations) = number(resolver, "iterations")? {
        params.iterations = iterations;
    }
    if let Some(mib) = number(resolver, "memory")? {
        params.memory_kib = mib
            .checked_mul(1024)
            .ok_or_else(|| anyhow!("transcrypt.memory: {mib} MiB is out of range"))?;
    }
    if let Some(lanes) = number(resolver, "lanes")? {
        params.lanes = lanes;
    }

    Ok(Some(Secret {
        password,
        cipher,
        kdf: params,
    }))
}

/// A numeric setting: `None` when it is unset or empty, an error when it is
/// set to something that is not a number.
fn number(resolver: &Resolver<'_>, key: &str) -> anyhow::Result<Option<u32>> {
    let raw = resolver.get_or_default(key, "").value;
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse()
        .map(Some)
        .with_context(|| format!("transcrypt.{key}"))
}

/// What sealing needs beyond the secret.
///
/// The context salt has no default. One derived from the password would put the
/// password back into the packet without the KDF, and a fixed one would let a
/// dictionary run against it once serve every rotation that follows.
fn resolve_seal_options(resolver: &Resolver<'_>, secret: &Secret) -> anyhow::Result<SealOptions> {
    let salt = resolver.get("salt").map(|rv| rv.value).ok_or_else(|| {
        anyhow!(
            "no context salt configured; set {} to 16 hex digits, for example from \
             `openssl rand -hex 8`",
            resolver.where_to_set("salt")
        )
    })?;
    let context_salt = parse_salt(&salt).context("transcrypt.salt")?;
    let mode: SaltMode = resolver
        .get_or_default("salt-mode", SaltMode::default().as_str())
        .value
        .parse()
        .context("transcrypt.salt-mode")?;
    Ok(SealOptions {
        cipher: secret.cipher,
        kdf: secret.kdf,
        mode,
        context_salt,
    })
}

fn parse_salt(hex: &str) -> anyhow::Result<[u8; SALT_SIZE]> {
    let hex = hex.trim();
    if hex.len() != 2 * SALT_SIZE || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected {} hex digits, got {hex:?}", 2 * SALT_SIZE);
    }
    let mut salt = [0u8; SALT_SIZE];
    for (i, byte) in salt.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)?;
    }
    Ok(salt)
}

fn clean(resolver: &Resolver<'_>, repo: &Repository, file: Option<&str>) -> anyhow::Result<()> {
    let mut plaintext = Vec::new();
    std::io::stdin().read_to_end(&mut plaintext)?;

    // An empty file has nothing to protect; leaving it empty avoids turning it
    // into a ciphertext blob.
    if plaintext.is_empty() {
        return Ok(());
    }

    let secret = resolve_secret(resolver)?.ok_or_else(|| {
        anyhow!(
            "no password configured; set {}",
            resolver.where_to_set("password")
        )
    })?;
    let options = resolve_seal_options(resolver, &secret)?;
    let previous = file.and_then(|path| repo.index_blob(path));

    let mut stdout = std::io::stdout().lock();
    match crypto::seal(
        &plaintext,
        &secret.password,
        file.unwrap_or(""),
        previous.as_deref(),
        &options,
    )? {
        Sealed::Unchanged => stdout.write_all(
            previous
                .as_deref()
                .expect("seal answers Unchanged only for a previous packet"),
        )?,
        Sealed::Packet(packet) => {
            stdout.write_all(packet.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn smudge(resolver: &Resolver<'_>, file: Option<&str>) -> anyhow::Result<()> {
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;
    let recovered = recover(resolver, file, &data)?;
    std::io::stdout().write_all(&recovered)?;
    Ok(())
}

fn textconv(resolver: &Resolver<'_>, file: &str) -> anyhow::Result<()> {
    let data = std::fs::read(file)?;
    let recovered = recover(resolver, Some(file), &data)?;
    std::io::stdout().write_all(&recovered)?;
    Ok(())
}

/// Turn stored bytes back into plaintext for smudge and textconv, which share
/// exactly this logic. The guiding rule: never abort git over content we simply
/// can't read. The one case we *do* fail on is a genuine packet the configured
/// password does not open — passing it through would hide a wrong key.
fn recover(resolver: &Resolver<'_>, file: Option<&str>, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    // Plaintext, binary, anything that isn't our packet: hand it back untouched.
    // This is what lets `git diff` work on a not-yet-encrypted or binary file.
    if !crypto::is_encrypted(data) {
        return Ok(data.to_vec());
    }

    // It is a packet, but no configured password is the benign "checkout without
    // the key" case — leave it encrypted rather than failing the checkout.
    let Some(secret) = resolve_secret(resolver)? else {
        return Ok(data.to_vec());
    };

    let opts = DecryptOptions {
        cipher: secret.cipher,
        kdf: secret.kdf,
        path: Some(file.unwrap_or("").to_owned()),
    };

    // `is_encrypted` already proved the payload is valid base64, hence ASCII.
    let ciphertext = std::str::from_utf8(data).unwrap_or_default().trim();
    match crypto::decrypt(ciphertext, &secret.password, &opts) {
        Ok(plaintext) => Ok(plaintext),
        Err(e) => {
            if std::env::var("WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK").is_ok() {
                log::warn!("decryption failed ({e}); passing raw content through");
                Ok(data.to_vec())
            } else {
                Err(e.into())
            }
        }
    }
}
