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

        let fx = Fixture {
            _dir: dir,
            repo,
            store,
        };
        fx.git(&["init", "-b", "main"]);
        fx.git(&["config", "user.email", "t@example.com"]);
        fx.git(&["config", "user.name", "Test"]);
        fx.git(&["remote", "add", "origin", "git@github.com:me/proj.git"]);
        fx
    }

    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.repo.join(path), text).unwrap();
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
        self.rev("HEAD")
    }

    fn rev(&self, spec: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn mr_dir(&self, id: &str) -> PathBuf {
        self.store.join("github.com/me/proj").join(id)
    }

    /// Seed a completed fetch: `info.json` (with one changed file and a reviewed
    /// snapshot) and `comments.json` (one remote thread).
    fn seed(&self, id: &str, head_sha: &str) {
        let info = INFO.replace("__ID__", id).replace("__HEAD__", head_sha);
        self.seed_info(id, &info);
        std::fs::write(self.mr_dir(id).join("comments.json"), COMMENTS).unwrap();
    }

    fn seed_info(&self, id: &str, json: &str) {
        let dir = self.mr_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.json"), json).unwrap();
    }

    /// Build the shape every force-push raises a question about, and seed a
    /// two-snapshot store over it:
    ///
    /// ```text
    ///   root ────────────── trunk-moves-on          (main)
    ///     │                      │
    ///     └── v1: edit + add     └── v2: same two commits, edit reworked
    /// ```
    ///
    /// Both snapshots report the *trunk tip* as `base_sha`, the way GitHub's
    /// `baseRefOid` does — only the second actually forked there. So the first
    /// snapshot is exactly the case where `base..head` lies and `fork..head`
    /// tells the truth.
    fn two_snapshots(&self) -> History {
        self.write("shared.c", "one\ntwo\nthree\n");
        self.write("trunk-only.c", "untouched by the MR\n");
        let fork1 = self.commit("root");

        self.git(&["checkout", "-b", "feature"]);
        self.write("shared.c", "one\ntwo\nTHREE\n");
        self.commit("MR: edit shared");
        self.write("mr-new.c", "added by the MR\n");
        let head1 = self.commit("MR: add a file");

        self.git(&["checkout", "main"]);
        self.write("trunk-only.c", "the trunk moved on\n");
        let tip = self.commit("trunk: unrelated work");

        self.git(&["checkout", "-B", "feature2", "main"]);
        self.write("shared.c", "one\ntwo\nTHREE (reworked)\n");
        self.commit("MR: edit shared");
        self.write("mr-new.c", "added by the MR\n");
        let head2 = self.commit("MR: add a file");

        self.seed_info(
            "7",
            &TWO_SNAPSHOTS
                .replace("__FORK1__", &fork1)
                .replace("__HEAD1__", &head1)
                .replace("__TIP__", &tip)
                .replace("__HEAD2__", &head2),
        );
        History { head1, head2 }
    }

    /// A rebase that silently reverted a change the base had made — the author
    /// resolved by keeping their own side. The two heads end up with *identical*
    /// content, so nothing distinguishes them by tree; only replaying the old
    /// series onto the new base reveals that the MR now undoes the base's work.
    ///
    /// The two edits sit far apart in the file on purpose, so the replay is clean
    /// and the revert has to be caught by the comparison itself rather than by
    /// the conflict fallback.
    fn clobbered_rebase(&self) -> History {
        let driver = |setup: &str, configure: &str| {
            let filler = |from: u32, to: u32| {
                (from..=to)
                    .map(|i| format!("    step_{i}();\n"))
                    .collect::<String>()
            };
            format!(
                "{}{setup}\n{}{configure}\n{}",
                filler(1, 10),
                filler(11, 20),
                filler(21, 30)
            )
        };

        self.write("driver.c", &driver("    setup();", "    configure();"));
        let fork1 = self.commit("root");

        self.git(&["checkout", "-b", "feature"]);
        self.write(
            "driver.c",
            &driver("    setup();", "    configure(FLAG_FAST);"),
        );
        let head1 = self.commit("driver: pass FLAG_FAST");

        self.git(&["checkout", "main"]);
        self.write("driver.c", &driver("    setup(ctx);", "    configure();"));
        let tip = self.commit("driver: thread ctx through setup");

        // Rebased onto `tip`, but `setup(ctx)` came back out — the same content
        // as `head1`, reached from a base that no longer says that.
        self.git(&["checkout", "-B", "feature2", "main"]);
        self.write(
            "driver.c",
            &driver("    setup();", "    configure(FLAG_FAST);"),
        );
        let head2 = self.commit("driver: pass FLAG_FAST");

        self.seed_info(
            "7",
            &TWO_SNAPSHOTS
                .replace("__FORK1__", &fork1)
                .replace("__HEAD1__", &head1)
                .replace("__TIP__", &tip)
                .replace("__HEAD2__", &head2),
        );
        History { head1, head2 }
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

/// The two review points [`Fixture::two_snapshots`] seeds, by head SHA.
struct History {
    head1: String,
    head2: String,
}

const INFO: &str = r##"{
  "schema": 1,
  "mr": { "id": "__ID__", "display": "#__ID__", "state": "open", "draft": false,
          "title": "Add a thing", "author": "alice", "base": "main",
          "source": "feature-__ID__", "head_sha": "__HEAD__",
          "updated_at": "2026-07-01T00:00:00Z", "labels": [],
          "web_url": "https://github.com/me/proj/pull/__ID__" },
  "snapshots": [ { "base_sha": "base000", "start_sha": "base000",
                   "head_sha": "__HEAD__", "fork_sha": "base000" } ],
  "fetched_at": 1700000000,
  "commits": [ { "sha": "__HEAD__", "subject": "Add a thing" } ],
  "files": [ { "path": "src/x.c", "status": "M" } ]
}"##;

/// A terminal MR — what `prune` sweeps without needing `--older-than`.
const MERGED: &str = r##"{
  "schema": 1,
  "mr": { "id": "1", "display": "#1", "state": "merged", "draft": false,
          "title": "Landed a while ago", "author": "alice", "base": "main",
          "source": "feature-1", "head_sha": "head111",
          "updated_at": "2026-07-01T00:00:00Z", "labels": [],
          "web_url": "https://github.com/me/proj/pull/1" },
  "snapshots": [], "fetched_at": 1700000000, "commits": [], "files": []
}"##;

