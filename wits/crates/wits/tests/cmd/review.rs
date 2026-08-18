//! Black-box tests for `wits review`'s local pipeline.
//!
//! The network verbs (`fetch`, `submit`) talk to a live forge, which a unit test
//! can't stand up; but everything *between* them is local — the three-file
//! store, the hand-edited `local.json` draft, the merged `--json` view. So these
//! drive the real binary against a throwaway git repo with a hand-seeded store
//! (simulating a completed `fetch`), author by writing `local.json` (the way an
//! editor or a human does), and pin the read/preview path plus a
//! `submit --dry-run` that plans without touching the network.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    store: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&store).unwrap();

        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-b", "main"]);
        git(&["remote", "add", "origin", "git@github.com:me/proj.git"]);

        Fixture {
            _dir: dir,
            repo,
            store,
        }
    }

    fn mr_dir(&self, id: &str) -> PathBuf {
        self.store.join("github.com/me/proj").join(id)
    }

    /// Seed a completed fetch: `info.json` (with one changed file and a reviewed
    /// snapshot) and `comments.json` (one remote thread).
    fn seed(&self, id: &str, head_sha: &str) {
        let dir = self.mr_dir(id);
        std::fs::create_dir_all(&dir).unwrap();

        let info = INFO.replace("__ID__", id).replace("__HEAD__", head_sha);
        std::fs::write(dir.join("info.json"), info).unwrap();
        std::fs::write(dir.join("comments.json"), COMMENTS).unwrap();
    }

    /// Author by writing the draft file, exactly as an editor/human would.
    fn write_local(&self, id: &str, json: &str) {
        let dir = self.mr_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local.json"), json).unwrap();
    }

    fn local_exists(&self, id: &str) -> bool {
        self.mr_dir(id).join("local.json").exists()
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_with(args, None)
    }

    fn run_stdin(&self, args: &[&str], stdin: &str) -> Out {
        self.run_with(args, Some(stdin))
    }

    fn run_with(&self, args: &[&str], stdin: Option<&str>) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wits"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("WITS_REVIEW_DIR", &self.store)
            .env("GITHUB_TOKEN", "x") // lets a dry-run submit resolve the forge
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

const INFO: &str = r##"{
  "schema": 1,
  "mr": { "id": "__ID__", "display": "#__ID__", "state": "open", "draft": false,
          "title": "Add a thing", "author": "alice", "base": "main",
          "source": "feature-__ID__", "head_sha": "__HEAD__",
          "updated_at": "2026-07-01T00:00:00Z", "labels": [],
          "web_url": "https://github.com/me/proj/pull/__ID__" },
  "snapshots": [ { "base_sha": "base000", "start_sha": "base000",
                   "head_sha": "__HEAD__" } ],
  "fetched_at": 1700000000,
  "commits": [ { "sha": "__HEAD__", "subject": "Add a thing" } ],
  "files": [ { "path": "src/x.c", "status": "M" } ]
}"##;

const COMMENTS: &str = r##"{
  "schema": 1,
  "threads": [ {
    "id": "remote:9987", "origin": "remote", "resolved": false, "outdated": true,
    "anchor": { "kind": "line", "path": "src/x.c", "end": { "line": 5, "side": "new" } },
    "comments": [ { "id": "remote:5", "author": "bob", "origin": "remote",
                    "body": "nit here", "created_at": "2026-07-01T00:00:00Z",
                    "state": "published" } ]
  } ]
}"##;

