//! Black-box tests for the `wits transcrypt` git-filter entry points.
//!
//! These spawn the real binary and drive it the way git does — bytes in on
//! stdin, bytes out on stdout — because what matters here is the contract with
//! git, not any single internal function. The behaviour worth pinning is the
//! clean/smudge round-trip, the deliberate split between a *missing* password
//! (degrade quietly so a checkout still works) and a *wrong* one (fail loudly
//! rather than hide a wrong key), and — in a real repository — that re-cleaning
//! unchanged content hands git the blob it already holds.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

struct Run {
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run the binary in a throwaway directory with git's global/system config
/// pointed at nothing, so the only configuration in play is what we set here.
fn run(args: &[&str], env: &[(&str, &str)], unset: &[&str], stdin: &[u8]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wits"));
    cmd.args(args)
        .current_dir(dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in unset {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let output = child.wait_with_output().unwrap();
    Run {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Like [`run`], but for `textconv`, which reads a file off disk instead of
/// stdin: write `contents` into a throwaway dir and point the command at it.
fn run_textconv(file_name: &str, contents: &[u8], env: &[(&str, &str)]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(file_name), contents).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wits"));
    cmd.args(["transcrypt", "textconv", file_name])
        .current_dir(dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    Run {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

const PASSWORD: (&str, &str) = ("WITS_TRANSCRYPT_PASSWORD", "test-passphrase");
const SALT: (&str, &str) = ("WITS_TRANSCRYPT_SALT", "0123456789abcdef");

fn sealed(plaintext: &[u8]) -> Vec<u8> {
    let cleaned = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD, SALT],
        &[],
        plaintext,
    );
    assert!(cleaned.success, "{}", cleaned.stderr);
    cleaned.stdout
}

#[test]
fn clean_then_smudge_round_trips() {
    let plaintext: &[u8] = b"top secret value\n";
    let cleaned = sealed(plaintext);
    assert_ne!(cleaned.as_slice(), plaintext, "clean should have encrypted");

    let smudged = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[PASSWORD],
        &[],
        &cleaned,
    );
    assert!(smudged.success);
    assert_eq!(smudged.stdout.as_slice(), plaintext);
}

#[test]
fn clean_without_a_context_salt_fails_naming_the_key() {
    let out = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD],
        &["WITS_TRANSCRYPT_SALT"],
        b"data\n",
    );
    assert!(!out.success);
    assert!(
        out.stderr.contains("WITS_TRANSCRYPT_SALT"),
        "{}",
        out.stderr
    );
}

#[test]
fn clean_passes_a_packet_it_opens_through_unchanged() {
    // A working tree checked out before the key was set holds the ciphertext
    // itself; adding it again must store it as it is, not seal it twice.
    let cleaned = sealed(b"already sealed\n");
    let again = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD, SALT],
        &[],
        &cleaned,
    );
    assert!(again.success, "{}", again.stderr);
    assert_eq!(again.stdout, cleaned);
}

#[test]
fn clean_refuses_a_packet_it_cannot_open() {
    let cleaned = sealed(b"data\n");
    let under_another_password = run(
        &["transcrypt", "clean", "secret.txt"],
        &[("WITS_TRANSCRYPT_PASSWORD", "the-wrong-one"), SALT],
        &[],
        &cleaned,
    );
    assert!(!under_another_password.success);
    let at_another_path = run(
        &["transcrypt", "clean", "elsewhere.txt"],
        &[PASSWORD, SALT],
        &[],
        &cleaned,
    );
    assert!(!at_another_path.success);
    assert!(
        at_another_path.stderr.contains("packet inside a packet"),
        "{}",
        at_another_path.stderr
    );
}

#[test]
fn smudge_without_a_password_passes_bytes_through() {
    let blob: &[u8] = b"not-really-ciphertext but still checked out\n";
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[],
        &["WITS_TRANSCRYPT_PASSWORD"],
        blob,
    );
    assert!(out.success);
    assert_eq!(out.stdout.as_slice(), blob);
}

#[test]
fn smudge_with_the_wrong_password_fails_loudly() {
    let cleaned = sealed(b"data\n");
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[("WITS_TRANSCRYPT_PASSWORD", "the-wrong-one")],
        &[],
        &cleaned,
    );
    assert!(!out.success);
}

#[test]
fn smudge_with_a_malformed_setting_fails_rather_than_passing_through() {
    // Only a *missing* password is the benign case; a setting that is there
    // but wrong must not quietly leave the ciphertext in the working tree.
    let cleaned = sealed(b"data\n");
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[PASSWORD, ("WITS_TRANSCRYPT_CIPHER", "rot13")],
        &[],
        &cleaned,
    );
    assert!(!out.success);
}