/// A store entry a feed refresh would leave: metadata only, no review point.
const FEED_ONLY: &str = r##"{
  "schema": 1,
  "mr": { "id": "8", "display": "#8", "state": "open", "draft": false,
          "title": "Never fully fetched", "author": "bob", "base": "main",
          "source": "wip", "updated_at": "2026-07-01T00:00:00Z", "labels": [],
          "web_url": "https://github.com/me/proj/pull/8" },
  "snapshots": [], "fetched_at": 0, "commits": [], "files": []
}"##;

const TWO_SNAPSHOTS: &str = r##"{
  "schema": 1,
  "mr": { "id": "7", "display": "#7", "state": "open", "draft": false,
          "title": "Edit shared and add a file", "author": "alice", "base": "main",
          "source": "feature", "head_sha": "__HEAD2__",
          "updated_at": "2026-07-01T00:00:00Z", "labels": ["vulkan", "wip"],
          "web_url": "https://github.com/me/proj/pull/7" },
  "snapshots": [
    { "base_sha": "__TIP__", "start_sha": "__TIP__",
      "head_sha": "__HEAD1__", "fork_sha": "__FORK1__" },
    { "base_sha": "__TIP__", "start_sha": "__TIP__",
      "head_sha": "__HEAD2__", "fork_sha": "__TIP__" }
  ],
  "fetched_at": 1700000000,
  "commits": [ { "sha": "__HEAD2__", "subject": "MR: add a file" } ],
  "files": [ { "path": "shared.c", "status": "M" },
             { "path": "mr-new.c", "status": "A" } ]
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

