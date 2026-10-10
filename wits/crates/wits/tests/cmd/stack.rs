//! Black-box tests for `wits stack slice` and `wits stack tree`.
//!
//! `slice` is a thin layer over `git rebase -i`, and what it can get wrong lives
//! in the interplay with git: which todo git generates under the user's rebase
//! settings, which `update-ref` lines git honours, which branches it refuses to
//! move. So these drive the real binary through real rebases; the unit tests
//! cover the todo text alone. The user's editor is played by a script named in
//! `GIT_SEQUENCE_EDITOR`, which `slice` resolves through git and runs on the
//! todo it seeded.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The user saves the todo exactly as `slice` seeded it.
const UNEDITED: &str = "true";

const STACK: &str = "main\n    feat-a\n        feat-b\n            feat-c\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// `main` with one commit, and `branch` checked out on top of it.
    fn new(branch: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // An empty project registry, so the developer's own is never consulted.
        std::fs::create_dir_all(root.join("registry")).unwrap();
        git(&repo, &["init", "-q", "-b", "main", "."]);
        let fx = Fixture {
            _dir: dir,
            root,
            repo,
        };
        fx.commit("base", "Base");
        fx.git(&["switch", "-q", "-c", branch]);
        fx
    }

    fn git(&self, args: &[&str]) {
        git(&self.repo, args);
    }

    /// Append `subject` to `file` and commit it under that subject.
    fn commit(&self, file: &str, subject: &str) {
        self.append(file, subject);
        self.git(&["commit", "-q", "-m", subject]);
    }

    /// Append a line to `file` and commit it as a fixup of `target`.
    fn fixup(&self, file: &str, target: &str) {
        self.append(file, "fix");
        self.git(&["commit", "-q", &format!("--fixup={target}")]);
    }

    fn append(&self, file: &str, line: &str) {
        let path = self.repo.join(file);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(line);
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        self.git(&["add", file]);
    }

    /// A, B and C on the checked-out branch, with `feat-a` and `feat-b` marking
    /// the first two.
    fn stack(&self) {
        self.commit("a", "Add A");
        self.git(&["branch", "feat-a"]);
        self.commit("b", "Add B");
        self.git(&["branch", "feat-b"]);
        self.commit("c", "Add C");
    }

    /// Move `main` on, so that slicing onto it rewrites every commit of the range.
    fn advance_main(&self) {
        self.git(&["switch", "-q", "main"]);
        self.commit("m", "Main moves");
        self.git(&["switch", "-q", "-"]);
    }

    /// Lay `forest` down as the stack, through `tree edit` reading stdin.
    fn record(&self, forest: &str) {
        let out = self.wits(&["stack", "tree", "edit", "-"], &[], Some(forest));
        assert!(out.success, "tree edit failed: {}", out.stderr);
    }

    /// The stack as `tree edit` would open it, read through an editor that only
    /// prints its buffer, with the help comments dropped.
    fn forest(&self) -> String {
        let out = self.wits(&["stack", "tree", "edit"], &[("GIT_EDITOR", "cat")], None);
        assert!(out.success, "tree edit failed: {}", out.stderr);
        out.stdout
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    /// Run wits in the repository with the scratch registry, extra environment,
    /// and optionally some stdin.
    fn wits(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Out {
        use std::io::Write as _;
        let mut child = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(&self.repo)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", self.root.join("registry"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.unwrap_or("").as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// An editor script running `body` on the todo, which it receives as `$1`.
    fn editor(&self, body: &str) -> String {
        let path = self.root.join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -e\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// `wits stack slice --base main`, with `editor` as the user's sequence editor.
    fn slice(&self, editor: &str) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(["stack", "slice", "--base", "main"])
            .current_dir(&self.repo)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", self.root.join("registry"))
            .env("GIT_SEQUENCE_EDITOR", editor)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn read(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .envs(hermetic_git())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn rev(&self, spec: &str) -> String {
        self.read(&["rev-parse", spec]).trim().to_owned()
    }

    /// The subjects of `range`, oldest first.
    fn subjects(&self, range: &str) -> Vec<String> {
        self.read(&["log", "--reverse", "--format=%s", range])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn rebasing(&self) -> bool {
        self.repo.join(".git/rebase-merge").exists()
    }
}

/// Git env that ignores the developer's own config — notably a global
/// `core.hooksPath`, whose hooks would fire into these rebases — while supplying
/// a commit identity and the rebase settings that shape the todo git generates:
/// fixups folded into their targets, and no commit lost from the todo unnoticed.
fn hermetic_git() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@e.com"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@e.com"),
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "rebase.autoSquash"),
        ("GIT_CONFIG_VALUE_0", "true"),
        ("GIT_CONFIG_KEY_1", "rebase.missingCommitsCheck"),
        ("GIT_CONFIG_VALUE_1", "error"),
    ]
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(hermetic_git())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {} failed", dir.display());
}

#[test]
fn a_fixup_folds_into_its_branch_and_the_stack_is_kept() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    // A review fix for A, committed on top of the stack.
    fx.fixup("a", "feat-a");

    let out = fx.slice(UNEDITED);
    assert!(out.success, "slice failed: {}", out.stderr);

    // The fix ships in feat-a, not as a stray commit at the tip of feat-c...
    assert_eq!(fx.subjects("main..feat-c"), ["Add A", "Add B", "Add C"]);
    assert_eq!(fx.read(&["show", "feat-a:a"]), "Add A\nfix\n");
    // ...and every branch still sits on its own, now rewritten, commit.
    assert_eq!(fx.rev("feat-c~1"), fx.rev("feat-b"));
    assert_eq!(fx.rev("feat-b~1"), fx.rev("feat-a"));
    assert!(
        out.stderr.contains("sliced into: feat-a, feat-b, feat-c"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!(fx.forest(), STACK);
}

#[test]
fn slicing_onto_a_moved_base_records_the_checked_out_branch() {
    // Every commit is rewritten, which is when an update-ref line of the
    // checked-out branch would make git fail the rebase at its very end.
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    fx.advance_main();

    let out = fx.slice(UNEDITED);
    assert!(out.success, "slice failed: {}", out.stderr);

    assert_eq!(fx.rev("feat-a~1"), fx.rev("main"));
    assert_eq!(fx.rev("feat-c~2"), fx.rev("feat-a"));
    assert_eq!(fx.rev("feat-c~1"), fx.rev("feat-b"));
    assert!(
        out.stderr.contains("sliced into: feat-a, feat-b, feat-c"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn a_commit_already_upstream_is_dropped_by_git_not_replayed() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record("main\n    feat-b\n        feat-c\n");
    // A reached main by cherry-pick, the way a merged bottom MR can.
    fx.git(&["switch", "-q", "main"]);
    fx.git(&["cherry-pick", "feat-a"]);
    fx.git(&["switch", "-q", "-"]);

    let out = fx.slice(UNEDITED);
    assert!(out.success, "slice failed: {}", out.stderr);

    assert_eq!(fx.subjects("main..feat-c"), ["Add B", "Add C"]);
    assert_eq!(fx.forest(), "main\n    feat-b\n        feat-c\n");
}

#[test]
fn a_stack_branch_checked_out_elsewhere_stops_the_slice_before_git_runs() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    fx.advance_main();
    let worktree = fx.root.join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        worktree.to_str().unwrap(),
        "feat-b",
    ]);
    let (b, c) = (fx.rev("feat-b"), fx.rev("feat-c"));

    let out = fx.slice(UNEDITED);
    assert!(!out.success, "slice should refuse");
    assert!(
        out.stderr.contains("'feat-b' is checked out in"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!((fx.rev("feat-b"), fx.rev("feat-c")), (b, c));
    assert!(!fx.rebasing());
}

#[test]
fn a_first_slice_takes_the_suggested_names_and_the_checked_out_branch() {
    let fx = Fixture::new("work");
    fx.commit("a", "Add A");
    fx.commit("b", "Add B");
    fx.commit("c", "Add C");
    fx.advance_main();
    // Take every suggestion, the checked-out branch's own line included.
    let editor = fx.editor(
        r#"sed 's|^# update-ref refs/heads/|update-ref refs/heads/|' "$1" > "$1.new"
mv "$1.new" "$1""#,
    );

    let out = fx.slice(&editor);
    assert!(out.success, "slice failed: {}", out.stderr);

    assert_eq!(
        fx.forest(),
        "main\n    stack/add-a\n        stack/add-b\n            work\n"
    );
    assert_eq!(fx.rev("stack/add-a"), fx.rev("work~2"));
    assert_eq!(fx.rev("stack/add-b"), fx.rev("work~1"));
    assert_eq!(fx.rev("work~3"), fx.rev("main"));
}

#[test]
fn the_checked_out_branch_cannot_be_assigned_mid_stack() {
    let fx = Fixture::new("work");
    fx.commit("a", "Add A");
    fx.commit("b", "Add B");
    fx.record("main\n    work\n");
    let before = fx.rev("work");
    // Move `work`'s line up, under the first pick.
    let editor = fx.editor(
        r#"awk 'NR == 1 { print; print "update-ref refs/heads/work"; next }
     $0 != "update-ref refs/heads/work"' "$1" > "$1.new"
mv "$1.new" "$1""#,
    );

    let out = fx.slice(&editor);
    assert!(!out.success, "slice should refuse");
    assert!(
        out.stderr
            .contains("keep the update-ref line of the current branch 'work'"),
        "stderr: {}",
        out.stderr
    );
    assert_eq!(fx.rev("work"), before);
    assert!(!fx.rebasing());
    assert_eq!(fx.forest(), "main\n    work\n");
}

#[test]
fn a_renamed_branch_keeps_its_place_and_its_children() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    fx.git(&["branch", "-m", "feat-b", "feat-b2"]);
    assert_eq!(
        fx.forest(),
        "main\n    feat-a\n        feat-b2\n            feat-c\n"
    );
}

#[test]
fn a_deleted_branchs_children_splice_up_to_its_parent() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    fx.git(&["branch", "-D", "feat-b"]);
    assert_eq!(fx.forest(), "main\n    feat-a\n        feat-c\n");
}

#[test]
fn tree_edit_refuses_a_name_that_is_not_a_branch_and_changes_nothing() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    let out = fx.wits(
        &["stack", "tree", "edit", "-"],
        &[],
        Some("main\n    feat-a\n        nope\n"),
    );
    assert!(!out.success);
    assert!(
        out.stderr.contains("nope is not a local branch"),
        "{}",
        out.stderr
    );
    assert_eq!(fx.forest(), STACK);
}