#[test]
fn show_reflects_the_seeded_remote_thread() {
    let fx = Fixture::new();
    fx.seed("1", "head111");

    let out = fx.run(&["review", "show", "1", "--json"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("\"head_sha\": \"head111\""));
    assert!(out.stdout.contains("\"remote:9987\""));
    assert!(out.stdout.contains("nit here"));
}

#[test]
fn inbox_lists_fetched_mrs() {
    let fx = Fixture::new();
    fx.seed("1", "head111");
    fx.seed("2", "head222");

    let out = fx.run(&["review", "show", "--json"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("\"id\": \"1\""));
    assert!(out.stdout.contains("\"id\": \"2\""));
}

#[test]
fn a_hand_written_draft_merges_into_the_view() {
    let fx = Fixture::new();
    fx.seed("1", "head111");
    fx.write_local(
        "1",
        r#"{ "schema": 1, "verdict": "request-changes",
             "actions": [
               { "action": "comment", "file": "src/x.c", "line": 50, "body": "looks off" },
               { "action": "reply", "thread": "9987", "body": "agreed" }
             ] }"#,
    );

    // `draft` echoes it back.
    let d = fx.run(&["review", "draft", "1", "--json"]);
    assert!(d.success, "stderr: {}", d.stderr);
    assert!(d.stdout.contains("request-changes"));
    assert!(d.stdout.contains("looks off"));

    // `show` folds the draft into the thread view: a new local thread, and the
    // reply attached to the remote thread.
    let s = fx.run(&["review", "show", "1", "--json"]);
    assert!(s.success, "stderr: {}", s.stderr);
    assert!(
        s.stdout.contains("\"origin\": \"local\""),
        "new comment becomes a local thread"
    );
    assert!(
        !s.stdout.contains("local:"),
        "local pending ids use action ids, not local:N"
    );
    assert!(s.stdout.contains("looks off"));
    assert!(
        s.stdout.contains("agreed"),
        "reply attaches to the remote thread"
    );
    assert!(s.stdout.contains("\"pending\""));
}

#[test]
fn a_remote_prefixed_thread_id_attaches_to_its_thread() {
    // The `remote:` form `show` prints must be an acceptable thread id on
    // `reply`/`resolve` — without normalization it would double-prefix
    // (`remote:remote:9987`) and match no thread.
    let fx = Fixture::new();
    fx.seed("1", "head111");
    fx.write_local(
        "1",
        r#"{ "schema": 1,
             "actions": [ { "action": "reply", "thread": "remote:9987", "body": "ok" } ] }"#,
    );

    let s = fx.run(&["review", "show", "1", "--json"]);
    assert!(s.success, "stderr: {}", s.stderr);
    assert!(
        s.stdout.contains("ok"),
        "remote:-prefixed reply attaches to its thread"
    );
}

#[test]
fn draft_ingest_appends_and_shows() {
    let fx = Fixture::new();
    fx.seed("1", "head111");

    // The tool owns the write: pipe a batch of actions in via `draft <mr> -`.
    let a = fx.run_stdin(
        &["review", "draft", "1", "-"],
        r#"{ "schema": 1, "verdict": "comment",
             "actions": [ { "action": "comment", "file": "src/x.c", "line": 7, "body": "first" } ] }"#,
    );
    assert!(a.success, "stderr: {}", a.stderr);
    // A second batch appends rather than replacing.
    let b = fx.run_stdin(
        &["review", "draft", "1", "-"],
        r#"{ "schema": 1, "actions": [ { "action": "reply", "thread": "9987", "body": "second" } ] }"#,
    );
    assert!(b.success, "stderr: {}", b.stderr);

    let d = fx.run(&["review", "draft", "1", "--json"]);
    assert!(d.stdout.contains("\"comment\""), "verdict preserved");
    assert!(
        d.stdout.contains("first") && d.stdout.contains("second"),
        "both batches present"
    );
    assert!(
        d.stdout.contains("\"id\""),
        "ingested actions are assigned stable ids"
    );
}

#[test]
fn draft_dedup_compacts_by_id_and_drop() {
    let fx = Fixture::new();
    fx.seed("1", "head111");
    fx.write_local(
        "1",
        r#"{ "schema": 1,
             "actions": [
               { "action": "comment", "id": "c1", "file": "src/x.c", "line": 7, "body": "old" },
               { "action": "comment", "id": "c1", "file": "src/x.c", "line": 8, "body": "new" },
               { "action": "summary", "id": "s1", "body": "summary" },
               { "action": "drop", "id": "s1" }
             ] }"#,
    );

    let out = fx.run(&["review", "draft", "1", "--dedup", "--json"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("new"));
    assert!(!out.stdout.contains("old"));
    assert!(!out.stdout.contains("summary"));
}

#[test]
fn submit_dry_run_plans_without_touching_the_network() {
    let fx = Fixture::new();
    fx.seed("1", "head111");
    fx.write_local(
        "1",
        r#"{ "schema": 1, "verdict": "request-changes",
             "actions": [
               { "action": "summary", "body": "overall blocker" },
               { "action": "comment", "file": "src/x.c", "line": 50, "body": "please fix" }
             ] }"#,
    );

    let out = fx.run(&["-n", "review", "submit", "1"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("[DRY-RUN]"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("request-changes"));
    assert!(out.stdout.contains("summary"));
    assert!(out.stdout.contains("src/x.c:50"));
    // A dry run leaves the draft untouched.
    assert!(fx.local_exists("1"));
}

#[test]
fn submit_drop_only_draft_does_not_require_a_fetched_snapshot() {
    let fx = Fixture::new();
    fx.write_local(
        "1",
        r#"{ "schema": 1, "actions": [ { "action": "drop", "id": "gone" } ] }"#,
    );

    let out = fx.run(&["review", "submit", "1"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        !fx.local_exists("1"),
        "local-only tombstones normalize away before snapshot lookup"
    );
}

#[test]
fn unknown_mr_is_a_clean_error_not_a_panic() {
    let fx = Fixture::new();
    let out = fx.run(&["review", "show", "99", "--json"]);
    assert!(!out.success);
    assert!(out.stderr.contains("isn't in the store") || out.stderr.contains("fetch"));
}