/// The regression: `prune` carried no dry-run guard at all, so `-n` destroyed the
/// very thing it was meant to preview — `local.json` included, the one file in the
/// store that no refetch can rebuild.
#[test]
fn prune_dry_run_previews_without_dropping_the_store() {
    let fx = Fixture::new();
    fx.seed_info("1", MERGED);
    fx.write_local(
        "1",
        r#"{ "schema": 1,
             "actions": [
               { "action": "comment", "file": "src/x.c", "line": 3, "body": "unsubmitted" }
             ] }"#,
    );

    let preview = fx.run(&["-n", "review", "prune"]);
    assert!(preview.success, "stderr: {}", preview.stderr);
    assert!(
        preview.stdout.contains("[DRY-RUN]") && preview.stdout.contains("prune MR 1"),
        "stdout: {}",
        preview.stdout
    );
    assert!(
        fx.local_exists("1"),
        "a dry run deleted the draft it was asked to preview"
    );

    // The real sweep still drops it, so the guard did not disable pruning.
    let real = fx.run(&["review", "prune"]);
    assert!(real.success, "stderr: {}", real.stderr);
    assert!(!fx.local_exists("1"));
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
fn a_review_point_spec_uses_its_pinned_fork_not_the_moved_base() {
    // The bug this guards: the forges disagree about `base_sha`. GitLab's is
    // already the merge base, GitHub's is the target branch's *current tip*, so
    // diffing straight against it replays the trunk's own progress as inverted
    // hunks. `trunk-only.c` is the witness — the MR never touches it, so it must
    // not appear in the MR's diff, however far `main` has moved.
    //
    // Naming a review point by its head SHA must therefore use the fork pinned
    // when it was fetched, rather than recomputing anything.
    let fx = Fixture::new();
    let history = fx.two_snapshots();

    let out = fx.run(&["review", "diff", "7", "--range", &history.head1]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("shared.c"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("mr-new.c"), "stdout: {}", out.stdout);
    assert!(
        !out.stdout.contains("trunk-only.c"),
        "the moved base leaked into the MR's diff: {}",
        out.stdout
    );

    // And the endpoints the payload hands an editor are fork..head, with the
    // forge's base reported separately rather than as something to diff against.
    let json = fx.run(&["review", "diff", "7", "--range", &history.head1, "--json"]);
    assert!(json.success, "stderr: {}", json.stderr);
    let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(v["mode"], "range");
    assert_eq!(v["to"]["head_sha"], history.head1.as_str());
    assert_ne!(
        v["to"]["fork_sha"], v["to"]["base_sha"],
        "fork point and forge base must not be conflated"
    );
}

#[test]
fn a_literal_range_describes_only_that_range() {
    // The wart the range grammar removes: a bare revision used to be accepted and
    // resolved to `git log <rev>` (the whole history) plus a diff against the
    // working tree. A range has two named ends and no room for that.
    let fx = Fixture::new();
    let history = fx.two_snapshots();
    let root = fx.rev("main~1");

    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--range",
        &format!("{root}..{}", history.head1),
        "--json",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();

    assert_eq!(v["to"]["fork_sha"], root.as_str());
    assert_eq!(v["to"]["head_sha"], history.head1.as_str());
    assert!(
        v["to"]["base_sha"].is_null(),
        "a hand-written range has no forge base to report: {}",
        out.stdout
    );
    // Exactly the MR's two commits — not the root, and not all of history.
    assert_eq!(v["commits"].as_array().unwrap().len(), 2, "{}", out.stdout);
}

#[test]
fn a_range_always_starts_from_the_merge_base() {
    // `A..B` resolves to `merge-base(A,B)..B`. When A is an ancestor of B that
    // changes nothing, which is what makes the rule free rather than surprising;
    // when A has diverged it is the difference between a real patch series and a
    // two-endpoint compare full of inverted hunks.
    let fx = Fixture::new();
    let history = fx.two_snapshots();
    let root = fx.rev("main~1");

    let ancestor = fx.run(&[
        "review",
        "diff",
        "7",
        "--range",
        &format!("{root}..{}", history.head2),
        "--json",
    ]);
    assert!(ancestor.success, "stderr: {}", ancestor.stderr);
    let a: serde_json::Value = serde_json::from_str(&ancestor.stdout).unwrap();
    assert_eq!(
        a["to"]["fork_sha"],
        root.as_str(),
        "an ancestor is its own merge base"
    );

    // head1 and head2 are two versions of the branch, so they diverge: their
    // merge base is the original fork, not head1.
    let divergent = fx.run(&[
        "review",
        "diff",
        "7",
        "--range",
        &format!("{}..{}", history.head1, history.head2),
        "--json",
    ]);
    assert!(divergent.success, "stderr: {}", divergent.stderr);
    let d: serde_json::Value = serde_json::from_str(&divergent.stdout).unwrap();
    assert_eq!(
        d["to"]["fork_sha"],
        root.as_str(),
        "divergent ends must fall back to their common ancestor: {}",
        divergent.stdout
    );
}

#[test]
fn a_spec_that_is_neither_a_range_nor_a_review_point_is_refused() {
    // No sugar: a bare revision is not a range, so the tool refuses rather than
    // guessing a base for it. The message has to name both ways out.
    let fx = Fixture::new();
    fx.two_snapshots();

    let bare = fx.run(&["review", "diff", "7", "--range", "main"]);
    assert!(!bare.success);
    assert!(bare.stderr.contains("A..B"), "{}", bare.stderr);
    assert!(bare.stderr.contains("--details"), "{}", bare.stderr);

    // git's three-dot form is named rather than passed through as a nonsense rev.
    let three_dot = fx.run(&["review", "diff", "7", "--range", "main...feature2"]);
    assert!(!three_dot.success);
    assert!(
        three_dot.stderr.contains("three-dot"),
        "{}",
        three_dot.stderr
    );

    // An omitted side would be HEAD to git, which is never the intent here.
    let half = fx.run(&["review", "diff", "7", "--range", "..feature2"]);
    assert!(!half.success);
    assert!(half.stderr.contains("both ends"), "{}", half.stderr);
}

#[test]
fn explicit_ranges_need_no_fetched_review_point() {
    // Two ranges are self-describing, so nothing has to consult the review
    // history — which is what makes the cherry-pick case from the upstream README
    // a first-class use rather than something riding on review state. Only an
    // omitted `--range` needs a review point to fall back on.
    let fx = Fixture::new();
    let history = fx.two_snapshots();
    let root = fx.rev("main~1");
    let tip = fx.rev("main");
    // Same MR, but a store entry that has never been fully fetched.
    fx.seed_info("8", FEED_ONLY);

    let bare = fx.run(&["review", "diff", "8"]);
    assert!(!bare.success, "an omitted range still needs a review point");
    assert!(bare.stderr.contains("--range"), "{}", bare.stderr);

    let explicit = fx.run(&[
        "review",
        "diff",
        "8",
        "--range",
        &format!("{tip}..{}", history.head2),
        "--against",
        &format!("{root}..{}", history.head1),
    ]);
    assert!(explicit.success, "stderr: {}", explicit.stderr);
    assert!(explicit.stdout.contains("shared.c"), "{}", explicit.stdout);
}

#[test]
fn two_hand_written_ranges_carry_no_forge_base() {
    // Comparing modulo the base is a general operation on two ranges — the
    // cherry-pick case from the upstream README, where neither end is a review
    // point and so neither has a forge base to report.
    let fx = Fixture::new();
    let history = fx.two_snapshots();
    let root = fx.rev("main~1");
    let tip = fx.rev("main");

    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--range",
        &format!("{tip}..{}", history.head2),
        "--against",
        &format!("{root}..{}", history.head1),
        "--json",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();

    assert_eq!(v["mode"], "interdiff");
    assert_eq!(v["from"]["head_sha"], history.head1.as_str());
    assert_eq!(v["to"]["head_sha"], history.head2.as_str());
    assert_eq!(
        v["rebased"], true,
        "the two ranges start from different forks"
    );
    for end in ["from", "to"] {
        assert!(
            v[end]["base_sha"].is_null(),
            "{end} was written as a range, so it has no forge base: {}",
            out.stdout
        );
    }
}

#[test]
fn against_pairs_the_two_review_points() {
    let fx = Fixture::new();
    let history = fx.two_snapshots();

    let out = fx.run(&["review", "diff", "7", "--against", &history.head1, "--json"]);
    assert!(out.success, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();

    assert_eq!(v["mode"], "interdiff");
    assert_eq!(v["from"]["head_sha"], history.head1.as_str());
    assert_eq!(v["to"]["head_sha"], history.head2.as_str());
    assert_eq!(v["rebased"], true, "the fork point moved between the two");

    // The untouched commit is recognised as the same patch across the rebase,
    // which is the whole point of pairing by patch rather than by SHA.
    let pairings: Vec<&str> = v["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pairing"].as_str().unwrap())
        .collect();
    assert!(
        pairings.contains(&"unchanged"),
        "the carried-through commit should pair: {pairings:?}"
    );

    // Only the file the MR actually reworked; the trunk's own change to
    // `trunk-only.c` sits between the two heads but is not the MR's doing.
    let files: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["shared.c"], "trunk drift leaked in");
}

