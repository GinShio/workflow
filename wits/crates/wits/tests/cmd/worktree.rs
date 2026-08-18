//! Black-box tests for `wits worktree`.
//!
//! Worktrees are cheap to create for real, and every interesting behaviour here
//! is a *judgement about git state* — is this branch merged, is that tree dirty,
//! is this record stale — so these drive the real binary against a real clone
//! rather than mocking git. The fixture is a clone of a local "upstream", because
//! `merged` is judged against the remote trunk and `upstream gone` needs a
//! remote-tracking ref to lose.
//!
//! What is pinned here is the part that deletes things: which worktrees a sweep
//! selects, which it refuses, and that `-n` changes nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The clone every command runs in — the "main worktree".
    repo: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Fixture {
    /// An upstream carrying `main` plus a branch per name, cloned into `work`.
    fn new(branches: &[&str]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();

        git(&up, &["init", "-q", "-b", "main", "."]);
        std::fs::write(up.join("root"), "r").unwrap();
        std::fs::write(up.join(".gitignore"), "_build/\n").unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "-q", "-m", "c1"]);
        for branch in branches {
            git(&up, &["branch", branch]);
        }

        let repo = root.join("work");
        git(&root, &["clone", "-q", up.to_str().unwrap(), "work"]);
        // Trunk detection reads `refs/remotes/origin/HEAD`, which a local clone
        // does not always establish; set it so `merged` has something to judge.
        git(&repo, &["remote", "set-head", "origin", "-a"]);
        // `clone` gives local branches only for the default one, and `create`
        // refuses a branch that exists only on the remote — so give each of them
        // the local branch a real working setup would have.
        for branch in branches {
            git(&repo, &["branch", branch, &format!("origin/{branch}")]);
        }

        Fixture {
            _dir: dir,
            root,
            repo,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Commit on a worktree's branch so it is no longer an ancestor of the trunk.
    fn diverge(&self, worktree: &Path) {
        std::fs::write(worktree.join("newfile"), "work").unwrap();
        git(worktree, &["add", "-A"]);
        git(worktree, &["commit", "-q", "-m", "diverged"]);
    }

    /// The abbreviation git itself would print for `rev` — the length depends on
    /// the repository, so a test must ask rather than assume.
    fn rev_parse_short(&self, rev: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "--short", rev])
            .current_dir(&self.repo)
            .envs(hermetic_git())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn run(&self, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(&self.repo)
            .envs(hermetic_git())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert!(
            out.success,
            "`wits {}` failed: {}",
            args.join(" "),
            out.stderr
        );
        out
    }
}

/// Git env that ignores the developer's own config (notably a global
/// `core.hooksPath`, which would fire third-party hooks into these commits) while
/// still supplying the identity a commit needs.
fn hermetic_git() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@e.com"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@e.com"),
        // A local `file://` submodule is how the submodule test below builds one;
        // git refuses that transport by default (CVE-2022-39253). Real submodules
        // are https/ssh, so this only ever affects the fixture.
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "protocol.file.allow"),
        ("GIT_CONFIG_VALUE_0", "always"),
    ]
}