#[test]
fn the_escape_hatch_forces_a_wrong_password_through() {
    let cleaned = sealed(b"data\n");
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[
            ("WITS_TRANSCRYPT_PASSWORD", "the-wrong-one"),
            ("WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK", "1"),
        ],
        &[],
        &cleaned,
    );
    assert!(out.success);
    assert_eq!(
        out.stdout, cleaned,
        "raw ciphertext should pass through unchanged"
    );
}

// A file matched by .gitattributes isn't necessarily encrypted — it may be
// plaintext committed before the filter, or binary. None of these should abort
// git; they pass through so `git diff` / checkout still work.

#[test]
fn textconv_passes_plaintext_through() {
    let contents: &[u8] = b"# a config comment\nkey = value\n";
    let out = run_textconv("config.ini", contents, &[]);
    assert!(out.success);
    assert_eq!(out.stdout.as_slice(), contents);
}

#[test]
fn textconv_passes_binary_through() {
    let contents: &[u8] = &[0x00, 0xff, 0x80, 0x42, 0xfe];
    let out = run_textconv("blob.bin", contents, &[]);
    assert!(out.success);
    assert_eq!(out.stdout.as_slice(), contents);
}

#[test]
fn textconv_decrypts_an_encrypted_file() {
    let plaintext: &[u8] = b"diff me please\n";
    let cleaned = sealed(plaintext);
    let out = run_textconv("secret.txt", &cleaned, &[PASSWORD]);
    assert!(out.success);
    assert_eq!(out.stdout.as_slice(), plaintext);
}

#[test]
fn smudge_passes_binary_non_packet_through() {
    // Even with a password set, content that isn't a packet (here: non-UTF-8
    // bytes) is handed back as-is rather than crashing on the read.
    let blob: &[u8] = &[0x00, 0x01, 0xff, 0xfe, 0x80, b'\n'];
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[PASSWORD],
        &[],
        blob,
    );
    assert!(out.success);
    assert_eq!(out.stdout.as_slice(), blob);
}

// ---------------------------------------------------------------------------
// In a real repository
// ---------------------------------------------------------------------------

/// A git repository whose `secret/` the binary under test seals, configured
/// through the repository alone: git's global and system config are pointed at
/// nothing and any `WITS_TRANSCRYPT_*` the caller exported is dropped, so the
/// filters git spawns see exactly this setup.
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let bin = env!("CARGO_BIN_EXE_wits");
        repo.git(&["init", "-q"]);
        for (key, value) in [
            (
                "filter.transcrypt.clean",
                format!("'{bin}' transcrypt clean %f"),
            ),
            (
                "filter.transcrypt.smudge",
                format!("'{bin}' transcrypt smudge %f"),
            ),
            ("filter.transcrypt.required", "true".to_owned()),
            ("wits.transcrypt.password", "test-passphrase".to_owned()),
            ("wits.transcrypt.salt", "0123456789abcdef".to_owned()),
        ] {
            repo.git(&["config", key, &value]);
        }
        repo.write(".gitattributes", "secret/** filter=transcrypt\n");
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        for (key, _) in std::env::vars() {
            if key.starts_with("WITS_TRANSCRYPT_") {
                cmd.env_remove(key);
            }
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// The blob the index holds for `path`.
    fn staged_blob(&self, path: &str) -> String {
        self.git(&["rev-parse", &format!(":{path}")])
            .trim()
            .to_owned()
    }
}

#[test]
fn readding_unchanged_content_keeps_the_blob() {
    let repo = Repo::new();
    repo.write("secret/a.txt", "v1\n");
    repo.git(&["add", "secret/a.txt"]);
    let blob = repo.staged_blob("secret/a.txt");
    assert!(
        repo.git(&["cat-file", "-p", &blob]).starts_with("U2FsdGVk"),
        "staged as a packet"
    );

    repo.git(&["add", "--renormalize", "secret/a.txt"]);
    assert_eq!(
        repo.staged_blob("secret/a.txt"),
        blob,
        "a file-mode re-clean hands back the index's packet"
    );

    repo.git(&["config", "wits.transcrypt.salt-mode", "context"]);
    repo.git(&["add", "--renormalize", "secret/a.txt"]);
    assert_eq!(
        repo.staged_blob("secret/a.txt"),
        blob,
        "switching mode re-seals nothing unchanged"
    );
}

#[test]
fn a_changed_file_gets_a_new_packet_that_opens() {
    let repo = Repo::new();
    repo.write("secret/a.txt", "v1\n");
    repo.git(&["add", "secret/a.txt"]);
    let first = repo.staged_blob("secret/a.txt");

    repo.write("secret/a.txt", "v2\n");
    repo.git(&["add", "secret/a.txt"]);
    assert_ne!(repo.staged_blob("secret/a.txt"), first);
    assert_eq!(
        repo.git(&["cat-file", "--filters", ":secret/a.txt"]),
        "v2\n"
    );
}