#[test]
fn a_two_way_patch_shows_the_rework_without_the_trunk_drift() {
    let fx = Fixture::new();
    let history = fx.two_snapshots();

    // `two_snapshots` reworks a line the base never touches, so the replay is
    // clean and the answer is the author's edit alone.
    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--against",
        &history.head1,
        "--patch",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("reworked"),
        "the real edit must show: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("trunk-only.c"),
        "trunk drift leaked into the comparison: {}",
        out.stdout
    );
}

#[test]
fn identical_heads_with_differing_series_are_never_silent() {
    // The reduction's honest limit. The rebase reverted the base's `setup(ctx)`,
    // so the MR now undoes someone else's work — but the two heads end up with
    // *identical* content, leaving no target diff to reduce. A bare "" here would
    // read as "the author changed nothing", which is the one wrong conclusion.
    //
    // So the empty result is checked against the commit pairing, and the
    // situation named. `3way`, which works from each series' own diff rather
    // than from the difference between the heads, does show it.
    let fx = Fixture::new();
    let history = fx.clobbered_rebase();

    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--against",
        &history.head1,
        "--patch",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("identical content") && out.stderr.contains("commits differ against"),
        "an empty patch must be explained, not just printed: {}",
        out.stderr
    );

    let three = fx.run(&[
        "review",
        "diff",
        "7",
        "--against",
        &history.head1,
        "--patch=3way",
    ]);
    assert!(three.success, "stderr: {}", three.stderr);
    assert!(
        three.stdout.contains("setup(ctx)"),
        "3way should expose the reverted base change:\n{}",
        three.stdout
    );
}

