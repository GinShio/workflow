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
//! and a working `git`, rather than a wedged repository. A *wrong* password is
//! a different story and fails loudly, because quietly writing garbage to disk
//! would be worse than stopping.
//!
//! These subcommands are meant to be invoked by git, not by hand. Configure
//! them in `.git/config` and `.gitattributes`; see `docs/transcrypt.md`.

use std::io::{Read, Write};

use anyhow::Context;
use clap::Args;

use wits_util::config::Resolver;
use wits_util::crypto::{
    CipherAlgorithm, DecryptOptions, EncryptOptions, HashAlgorithm, KdfAlgorithm, SivMode,
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
        TranscryptAction::Clean { file } => clean(&resolver, file.as_deref()),
        TranscryptAction::Smudge { file } => smudge(&resolver, file.as_deref()),
        TranscryptAction::Textconv { file } => textconv(&resolver, file),
    }
}

fn status(resolver: &Resolver<'_>, context: &str) -> anyhow::Result<()> {
    println!("wits transcrypt status (context: {context})\n");
    for key in ["password", "cipher", "digest", "kdf", "iterations"] {
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

/// The encryption settings, gathered from the resolver. The password is the
/// only one that's mandatory; the rest carry the historical defaults so an
/// unconfigured repo still round-trips.
fn resolve_crypto_config(
    resolver: &Resolver<'_>,
) -> anyhow::Result<(String, CipherAlgorithm, HashAlgorithm, KdfAlgorithm, u32)> {
    let password = resolver.get("password").map(|rv| rv.value).ok_or_else(|| {
        anyhow::anyhow!(
            "no password configured; set WITS_TRANSCRYPT_PASSWORD or git config wits.transcrypt.password"
        )
    })?;

    // A misconfigured algorithm must fail loudly, not silently fall back to the
    // default: encrypting under a different cipher than intended is exactly the
    // kind of quiet wrong-key hazard this tool exists to avoid. An *unset* key
    // still round-trips, because `get_or_default` hands back the historical
    // default string, which parses cleanly.
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
    // Iterations are numeric, and unset means "use the KDF's default". Only a
    // *non-empty, unparseable* value is an error.
    let iterations = {
        let raw = resolver.get_or_default("iterations", "").value;
        if raw.is_empty() {
            kdf.default_iterations()
        } else {
            raw.parse().context("transcrypt.iterations")?
        }
    };

    Ok((password, cipher, hash, kdf, iterations))
}

fn clean(resolver: &Resolver<'_>, file: Option<&str>) -> anyhow::Result<()> {
    let mut plaintext = Vec::new();
    std::io::stdin().read_to_end(&mut plaintext)?;

    // An empty file has nothing to protect; leaving it empty avoids turning it
    // into a ciphertext blob and keeps us byte-compatible with the reference.
    if plaintext.is_empty() {
        return Ok(());
    }

    let (password, cipher, hash, kdf, iterations) = resolve_crypto_config(resolver)?;
    let opts = EncryptOptions {
        cipher,
        kdf,
        hash,
        iterations: Some(iterations),
        siv_mode: SivMode::LocalDeterministic {
            context: file.unwrap_or("").to_owned(),
        },
    };
    let ciphertext = wits_util::crypto::encrypt(&plaintext, &password, &opts)?;
    std::io::stdout().write_all(ciphertext.as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
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
/// can't read. The one case we *do* fail on is a genuine packet under the wrong
/// password — silently writing garbage there would be worse than stopping.
fn recover(resolver: &Resolver<'_>, file: Option<&str>, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    // Plaintext, binary, anything that isn't our packet: hand it back untouched.
    // This is what lets `git diff` work on a not-yet-encrypted or binary file.
    if !wits_util::crypto::is_encrypted(data) {
        return Ok(data.to_vec());
    }

    // It is a packet, but no configured password is the benign "checkout without
    // the key" case — leave it encrypted rather than failing the checkout.
    let Ok((password, cipher, hash, kdf, iterations)) = resolve_crypto_config(resolver) else {
        return Ok(data.to_vec());
    };

    let opts = DecryptOptions {
        cipher,
        kdf,
        hash,
        iterations: Some(iterations),
        verify_context: Some(file.unwrap_or("").to_owned()),
    };

    // `is_encrypted` already proved the payload is valid base64, hence ASCII.
    let ciphertext = std::str::from_utf8(data).unwrap_or_default().trim();
    match wits_util::crypto::decrypt(ciphertext, &password, &opts) {
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