/// Run `wits` in an arbitrary directory — for the fixtures below, whose shapes
/// (a submodule, a bare repo's linked worktree) are not a plain clone.
fn wits_in(dir: &Path, args: &[&str]) -> Out {
    let output = Command::new(env!("CARGO_BIN_EXE_wits"))
        .args(args)
        .current_dir(dir)
        .envs(hermetic_git())
        .output()
        .unwrap();
    Out {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A repo with one commit and the given extra branches.
fn init_repo(parent: &Path, name: &str, branches: &[&str]) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.join("root"), "r").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    for branch in branches {
        git(&dir, &["branch", branch]);
    }
    dir
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
fn create_defaults_beside_the_main_worktree_and_is_idempotent() {
    let fx = Fixture::new(&["feat"]);
    let expected = fx.path("work.feat");

    let out = fx.ok(&["worktree", "create", "feat"]);
    assert!(expected.join("root").exists(), "stderr: {}", out.stderr);

    // A second run reports and succeeds rather than failing, so `create` is safe
    // in a script or a hook.
    let again = fx.ok(&["worktree", "create", "feat"]);
    assert!(
        again.stderr.contains("already exists"),
        "stderr: {}",
        again.stderr
    );
}

#[test]
fn create_accepts_an_explicit_directory_and_makes_its_parents() {
    let fx = Fixture::new(&["feat"]);
    let target = fx.path("nested/deeper/wt");

    fx.ok(&["worktree", "create", "feat", target.to_str().unwrap()]);
    assert!(target.join("root").exists(), "worktree materialised");
}

/// Creating a worktree and creating a branch are separate acts. Left to itself
/// `git worktree add` invents a local branch from a same-named remote one, or from
/// the target directory's name — so every way of asking for a branch that is not
/// there must be refused, and told what would mean it.
#[test]
fn create_never_invents_a_branch() {
    let fx = Fixture::new(&["feat"]);
    // `remote-only` exists upstream but was never given a local branch.
    git(&fx.path("up"), &["branch", "remote-only"]);
    git(&fx.repo, &["fetch", "-q"]);
    git(&fx.repo, &["tag", "v1"]);

    let remote = fx.run(&["worktree", "create", "remote-only"]);
    assert!(!remote.success);
    assert!(
        remote.stderr.contains("only as 'origin/remote-only'")
            && remote
                .stderr
                .contains("git branch remote-only origin/remote-only")
            && remote.stderr.contains("--detach"),
        "names both remedies: {}",
        remote.stderr
    );
    assert!(!fx.path("work.remote-only").exists(), "and made nothing");

    // A tag resolves, but attaching to it is impossible; say so rather than
    // detaching without being asked.
    let tag = fx.run(&["worktree", "create", "v1"]);
    assert!(!tag.success);
    assert!(tag.stderr.contains("is not a branch"), "{}", tag.stderr);

    let missing = fx.run(&["worktree", "create", "nope"]);
    assert!(!missing.success);
    assert!(
        missing.stderr.contains("does not exist"),
        "{}",
        missing.stderr
    );

    // Nothing above may have left a local branch behind.
    let branches = Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(&fx.repo)
        .envs(hermetic_git())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&branches.stdout);
    assert!(!branches.contains("remote-only"), "branches: {branches}");
    assert!(!branches.contains("nope"), "branches: {branches}");
}

/// `--detach` is how any non-branch revision gets checked out — a tag, a commit,
/// or someone else's remote branch — without adding a local branch.
#[test]
fn detach_takes_any_revision_and_adds_no_branch() {
    let fx = Fixture::new(&["feat"]);
    git(&fx.path("up"), &["branch", "theirs"]);
    git(&fx.repo, &["fetch", "-q"]);
    git(&fx.repo, &["tag", "v1"]);

    for rev in ["v1", "origin/theirs"] {
        let out = fx.ok(&["worktree", "create", "--detach", rev]);
        assert!(out.success, "stderr: {}", out.stderr);
    }
    assert!(fx.path("work.v1").join("root").exists());
    assert!(fx.path("work.origin_theirs").join("root").exists());

    let branches = Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .current_dir(&fx.repo)
        .envs(hermetic_git())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&branches.stdout);
    assert!(
        !branches.contains("theirs"),
        "no local branch was added: {branches}"
    );

    // A revision that resolves to nothing is still refused.
    let out = fx.run(&["worktree", "create", "--detach", "nope"]);
    assert!(!out.success);
    assert!(
        out.stderr.contains("does not name an existing"),
        "{}",
        out.stderr
    );
}