#[test]
fn a_recorded_fork_that_cannot_be_right_is_called_out() {
    // Seen on a real MR: a review point recorded a fork that is not an ancestor
    // of its own head, from back when `fetch` would settle for the forge's base
    // if it could not resolve a merge base. `fork..head` is then not a series at
    // all and everything built on it is too wide — 22 files reported against the
    // 4 actually touched.
    //
    // `fetch` now refuses to record such a snapshot, so this can only arrive as
    // old data, and nothing can repair it in place (the forge's base is
    // deliberately no longer kept). The read path names it and says what to do.
    let fx = Fixture::new();
    let history = fx.two_snapshots();
    let tip = fx.rev("main");

    // Corrupt snapshot 1's fork to the trunk tip, which is *not* an ancestor of
    // head1 — exactly the shape found in the wild.
    fx.seed_info(
        "7",
        &TWO_SNAPSHOTS
            .replace("__FORK1__", &tip)
            .replace("__HEAD1__", &history.head1)
            .replace("__TIP__", &tip)
            .replace("__HEAD2__", &history.head2),
    );

    let out = fx.run(&["review", "diff", "7", "--against", &history.head1, "--json"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("not an ancestor") && out.stderr.contains("fetch"),
        "a broken fork must be reported, not used in silence: {}",
        out.stderr
    );
}

#[test]
fn a_two_way_patch_excludes_the_base_entirely() {
    // The guarantee: with a clean replay, both sides stand on the same base, so
    // base movement cancels by construction — not by a heuristic about which
    // hunks looked base-caused. `trunk-only.c` changed on the base between the
    // two review points and must leave no trace whatsoever.
    let fx = Fixture::new();
    let history = fx.two_snapshots();

    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--against",
        &history.head1,
        "--patch",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        !out.stdout.contains("trunk-only"),
        "base movement leaked into the answer:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("THREE (reworked)"),
        "the author's actual edit is missing:\n{}",
        out.stdout
    );
    // A real unified diff, not a prefixed or annotated rendering.
    assert!(
        out.stdout.starts_with("diff --git"),
        "expected a plain git patch:\n{}",
        out.stdout
    );
}