#[test]
fn tree_rm_splices_children_up_and_delete_removes_the_branch() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    let out = fx.wits(
        &["stack", "tree", "rm", "--delete", "--force", "feat-b"],
        &[],
        None,
    );
    assert!(out.success, "{}", out.stderr);
    assert_eq!(fx.forest(), "main\n    feat-a\n        feat-c\n");
    assert!(fx.read(&["branch", "--list", "feat-b"]).is_empty());
}

#[test]
fn tree_mv_moves_a_branch_with_its_substack() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    let out = fx.wits(
        &["stack", "tree", "mv", "feat-b", "--onto", "main"],
        &[],
        None,
    );
    assert!(out.success, "{}", out.stderr);
    assert_eq!(
        fx.forest(),
        "main\n    feat-a\n    feat-b\n        feat-c\n"
    );
}

#[test]
fn status_reports_what_each_branch_waits_on() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    // feat-a moves on under feat-b and feat-c.
    fx.git(&["switch", "-q", "feat-a"]);
    fx.commit("a", "Amend A");
    fx.git(&["switch", "-q", "feat-c"]);

    let out = fx.wits(&["stack", "status", "--offline", "--json"], &[], None);
    assert!(out.success, "{}", out.stderr);
    let report: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(report["base"], "main");
    let rows = report["branches"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["feat-a", "feat-b", "feat-c"]);
    assert_eq!(rows[1]["restack"], 1, "feat-a has a commit feat-b lacks");
    assert_eq!(rows[1]["needs"], serde_json::json!(["restack"]));
    assert_eq!(rows[2]["current"], true);
    assert!(rows[2]["restack"].is_null(), "feat-b did not move");
}

#[test]
fn a_bare_wits_stack_shows_the_tree_without_a_forge() {
    let fx = Fixture::new("feat-c");
    fx.stack();
    fx.record(STACK);
    let out = fx.wits(&["stack"], &[], None);
    assert!(out.success, "{}", out.stderr);
    assert!(out.stdout.starts_with("main\n"), "{}", out.stdout);
    assert!(out.stdout.contains("└── feat-a"), "{}", out.stdout);
    assert!(
        out.stdout.contains("        └── feat-c *"),
        "{}",
        out.stdout
    );
}