/// `switch` moves a worktree that already exists; naming none means the one you
/// are standing in.
#[test]
fn switch_moves_an_existing_worktree() {
    let fx = Fixture::new(&["feat", "other"]);
    fx.ok(&["worktree", "create", "feat"]);
    let wt = fx.path("work.feat");
    let branch_of = |dir: &Path| {
        let out = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir)
            .envs(hermetic_git())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };

    // Named by its directory, which stays stable across switches (its *branch*
    // does not — that is what is being changed).
    fx.ok(&["worktree", "switch", "other", "work.feat"]);
    assert_eq!(branch_of(&wt), "other");

    // From inside it, with no target.
    let out = wits_in(&wt, &["worktree", "switch", "feat"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert_eq!(branch_of(&wt), "feat");

    // The same guards as everywhere: an unknown target, a branch that is not
    // there, and a tree holding uncommitted work.
    let unknown = fx.run(&["worktree", "switch", "other", "nonexistent"]);
    assert!(!unknown.success);
    assert!(unknown.stderr.contains("no worktree matches"));

    let invented = fx.run(&["worktree", "switch", "brand-new", "work.feat"]);
    assert!(!invented.success);
    assert!(
        invented.stderr.contains("does not exist"),
        "{}",
        invented.stderr
    );
    assert_eq!(branch_of(&wt), "feat", "a refusal moves nothing");

    std::fs::write(wt.join("root"), "uncommitted").unwrap();
    let dirty = fx.run(&["worktree", "switch", "other", "work.feat"]);
    assert!(!dirty.success);
    assert!(
        dirty.stderr.contains("uncommitted changes"),
        "{}",
        dirty.stderr
    );
    assert_eq!(branch_of(&wt), "feat", "and buries nothing");
}

/// A slug replaces path separators, so a `feature/x` branch still names one
/// directory instead of burying the worktree a level down.
#[test]
fn a_slashed_branch_becomes_one_directory() {
    let fx = Fixture::new(&["feature/x"]);
    fx.ok(&["worktree", "create", "feature/x"]);
    assert!(fx.path("work.feature_x").join("root").exists());
}

/// A relative directory must land beside the caller's cwd, not beside the main
/// worktree — `git worktree add` is driven from the latter, so the path has to be
/// resolved before it gets there.
#[test]
fn a_relative_directory_resolves_against_the_callers_cwd() {
    let fx = Fixture::new(&["feat"]);
    // Run from a linked worktree, where "beside the main worktree" and "beside
    // me" are different places.
    fx.ok(&["worktree", "create", "feat"]);
    let from = fx.path("work.feat");

    // A detached `HEAD` rather than a branch, since git allows one branch in only
    // one worktree and every branch here is already taken.
    let out = Command::new(env!("CARGO_BIN_EXE_wits"))
        .args(["worktree", "create", "--detach", "HEAD", "relwt"])
        .current_dir(&from)
        .envs(hermetic_git())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        from.join("relwt").join("root").exists(),
        "landed beside the caller, not at {}",
        fx.repo.join("relwt").display()
    );
    assert!(!fx.repo.join("relwt").exists());
}

/// Run from a *linked* worktree, both the main worktree and the one you are
/// standing in must stay immune — the first because it is the repository, the
/// second because removing it would delete the ground under your shell.
#[test]
fn main_and_current_stay_immune_when_invoked_from_a_linked_worktree() {
    let fx = Fixture::new(&["feat", "other"]);
    fx.ok(&["worktree", "create", "feat"]);
    fx.ok(&["worktree", "create", "other"]);
    let from = fx.path("work.feat");

    let run_there = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(&from)
            .envs(hermetic_git())
            .output()
            .unwrap()
    };

    let out = run_there(&["worktree", "info", "--long"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout
            .matches("never — this is the repository itself")
            .count(),
        1,
        "exactly one worktree is the main one: {stdout}"
    );
    assert_eq!(
        stdout.matches("never — you are in it").count(),
        1,
        "exactly one worktree is the current one: {stdout}"
    );

    // Every worktree here is merged, so an unguarded sweep would take all three.
    let swept = run_there(&["worktree", "prune"]);
    assert!(
        swept.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&swept.stderr)
    );
    assert!(fx.repo.join("root").exists(), "the main worktree survives");
    assert!(from.join("root").exists(), "the current worktree survives");
    assert!(
        !fx.path("work.other").exists(),
        "…while the third one is still reclaimed"
    );

    // Naming it explicitly is refused too, with a message that says what to do.
    let named = run_there(&["worktree", "prune", "feat"]);
    assert!(!named.status.success());
    assert!(String::from_utf8_lossy(&named.stderr).contains("worktree you are in"));
}

