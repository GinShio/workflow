//! Black-box tests for the `wits transcrypt` git-filter entry points.
//!
//! These spawn the real binary and drive it the way git does — bytes in on
//! stdin, bytes out on stdout — because what matters here is the contract with
//! git, not any single internal function. The behaviour worth pinning is the
//! clean/smudge round-trip and the deliberate split between a *missing*
//! password (degrade quietly so a checkout still works) and a *wrong* one (fail
//! loudly rather than write garbage to the working tree).

use std::io::Write;
use std::process::{Command, Stdio};

struct Run {
    success: bool,
    stdout: Vec<u8>,
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
    }
}

const PASSWORD: (&str, &str) = ("WITS_TRANSCRYPT_PASSWORD", "test-passphrase");

#[test]
fn clean_then_smudge_round_trips() {
    let plaintext: &[u8] = b"top secret value\n";

    let cleaned = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD],
        &[],
        plaintext,
    );
    assert!(cleaned.success);
    assert_ne!(
        cleaned.stdout.as_slice(),
        plaintext,
        "clean should have encrypted"
    );

    let smudged = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[PASSWORD],
        &[],
        &cleaned.stdout,
    );
    assert!(smudged.success);
    assert_eq!(smudged.stdout.as_slice(), plaintext);
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
    let cleaned = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD],
        &[],
        b"data\n",
    );
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[("WITS_TRANSCRYPT_PASSWORD", "the-wrong-one")],
        &[],
        &cleaned.stdout,
    );
    assert!(!out.success);
}

#[test]
fn the_escape_hatch_forces_a_wrong_password_through() {
    let cleaned = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD],
        &[],
        b"data\n",
    );
    let out = run(
        &["transcrypt", "smudge", "secret.txt"],
        &[
            ("WITS_TRANSCRYPT_PASSWORD", "the-wrong-one"),
            ("WITS_TRANSCRYPT_ALLOW_RAW_FALLBACK", "1"),
        ],
        &[],
        &cleaned.stdout,
    );
    assert!(out.success);
    assert_eq!(
        out.stdout, cleaned.stdout,
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
    let cleaned = run(
        &["transcrypt", "clean", "secret.txt"],
        &[PASSWORD],
        &[],
        plaintext,
    );
    assert!(cleaned.success);

    let out = run_textconv("secret.txt", &cleaned.stdout, &[PASSWORD]);
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