#[test]
fn a_base_change_overlapping_the_series_is_kept_not_refused() {
    // The base rewrote the line directly above the one the series edits. A
    // merge-based approach has no answer here — the replay conflicts — but the
    // reduction has no trouble: the region is inside the series' own footprint,
    // so the hunk is kept and the reader sees both the base's move and the edit,
    // which is exactly the conflict-resolution zone worth reading.
    let fx = Fixture::new();
    fx.write("driver.c", "init();\nsetup();\nconfigure();\n");
    let fork1 = fx.commit("root");
    fx.git(&["checkout", "-b", "feature"]);
    fx.write("driver.c", "init();\nsetup();\nconfigure(FLAG_FAST);\n");
    let head1 = fx.commit("mr: flag");
    fx.git(&["checkout", "main"]);
    fx.write("driver.c", "init();\nsetup(ctx);\nconfigure();\n");
    let tip = fx.commit("base: adjacent line");
    fx.git(&["checkout", "-B", "feature2", "main"]);
    fx.write("driver.c", "init();\nsetup(ctx);\nconfigure(FLAG_SAFE);\n");
    let head2 = fx.commit("mr: flag");
    fx.seed_info(
        "7",
        &TWO_SNAPSHOTS
            .replace("__FORK1__", &fork1)
            .replace("__HEAD1__", &head1)
            .replace("__TIP__", &tip)
            .replace("__HEAD2__", &head2),
    );

    let out = fx.run(&["review", "diff", "7", "--against", &head1, "--patch"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        out.stdout.starts_with("diff --git"),
        "a plain patch, no fallback rendering:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("FLAG_SAFE"),
        "the author's edit must show:\n{}",
        out.stdout
    );
    assert!(
        out.stderr.is_empty(),
        "nothing to warn about — the reduction handles this: {}",
        out.stderr
    );
}

#[test]
fn three_way_prints_each_side_and_the_relevant_part_of_the_base() {
    let fx = Fixture::new();
    let history = fx.two_snapshots();

    let out = fx.run(&[
        "review",
        "diff",
        "7",
        "--against",
        &history.head1,
        "--patch=3way",
    ]);
    assert!(out.success, "stderr: {}", out.stderr);
    for section in ["=== A", "=== B", "=== base"] {
        assert!(
            out.stdout.contains(section),
            "missing {section:?} in:\n{}",
            out.stdout
        );
    }
    // The base's only change here is to a file neither series touches, so the
    // base section is empty. Printing it would bury the two sections that matter
    // — on a real MR that section ran to 23,000 lines.
    assert!(
        !out.stdout.contains("trunk-only.c"),
        "the base section must stay within the files under review:\n{}",
        out.stdout
    );
}

#[test]
fn the_three_way_base_section_keeps_changes_to_reviewed_files() {
    // The complement of the test above: when the base *did* touch a file the
    // series works on, that is precisely what the base section is for.
    let fx = Fixture::new();
    fx.write("shared.c", "one\ntwo\nthree\n");
    let fork1 = fx.commit("root");
    fx.git(&["checkout", "-b", "feature"]);
    fx.write("shared.c", "one\ntwo\nTHREE\n");
    let head1 = fx.commit("mr: raise three");
    fx.git(&["checkout", "main"]);
    fx.write("shared.c", "ONE (by the base)\ntwo\nthree\n");
    let tip = fx.commit("base: raise one");
    fx.git(&["checkout", "-B", "feature2", "main"]);
    fx.write("shared.c", "ONE (by the base)\ntwo\nTHREE (reworked)\n");
    let head2 = fx.commit("mr: raise three");
    fx.seed_info(
        "7",
        &TWO_SNAPSHOTS
            .replace("__FORK1__", &fork1)
            .replace("__HEAD1__", &head1)
            .replace("__TIP__", &tip)
            .replace("__HEAD2__", &head2),
    );

    let out = fx.run(&["review", "diff", "7", "--against", &head1, "--patch=3way"]);
    assert!(out.success, "stderr: {}", out.stderr);
    let base_section = out.stdout.split("=== base").nth(1).unwrap_or("");
    assert!(
        base_section.contains("ONE (by the base)"),
        "a base change in a reviewed file belongs in the base section:\n{}",
        out.stdout
    );
}

#[test]
fn a_three_way_patch_needs_a_second_range() {
    // `3way` shows two versions against their base, so one range has nothing to
    // put in two of the three sections. Say so rather than printing blanks.
    let fx = Fixture::new();
    fx.two_snapshots();

    let out = fx.run(&["review", "diff", "7", "--patch=3way"]);
    assert!(!out.success);
    assert!(out.stderr.contains("--against"), "{}", out.stderr);
}

#[test]
fn patch_mode_must_be_attached_with_an_equals_sign() {
    // Without `require_equals`, `diff --patch 7` would read `7` as the mode and
    // leave the MR unnamed. The space form is refused outright.
    let fx = Fixture::new();
    fx.two_snapshots();

    let spaced = fx.run(&["review", "diff", "7", "--patch", "3way"]);
    assert!(!spaced.success);

    // Bare `--patch` still means 2way.
    let bare = fx.run(&["review", "diff", "7", "--patch"]);
    assert!(bare.success, "stderr: {}", bare.stderr);
    assert!(bare.stdout.contains("shared.c"), "{}", bare.stdout);
}

#[test]
fn against_requires_a_value() {
    // `--against` shares `--range`'s grammar, so a bare flag would be ambiguous
    // rather than convenient. There is no `prev` keyword to fall back on.
    let fx = Fixture::new();
    fx.two_snapshots();

    let out = fx.run(&["review", "diff", "7", "--against"]);
    assert!(!out.success);
    assert!(out.stderr.contains("value is required"), "{}", out.stderr);
}

#[test]
fn details_prints_the_metadata_and_the_snapshot_history() {
    let fx = Fixture::new();
    fx.two_snapshots();

    let out = fx.run(&["review", "show", "7", "--details"]);
    assert!(out.success, "stderr: {}", out.stderr);
    for expected in [
        "author",
        "alice",
        "feature -> main",
        "vulkan, wip",
        "history",
        "rebased",
        "threads",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing {expected:?} in:\n{}",
            out.stdout
        );
    }
    // Metadata, not a content dump: the headline appears once, not twice.
    assert_eq!(
        out.stdout.matches("Edit shared and add a file").count(),
        1,
        "stdout: {}",
        out.stdout
    );

    // The inbox grows a detail line per row rather than a whole block.
    let inbox = fx.run(&["review", "show", "--details"]);
    assert!(inbox.success, "stderr: {}", inbox.stderr);
    assert!(inbox.stdout.contains("feature -> main"), "{}", inbox.stdout);
    assert!(inbox.stdout.contains("2 snapshot(s)"), "{}", inbox.stdout);
}

#[test]
fn thread_counts_survive_a_filter() {
    // `--details` reports the shape of the whole discussion, so a filtered
    // thread list still tells you how much you are not looking at.
    let fx = Fixture::new();
    fx.seed("1", "head111");

    let out = fx.run(&["review", "show", "1", "--details", "--file", "nothing.c"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("1 total"),
        "counts must precede filtering: {}",
        out.stdout
    );
    assert!(out.stdout.contains("(no threads)"), "{}", out.stdout);
}

#[test]
fn unknown_mr_is_a_clean_error_not_a_panic() {
    let fx = Fixture::new();
    let out = fx.run(&["review", "show", "99", "--json"]);
    assert!(!out.success);
    assert!(out.stderr.contains("isn't in the store") || out.stderr.contains("fetch"));
}