/// Naming a target and filtering the set are different questions, so asking both
/// at once is refused rather than silently resolved one way.
#[test]
fn a_target_and_a_filter_are_mutually_exclusive() {
    let fx = Fixture::new(&["feat"]);
    fx.ok(&["worktree", "create", "feat"]);
    for args in [
        vec!["worktree", "info", "feat", "--merged"],
        vec!["worktree", "prune", "feat", "--older-than", "30d"],
    ] {
        let out = fx.run(&args);
        assert!(!out.success, "`wits {}` should be refused", args.join(" "));
        assert!(out.stderr.contains("cannot be used with"), "{}", out.stderr);
    }
}

#[test]
fn info_lists_worktrees_and_resolves_a_target_three_ways() {
    let fx = Fixture::new(&["feat"]);
    fx.ok(&["worktree", "create", "feat"]);
    let wt = fx.path("work.feat");

    let list = fx.ok(&["worktree", "info"]);
    // The repository block comes first, and describes the repo rather than any
    // worktree: where its git dir is, and what `merged` is measured against.
    let (block, table) = list
        .stdout
        .split_once("\n\n")
        .expect("a blank line splits them");
    assert!(block.contains("repository"), "block: {block}");
    assert!(block.contains(".git"), "block names the git dir: {block}");
    assert!(block.contains("trunk       origin/main"), "block: {block}");
    assert!(
        !block.contains("work.feat"),
        "no worktree leaks in: {block}"
    );

    assert!(table.contains("BRANCH"), "a header row: {table}");
    // Run from the main worktree, so it is both the repository and where we are.
    assert!(table.contains("main, current"), "table: {table}");
    assert!(table.contains("work.feat"));

    // Path, branch, and directory name are all valid handles; `--path` is the
    // single-value form `cd "$(...)"` uses.
    for target in ["feat", "work.feat", wt.to_str().unwrap()] {
        let out = fx.ok(&["worktree", "info", target, "--path"]);
        assert_eq!(
            out.stdout.trim(),
            wt.to_str().unwrap(),
            "resolving '{target}'"
        );
    }

    let unknown = fx.run(&["worktree", "info", "nope"]);
    assert!(!unknown.success);
    assert!(unknown.stderr.contains("no worktree matches"));
}

/// Naming one worktree gives its panel, not a one-row table: a row per fact,
/// each labelled, and no repository block (the question was about the worktree).
#[test]
fn info_on_one_target_prints_a_panel() {
    let fx = Fixture::new(&["feat"]);
    fx.ok(&["worktree", "create", "feat"]);
    fx.diverge(&fx.path("work.feat"));

    let out = fx.ok(&["worktree", "info", "feat"]);
    assert!(
        !out.stdout.contains("BRANCH"),
        "not a table: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("repository"),
        "no block: {}",
        out.stdout
    );
    for expected in [
        "branch      feat",
        "trunk       1 ahead of origin/main",
        "changes     clean",
        "tracking    origin/feat",
        "prune       kept — 1 commit not on origin/main",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing `{expected}` in:\n{}",
            out.stdout
        );
    }
    // The commit is identified by hash, abbreviated the way git would.
    let short = fx.rev_parse_short("feat");
    assert!(
        out.stdout.contains(&short),
        "head hash {short}: {}",
        out.stdout
    );
}

/// A row only appears when it has something to say, so a bare repository — which
/// has no working tree, no HEAD and nothing to date — shows no `head` or `trunk`
/// row and says why `changes` is empty rather than claiming it is clean.
#[test]
fn a_panel_omits_rows_that_have_nothing_to_say() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let inner = init_repo(root, "inner", &[]);
    git(
        root,
        &["clone", "-q", "--bare", inner.to_str().unwrap(), "b.git"],
    );

    let out = wits_in(&root.join("b.git"), &["worktree", "info", "--long"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("(bare)"), "{}", out.stdout);
    assert!(
        out.stdout.contains("changes     (bare — no working tree)"),
        "{}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("  head "),
        "no head row: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("  trunk "),
        "no trunk row: {}",
        out.stdout
    );
    // A bare clone publishes no origin/HEAD, so the block says so outright rather
    // than leaving the reader to wonder why nothing reads as merged.
    assert!(out.stdout.contains("trunk       (none"), "{}", out.stdout);
}

/// `--long` is the same listing in panel form: the repository block, then every
/// worktree expanded.
#[test]
fn long_expands_every_worktree_into_a_panel() {
    let fx = Fixture::new(&["feat", "other"]);
    fx.ok(&["worktree", "create", "feat"]);
    fx.ok(&["worktree", "create", "other"]);

    let out = fx.ok(&["worktree", "info", "--long"]);
    assert!(out.stdout.contains("repository"), "{}", out.stdout);
    assert!(
        !out.stdout.contains("BRANCH"),
        "not a table: {}",
        out.stdout
    );
    assert_eq!(
        out.stdout.matches("  branch      ").count(),
        3,
        "one panel per worktree: {}",
        out.stdout
    );
}

/// The default sweep takes work that demonstrably landed and leaves everything
/// else — including a branch that has diverged, and a tree holding uncommitted
/// changes.
#[test]
fn prune_sweeps_merged_worktrees_and_keeps_the_rest() {
    let fx = Fixture::new(&["landed", "diverged", "busy"]);
    for branch in ["landed", "diverged", "busy"] {
        fx.ok(&["worktree", "create", branch]);
    }
    fx.diverge(&fx.path("work.diverged"));
    std::fs::write(fx.path("work.busy").join("root"), "uncommitted").unwrap();
    // Ignored build output must not count as uncommitted work, or a sweep would
    // never reclaim anything anyone had built in.
    std::fs::create_dir_all(fx.path("work.landed").join("_build")).unwrap();
    std::fs::write(fx.path("work.landed").join("_build/o"), "obj").unwrap();

    let out = fx.ok(&["worktree", "prune"]);
    assert!(!fx.path("work.landed").exists(), "stderr: {}", out.stderr);
    assert!(
        fx.path("work.diverged").exists(),
        "unmerged work is not swept"
    );
    assert!(
        fx.path("work.busy").exists(),
        "a dirty worktree is kept, not deleted"
    );
    assert!(out.stderr.contains("uncommitted changes"), "and says why");
    assert!(fx.repo.join("root").exists(), "the main worktree survives");
}

/// Dormancy is never implied: only `--older-than` may select on it.
#[test]
fn dormancy_is_opt_in() {
    let fx = Fixture::new(&["diverged"]);
    fx.ok(&["worktree", "create", "diverged"]);
    fx.diverge(&fx.path("work.diverged"));

    // A bare sweep leaves it: it has not landed, and age alone is not evidence.
    fx.ok(&["worktree", "prune"]);
    assert!(fx.path("work.diverged").exists());

    // Asked for explicitly, a cutoff its commit predates selects it.
    let preview = fx.ok(&["worktree", "info", "--older-than", "2999-01-01"]);
    assert!(preview.stdout.contains("dormant"), "{}", preview.stdout);
    fx.ok(&["worktree", "prune", "--older-than", "2999-01-01"]);
    assert!(!fx.path("work.diverged").exists());
}

#[test]
fn a_named_target_is_dropped_whatever_its_state_but_never_silently() {
    let fx = Fixture::new(&["diverged"]);
    fx.ok(&["worktree", "create", "diverged"]);
    let wt = fx.path("work.diverged");
    fx.diverge(&wt);

    // Naming an unmerged worktree drops it — the "I'm done with this" path.
    fx.ok(&["worktree", "prune", "diverged"]);
    assert!(!wt.exists());

    // But uncommitted work is an error, not a skip: this one was asked for by
    // name, so silence would be the wrong answer.
    fx.ok(&["worktree", "create", "diverged"]);
    std::fs::write(wt.join("root"), "uncommitted").unwrap();
    let refused = fx.run(&["worktree", "prune", "diverged"]);
    assert!(!refused.success);
    assert!(refused.stderr.contains("uncommitted changes"));
    assert!(wt.exists(), "refusing must not have deleted it");

    fx.ok(&["worktree", "prune", "diverged", "--force"]);
    assert!(!wt.exists(), "--force discards the changes as advertised");
}

#[test]
fn the_main_worktree_is_never_reclaimable() {
    let fx = Fixture::new(&[]);
    let out = fx.run(&["worktree", "prune", "main"]);
    assert!(!out.success);
    assert!(out.stderr.contains("main worktree"));
    assert!(fx.repo.join("root").exists());
}

/// `git worktree prune`'s job, folded in: a directory someone deleted by hand
/// leaves a record nothing else cleans up.
#[test]
fn prune_forgets_records_of_hand_deleted_directories() {
    let fx = Fixture::new(&["stale"]);
    let gone = fx.path("gonedir");
    fx.ok(&["worktree", "create", "stale", gone.to_str().unwrap()]);
    std::fs::remove_dir_all(&gone).unwrap();

    // "records only" is exclusive: with the directory gone, no other tag bears on
    // what happens to the entry, and listing `merged` beside it would invite the
    // reader to expect a sweep to remove something that is already absent.
    let before = fx.ok(&["worktree", "info"]);
    let stale_row = before
        .stdout
        .lines()
        .find(|line| line.contains("gonedir"))
        .unwrap_or_default();
    assert!(stale_row.contains("records only"), "{}", before.stdout);
    assert!(!stale_row.contains("merged"), "exclusive: {stale_row}");

    let out = fx.ok(&["worktree", "prune"]);
    assert!(out.stderr.contains("stale worktree record"));
    let after = fx.ok(&["worktree", "info"]);
    assert!(
        !after.stdout.contains("gonedir"),
        "the record is forgotten: {}",
        after.stdout
    );
}

/// A dry run must plan without touching anything, and must not claim otherwise.
#[test]
fn dry_run_changes_nothing_and_says_would() {
    let fx = Fixture::new(&["landed"]);
    fx.ok(&["worktree", "create", "landed"]);

    let out = fx.ok(&["-n", "worktree", "prune"]);
    assert!(fx.path("work.landed").exists(), "nothing was removed");
    assert!(
        out.stderr.contains("would remove"),
        "the log describes an intention: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("removed worktree"),
        "and never claims to have done it: {}",
        out.stderr
    );
    assert!(out.stdout.contains("[DRY-RUN]"), "the plan lands on stdout");

    // Creating is planned the same way.
    let create = fx.ok(&[
        "-n",
        "worktree",
        "create",
        "landed",
        "/tmp/never-made-by-wits",
    ]);
    assert!(!Path::new("/tmp/never-made-by-wits").exists());
    assert!(create.stderr.contains("would create"), "{}", create.stderr);
}

/// The applet list meson installs symlinks for must match the binary's own idea
/// of its built-ins, which is the cross-check `wits __applets` exists for.
/// "One bare clone, a worktree per branch" is a normal worktree setup, so a bare
/// repository must work — and the repository itself must stay immune.
#[test]
fn a_bare_repository_is_a_valid_place_to_work() {
    let fx = Fixture::new(&["a"]);
    let bare = fx.path("bare.git");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            "--bare",
            fx.path("up").to_str().unwrap(),
            "bare.git",
        ],
    );

    let run_bare = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(&bare)
            .envs(hermetic_git())
            .output()
            .unwrap()
    };

    let created = run_bare(&["worktree", "create", "a"]);
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(fx.path("bare.git.a").join("root").exists());

    // Bare is shown as bare, not conflated with a detached HEAD…
    let listed = run_bare(&["worktree", "info"]);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("(bare)"), "{stdout}");
    assert!(!stdout.contains("(detached)"), "{stdout}");

    // …and it is never a sweep candidate.
    assert!(run_bare(&["worktree", "prune"]).status.success());
    assert!(bare.join("HEAD").exists(), "the bare repository survives");
}

/// Inside a repository that is itself a **submodule**, `git worktree list`
/// reports the main entry's *git-dir* (`<super>/.git/modules/<name>`) rather than
/// its working tree. Reporting that would send anyone who followed the path into
/// `.git`, and would put a new worktree there too.
#[test]
fn a_submodules_main_worktree_is_its_working_tree_not_its_git_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let inner = init_repo(root, "inner", &["feat"]);
    let super_ = init_repo(root, "super", &[]);
    git(
        &super_,
        &[
            "submodule",
            "add",
            "-q",
            &format!("file://{}", inner.display()),
            "sub",
        ],
    );
    git(&super_, &["commit", "-q", "-m", "add sub"]);
    let sub = super_.join("sub");

    let out = wits_in(&sub, &["worktree", "info", "--path"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim(),
        sub.to_str().unwrap(),
        "the main worktree is the submodule's working tree"
    );
    assert!(
        !out.stdout.contains(".git"),
        "never the git-dir: {}",
        out.stdout
    );

    // And a new worktree lands beside that working tree, not inside `.git`.
    // `submodule add` clones, so the branch is remote-only until asked for.
    git(&sub, &["branch", "feat", "origin/feat"]);
    let created = wits_in(&sub, &["worktree", "create", "feat"]);
    assert!(created.success, "stderr: {}", created.stderr);
    assert!(super_.join("sub.feat").join("root").exists());
    assert!(
        !super_.join(".git/modules/sub.feat").exists(),
        "nothing may be created under .git/modules"
    );
}

/// Seen from a linked worktree of a **bare** repo, the repository is the bare
/// git-dir. `rev-parse --is-bare-repository` reports `false` from there, so only
/// the common config settles it — and getting it wrong would anchor new worktrees
/// to the directory merely *containing* the repository.
#[test]
fn a_bare_repo_anchors_correctly_from_one_of_its_linked_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let inner = init_repo(root, "inner", &["feat"]);
    git(
        root,
        &["clone", "-q", "--bare", inner.to_str().unwrap(), "b.git"],
    );
    let bare = root.join("b.git");
    let linked = root.join("bwt");
    git(
        &bare,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "feat"],
    );

    let listed = wits_in(&linked, &["worktree", "info"]);
    assert!(listed.success, "stderr: {}", listed.stderr);
    assert!(listed.stdout.contains("(bare)"), "{}", listed.stdout);

    // The default location is beside the bare repo, not beside its parent.
    let created = wits_in(&linked, &["worktree", "create", "main"]);
    assert!(created.success, "stderr: {}", created.stderr);
    assert!(
        root.join("b.git.main").join("root").exists(),
        "landed beside b.git, not at {}",
        root.display()
    );
}

#[test]
fn a_directory_that_is_not_a_repository_is_refused() {
    let fx = Fixture::new(&[]);
    let plain = fx.path("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_wits"))
        .args(["worktree", "info"])
        .current_dir(&plain)
        .envs(hermetic_git())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a git repository"));
}

#[test]
fn worktree_is_a_registered_builtin() {
    let fx = Fixture::new(&[]);
    let out = fx.ok(&["__applets"]);
    assert!(
        out.stdout.lines().any(|line| line == "worktree"),
        "applets: {}",
        out.stdout
    );
}
