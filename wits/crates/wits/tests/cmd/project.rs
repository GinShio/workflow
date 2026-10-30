//! Black-box tests for the shared-component half of `wits project` / `wits
//! update`: a repo **borrowed** from another project (`from`) and the paths a
//! checkout **skips**.
//!
//! These drive the real binary against real git because that is where the
//! behaviour actually lives. `skip` is two git mechanisms whose *order* matters
//! — `sparse-checkout` alone cannot mask a materialised submodule, and only
//! `deinit` first makes it work — and the verification is deliberately
//! behavioural (does the path exist, is the index entry `skip-worktree`) rather
//! than a comparison of pattern text. Neither is something a mock would tell the
//! truth about.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wits_core::git::Repository;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The config root every command reads (`WITS_PROJECT_CONFIG`).
    config: PathBuf,
}

struct Out {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Git env that ignores the developer's own config while still supplying a commit
/// identity, and allows the `file://` transport a local submodule fixture needs
/// (git refuses it by default, CVE-2022-39253; real submodules are https/ssh).
fn hermetic_git() -> Vec<(&'static str, String)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_SYSTEM", "/dev/null".into()),
        ("GIT_AUTHOR_NAME", "T".into()),
        ("GIT_AUTHOR_EMAIL", "t@e.com".into()),
        ("GIT_COMMITTER_NAME", "T".into()),
        ("GIT_COMMITTER_EMAIL", "t@e.com".into()),
        ("GIT_CONFIG_COUNT", "1".into()),
        ("GIT_CONFIG_KEY_0", "protocol.file.allow".into()),
        ("GIT_CONFIG_VALUE_0", "always".into()),
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(hermetic_git())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Fixture {
    /// An "upstream" pair — a `work` component and a `wrap` that carries it as a
    /// submodule at `nested/work` — plus a config root declaring `work` as its own
    /// project and `wrap` borrowing it, with the submodule path skipped.
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();

        let work_up = Fixture::seed(&root, "up-work", &["component.c"]);
        let wrap_up = Fixture::seed(&root, "up-wrap", &["top.c"]);
        // The wrap also owns a plain directory we can skip, to cover the
        // non-submodule half of the same field.
        std::fs::create_dir_all(wrap_up.join("vendor/blob")).unwrap();
        std::fs::write(wrap_up.join("vendor/blob/big.bin"), "x").unwrap();
        std::fs::write(wrap_up.join("vendor/keep.c"), "k").unwrap();
        git(&wrap_up, &["add", "-A"]);
        git(&wrap_up, &["commit", "-q", "-m", "vendor"]);
        git(
            &wrap_up,
            &[
                "submodule",
                "add",
                "-q",
                work_up.to_str().unwrap(),
                "nested/work",
            ],
        );
        git(&wrap_up, &["commit", "-q", "-m", "add submodule"]);

        let wrap_dir = root.join("src-wrap");
        Fixture::declare_work(&root, "");
        std::fs::write(
            config.join("wrap.toml"),
            format!(
                r#"
[project]
focus = "component"
[repos.main]
path = "{}"
main_branch = "main"
build_dir = "{{{{repos.main.workdir}}}}/_build/{{{{branch.slug}}}}-{{{{build_type}}}}"
skip = ["/nested/work", "/vendor", "!/vendor/keep.c"]
[repos.main.remotes]
origin = "{}"
[repos.component]
from = "work"
anchor = "main"
"#,
                wrap_dir.display(),
                wrap_up.display()
            ),
        )
        .unwrap();

        Fixture {
            _dir: dir,
            root,
            config,
        }
    }

    /// (Re)write the component project's registry file. `extra` is appended to its
    /// `[repos.main]`, which is how a test adds a declaration the shared fixture
    /// does not carry: every command reads the registry fresh, so the rewritten
    /// value is in force from the next run.
    fn declare_work(root: &Path, extra: &str) {
        std::fs::write(
            root.join("config/work.toml"),
            format!(
                r#"
[project]
[repos.main]
path = "{}"
main_branch = "main"
build_dir = "{{{{repos.main.workdir}}}}/_build/{{{{branch.slug}}}}-{{{{build_type}}}}"
{extra}
[repos.main.remotes]
origin = "{}"
"#,
                root.join("src-work").display(),
                root.join("up-work").display()
            ),
        )
        .unwrap();
    }

    /// A repo with one commit per named file.
    fn seed(parent: &Path, name: &str, files: &[&str]) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        for file in files {
            std::fs::write(dir.join(file), "c").unwrap();
        }
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "c1"]);
        dir
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(cwd)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", &self.config)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Like [`run`], with extra environment for the child. Process-global inputs
    /// such as `WITS_PROJECT_TOOLCHAIN` are only testable at the process
    /// boundary; setting them in-process would race every other test.
    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_wits"))
            .args(args)
            .current_dir(&self.root)
            .envs(hermetic_git())
            .env("WITS_PROJECT_CONFIG", &self.config)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Out {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_in(&self.root, args)
    }

    fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert!(
            out.success,
            "`wits {}` failed:\n{}\n{}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        out
    }
}

/// The index tag `git ls-files -t` gives a path — `S` for skip-worktree, which is
/// what a fully realised `skip` looks like from git's side.
fn index_tag(repo: &Path, path: &str) -> Option<char> {
    git_out(repo, &["ls-files", "-t", "--", path])
        .lines()
        .next()
        .and_then(|l| l.chars().next())
}

#[test]
fn project_exists_requires_a_cloned_main_repository() {
    let fx = Fixture::new();

    let absent = fx.run(&["project", "exists", "work"]);
    assert!(!absent.success);
    assert!(absent.stdout.is_empty());
    assert!(absent.stderr.contains("repos.main is not cloned"));

    std::fs::create_dir_all(fx.path("src-work")).unwrap();
    let plain_dir = fx.run(&["project", "exists", "work"]);
    assert!(!plain_dir.success, "a plain directory counted as a clone");
    std::fs::remove_dir_all(fx.path("src-work")).unwrap();

    fx.ok(&["update", "work"]);
    let present = fx.run(&["project", "exists", "work"]);
    assert!(present.success, "{}", present.stderr);
    assert!(present.stdout.is_empty());
    assert!(present.stderr.is_empty());

    let unregistered = fx.run(&["project", "exists", "missing"]);
    assert!(!unregistered.success);
    assert!(unregistered.stdout.is_empty());
    assert!(unregistered.stderr.contains("no project 'missing'"));
}

#[test]
fn project_exists_accepts_bare_clones_but_not_subdirectories() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);

    let nested = fx.path("src-work/not-a-repository");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        fx.config.join("nested.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{}"
main_branch = "main"
"#,
            nested.display()
        ),
    )
    .unwrap();
    let nested_result = fx.run(&["project", "exists", "nested"]);
    assert!(
        !nested_result.success,
        "a directory inside another checkout counted as the configured clone"
    );

    let bare = fx.path("bare.git");
    git(&fx.root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    std::fs::write(
        fx.config.join("bare.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{}"
main_branch = "main"
"#,
            bare.display()
        ),
    )
    .unwrap();
    let bare_result = fx.run(&["project", "exists", "bare"]);
    assert!(bare_result.success, "{}", bare_result.stderr);
    assert!(bare_result.stdout.is_empty());
    assert!(bare_result.stderr.is_empty());
}

#[test]
fn update_clones_a_wrap_with_its_skipped_paths_never_materialised() {
    let fx = Fixture::new();
    fx.ok(&["update", "wrap"]);

    let wrap = fx.path("src-wrap");
    assert!(
        wrap.join("top.c").exists(),
        "the wrap itself is checked out"
    );

    // The skipped submodule: absent, and marked so rather than merely missing.
    assert!(
        !wrap.join("nested/work").exists(),
        "skipped submodule was materialised"
    );
    assert_eq!(index_tag(&wrap, "nested/work"), Some('S'));

    // The skipped plain directory, and the `!` re-include inside it.
    assert!(!wrap.join("vendor/blob/big.bin").exists());
    assert!(
        wrap.join("vendor/keep.c").exists(),
        "a `!` entry must re-include even under a skipped parent"
    );

    // Nothing about this leaves the tree looking modified.
    assert_eq!(git_out(&wrap, &["status", "--porcelain"]).trim(), "");
}

/// A deferred (`--no-checkout`) clone has to be materialised explicitly: writing
/// the sparse patterns does not do it, and leaves every path staged as a deletion.
/// The declared main branch normally supplies the target, so this pins the case
/// that has none — reachable, since the loader tolerates a missing `main_branch`.
#[test]
fn a_skipping_repo_without_a_main_branch_still_gets_a_working_tree() {
    let fx = Fixture::new();
    std::fs::write(
        fx.config.join("nomb.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{}"
skip = ["/vendor"]
[repos.main.remotes]
origin = "{}"
"#,
            fx.path("src-nomb").display(),
            fx.path("up-wrap").display()
        ),
    )
    .unwrap();

    fx.ok(&["update", "nomb"]);
    let repo = fx.path("src-nomb");
    assert!(repo.join("top.c").exists(), "working tree was never filled");
    assert!(!repo.join("vendor").exists(), "skipped path materialised");
    assert_eq!(git_out(&repo, &["status", "--porcelain"]).trim(), "");
}

/// A `clone` hook may narrow the checkout to a sparse cone of its own (the
/// bootstrap scripts of a monorepo do). `sparse-checkout set` replaces the whole
/// pattern list, so applying `skip` on top would *widen* that checkout to the
/// entire tree — and cone mode cannot hold a `!` exclusion in any case. Refuse
/// loudly instead.
#[test]
fn skip_refuses_to_overwrite_sparse_patterns_it_did_not_write() {
    let fx = Fixture::new();
    let repo = fx.path("src-hooked");
    let up = fx.path("up-wrap");
    // A `clone` hook standing in for a bootstrap script: it clones, then sets its
    // own cone — deliberately excluding `nested` while keeping `vendor`.
    std::fs::write(
        fx.config.join("hooked.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{repo}"
main_branch = "main"
skip = ["/vendor"]
[repos.main.remotes]
origin = "{up}"
[repos.main.hooks]
clone = "git clone -q {up} {repo} && git -C {repo} sparse-checkout set --cone vendor"
"#,
            repo = repo.display(),
            up = up.display()
        ),
    )
    .unwrap();

    let out = fx.run(&["update", "hooked"]);
    assert!(!out.success, "wits widened a deliberately narrow checkout");
    assert!(
        out.stderr.contains("would replace them"),
        "stderr: {}",
        out.stderr
    );
    // The hook's own cone is intact: nothing was rewritten before the refusal.
    assert!(
        git_out(&repo, &["sparse-checkout", "list"]).contains("vendor"),
        "the hook's patterns were disturbed"
    );
}

#[test]
fn update_leaves_a_borrowed_repo_to_its_owner_until_asked() {
    let fx = Fixture::new();
    fx.ok(&["update", "wrap"]);
    assert!(
        !fx.path("src-work").exists(),
        "a borrowed repo is the owning project's to clone"
    );

    fx.ok(&["update", "wrap", "--with-borrowed"]);
    assert!(fx.path("src-work").join("component.c").exists());
}

#[test]
fn a_skipped_path_that_is_materialised_fails_check_and_update() {
    let fx = Fixture::new();
    // Clone the way something other than wits would: everything materialised.
    let wrap = fx.path("src-wrap");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            "--recurse-submodules",
            fx.path("up-wrap").to_str().unwrap(),
            wrap.to_str().unwrap(),
        ],
    );
    assert!(wrap.join("nested/work").exists(), "fixture precondition");

    let check = fx.run(&["project", "check", "wrap"]);
    assert!(!check.success, "check passed on a contradicted skip");
    assert!(
        check.stderr.contains("nested/work"),
        "the offending path is named: {}",
        check.stderr
    );

    let update = fx.run(&["update", "wrap"]);
    assert!(!update.success, "update ran against a contradicted skip");
    assert!(
        update.stderr.contains("not in force"),
        "stderr: {}",
        update.stderr
    );

    // Without -v the error stays terse; with it, the fix is spelled out — the
    // destructive step is ours to run, never wits's.
    assert!(!update.stderr.contains("submodule deinit"));
    let verbose = fx.run(&["-v", "update", "wrap"]);
    assert!(
        verbose.stderr.contains("submodule deinit")
            && verbose.stderr.contains("sparse-checkout set"),
        "stderr: {}",
        verbose.stderr
    );
}

#[test]
fn the_owner_answers_for_a_shared_checkout() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    let work = fx.path("src-work");

    // Standing in the shared component resolves to the project that *is* it, not
    // to a project that merely borrows it.
    let out = fx.run_in(&work, &["project", "info", "--get", "repo.main_branch"]);
    assert!(out.success, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "main");

    let described = fx.run_in(&work, &["project", "info", "."]);
    assert!(
        described.stdout.contains("project: work"),
        "stdout: {}",
        described.stdout
    );
}

/// The counterpart to [`the_owner_answers_for_a_shared_checkout`]: the owner
/// saying which project the shared checkout is *being developed as* moves the
/// path queries to it, borrow and all. `wrap` focuses on the component it
/// borrows, so its build is keyed on that component's branch — which is exactly
/// the case the owner's answer gets wrong for anyone working on `wrap`.
#[test]
fn developed_as_answers_for_a_shared_checkout_instead_of_its_owner() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    let work = fx.path("src-work");
    let owns = |out: &Out, rel: &str| {
        let want = fx.path(rel);
        out.stdout.trim().starts_with(want.to_str().unwrap())
    };

    let query = ["project", "info", "--get", "build_dir"];
    let default = fx.run_in(&work, &query);
    assert!(default.success, "stderr: {}", default.stderr);
    assert!(owns(&default, "src-work"), "stdout: {}", default.stdout);

    Fixture::declare_work(&fx.root, r#"developed_as = "wrap""#);
    let redirected = fx.run_in(&work, &query);
    assert!(redirected.success, "stderr: {}", redirected.stderr);
    assert!(
        owns(&redirected, "src-wrap"),
        "stdout: {}",
        redirected.stdout
    );
    // The redirect is the whole registry's answer, so `check` has nothing to say
    // about the declaration the query just followed. (It still reports this
    // fixture's unrelated gaps, so the absence of a complaint is the assertion.)
    let accepted = fx.run(&["project", "check"]);
    assert!(
        !accepted.stderr.contains("developed_as"),
        "stderr: {}",
        accepted.stderr
    );

    // A value naming no known project must not break every query that passes
    // through path resolution — it degrades to the structural answer, and the
    // report of it belongs to `check`.
    Fixture::declare_work(&fx.root, r#"developed_as = "no-such-project""#);
    let stale = fx.run_in(&work, &query);
    assert!(stale.success, "stderr: {}", stale.stderr);
    assert!(owns(&stale, "src-work"), "stdout: {}", stale.stdout);

    let checked = fx.run(&["project", "check"]);
    assert!(!checked.success);
    assert!(
        checked
            .stderr
            .contains("developed_as 'no-such-project' names no known project"),
        "stderr: {}",
        checked.stderr
    );

    // A real project that does not borrow this repo is the other way the value
    // can resolve to nothing, and reads differently in the report.
    Fixture::declare_work(&fx.root, r#"developed_as = "work""#);
    let unrelated = fx.run(&["project", "check"]);
    assert!(
        unrelated
            .stderr
            .contains("developed_as 'work' does not borrow this repo"),
        "stderr: {}",
        unrelated.stderr
    );
}

/// A branch of a shared checkout identifies a build tree in *every* project that
/// focuses on it, so the deletion query has to cross project boundaries — and
/// expand the build-type axis, since a branch normally has more than one tree.
#[test]
fn branch_build_dirs_span_every_project_the_branch_identifies() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    let work = fx.path("src-work");

    // Only directories that exist are reported: a cleanup acts on what is there.
    let present = [
        "src-work/_build/main-debug",
        "src-wrap/_build/main-debug",
        "src-wrap/_build/main-release",
    ];
    for rel in present {
        std::fs::create_dir_all(fx.path(rel)).unwrap();
    }
    // Trees that must survive: another branch entirely, and — the dangerous one —
    // a branch whose slug *starts with* the deleted one's, which a plain prefix
    // match would sweep in.
    for rel in [
        "src-wrap/_build/other-debug",
        "src-wrap/_build/main-extra-debug",
        "src-work/_build/main-extra-release",
    ] {
        std::fs::create_dir_all(fx.path(rel)).unwrap();
    }

    let out = fx.run_in(&work, &["project", "branch-build-dirs", "--branch", "main"]);
    assert!(out.success, "stderr: {}", out.stderr);
    for rel in present {
        assert!(
            out.stdout.contains(fx.path(rel).to_str().unwrap()),
            "{rel} missing from:\n{}",
            out.stdout
        );
    }
    for survivor in ["other-debug", "main-extra-debug", "main-extra-release"] {
        assert!(
            !out.stdout.contains(survivor),
            "{survivor} would have been swept in:\n{}",
            out.stdout
        );
    }
    assert_eq!(out.stdout.lines().count(), present.len(), "{}", out.stdout);
}

/// `info` is the whole picture, so it reports the facts that are invisible in a
/// path: where a repo's identity came from, what its checkout leaves out, and —
/// side by side — the declared templates and what they resolve to. Assertions
/// match label and value on one line rather than exact columns, so re-aligning
/// the report is not a test change.
#[test]
fn info_reports_the_borrow_the_skip_list_and_both_template_and_resolution() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    let out = fx.ok(&["project", "info", "wrap"]);
    let has = |label: &str, value: &str| {
        out.stdout
            .lines()
            .any(|l| l.contains(label) && l.contains(value))
    };

    assert!(has("borrowed from", "work"), "stdout: {}", out.stdout);
    assert!(has("skip", "/nested/work"), "stdout: {}", out.stdout);

    // The declared template and its resolution are both present, and they are
    // different text — showing only one of them would hide the other's bugs.
    assert!(out.stdout.contains("templates:"), "stdout: {}", out.stdout);
    assert!(has("build_dir", "{{"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("resolved ("), "stdout: {}", out.stdout);

    // The three repo roles are reported separately: they coincide here, and the
    // point is that a reader can see that rather than having to assume it.
    for role in ["focus:", "build_repo:", "identity_repo:", "strategy:"] {
        assert!(out.stdout.contains(role), "{role} missing: {}", out.stdout);
    }
}

/// A target that names a path describes the repo containing that path, not the
/// project's focus. `wrap` focuses `component`, so running from its `main`
/// checkout is the discriminating case: the description and `--get` must agree.
#[test]
fn project_info_uses_the_path_derived_repo_not_the_project_focus() {
    let fx = Fixture::new();
    fx.ok(&["update", "wrap"]);
    let main = fx.path("src-wrap");

    let described = fx.run_in(&main, &["project", "info"]);
    assert!(described.success, "stderr: {}", described.stderr);
    let focus_lines: Vec<&str> = described
        .stdout
        .lines()
        .filter(|line| line.trim_start().starts_with("focus:"))
        .collect();
    assert_eq!(focus_lines.len(), 2, "stdout: {}", described.stdout);
    for line in focus_lines {
        assert_eq!(
            line.split_whitespace().last(),
            Some("main"),
            "path target should describe main: {line}"
        );
    }

    let got = fx.run_in(&main, &["project", "info", "--get", "project.focus"]);
    assert!(got.success, "stderr: {}", got.stderr);
    assert_eq!(got.stdout.trim(), "main");
}

/// `--work-dir` is a Profile axis, so it must reach every read query that
/// resolves paths. A helper that dropped it would make `info` print the
/// configured checkout while `build` uses the caller's — exactly the kind of
/// disagreement this report exists to expose.
#[test]
fn project_info_honours_work_dir_for_the_description_and_get() {
    let fx = Fixture::new();
    let elsewhere = fx.path("review-worktree");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let dir = elsewhere.to_str().unwrap();

    let got = fx.ok(&[
        "project",
        "info",
        "--get",
        "repo.workdir",
        "--get",
        "build_dir",
        "work",
        "--branch",
        "main",
        "--work-dir",
        dir,
    ]);
    let build_dir = format!("{dir}/_build/main-debug");
    assert_eq!(
        got.stdout.lines().collect::<Vec<_>>(),
        vec![dir, build_dir.as_str()]
    );

    let described = fx.ok(&[
        "project",
        "info",
        "work",
        "--branch",
        "main",
        "--work-dir",
        dir,
    ]);
    let has = |label: &str, value: &str| {
        described
            .stdout
            .lines()
            .any(|line| line.contains(label) && line.contains(value))
    };
    assert!(
        has("repos.main.workdir:", dir),
        "stdout: {}",
        described.stdout
    );
    assert!(
        has("build_dir:", &build_dir),
        "stdout: {}",
        described.stdout
    );
}

#[test]
fn worktree_strategy_clones_bare_bootstraps_and_updates_main() {
    let fx = Fixture::new();
    let upstream = fx.path("up-wrap");
    git(
        &upstream,
        &[
            "submodule",
            "add",
            "-q",
            fx.path("up-work").to_str().unwrap(),
            "nested/kept",
        ],
    );
    git(&upstream, &["commit", "-q", "-m", "add kept submodule"]);
    git(&upstream, &["branch", "feat"]);
    git(&upstream, &["branch", "other"]);

    let bare = fx.path("bare-wrap.git");
    let bootstrap = fx.path("bare-wrap.wt/main");
    std::fs::write(
        fx.config.join("bare-wrap.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "worktree"
worktree_dir = "{root}/bare-wrap.wt/{{{{branch.slug}}}}"
skip = ["/nested/work", "/vendor", "!/vendor/keep.c"]
[repos.main.remotes]
origin = "{upstream}"
[repos.main.hooks]
post_clone = "test -f top.c && touch post-clone-ran"
[repos.kept]
path = "nested/kept"
main_branch = "main"
anchor = "main"
"#,
            bare = bare.display(),
            root = fx.root.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();

    fx.ok(&["update", "bare-wrap"]);
    assert_eq!(
        git_out(&bare, &["config", "--bool", "core.bare"]).trim(),
        "true"
    );
    assert!(Repository::new(&bare).is_bare());
    assert!(
        !bare.join("top.c").exists(),
        "the Git path is not a checkout"
    );
    assert!(bootstrap.join("top.c").exists(), "main was bootstrapped");
    assert!(
        bootstrap.join("post-clone-ran").exists(),
        "post_clone ran in the bootstrap worktree"
    );
    assert!(!bootstrap.join("nested/work").exists());
    assert!(!bootstrap.join("vendor/blob/big.bin").exists());
    assert!(bootstrap.join("vendor/keep.c").exists());
    assert_eq!(index_tag(&bootstrap, "nested/work"), Some('S'));

    // The host tracks the remote instead of copying its branches into
    // `refs/heads`, so it starts with exactly one local branch and every other
    // branch is reachable as `origin/<name>`.
    assert_eq!(
        git_out(
            &bare,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"]
        )
        .trim(),
        "main"
    );
    for tracked in ["origin/feat", "origin/other"] {
        assert!(
            Repository::new(&bare).rev_exists(tracked),
            "{tracked} should be a remote-tracking ref"
        );
    }
    assert_eq!(
        Repository::new(&bare)
            .remote_default_branch("origin")
            .as_deref(),
        Some("main"),
        "origin/HEAD is published, so worktree info can find a trunk"
    );

    // A later worktree is driven from the sparse bootstrap checkout, so Git
    // copies its patterns even though the common repository itself is bare.
    // Its branch is asked for explicitly: `create` never invents one.
    let feature = fx.path("bare-wrap.feat");
    git(&bare, &["branch", "feat", "origin/feat"]);
    let created = fx.run_in(
        &bare,
        &[
            "worktree",
            "create",
            "feat",
            feature.to_str().unwrap(),
            "--submodules",
        ],
    );
    assert!(created.success, "stderr: {}", created.stderr);
    assert!(!feature.join("nested/work").exists());
    assert!(!feature.join("vendor/blob/big.bin").exists());
    assert!(feature.join("vendor/keep.c").exists());
    assert_eq!(index_tag(&feature, "nested/work"), Some('S'));
    assert!(
        feature.join("nested/kept/component.c").exists(),
        "the retained submodule was materialised"
    );
    // Both checkouts borrow the submodule's objects from the store the *bare
    // repository* owns — including the bootstrap, which is the one that paid for
    // the download. Nothing under a worktree's administrative directory holds a
    // second copy that `worktree remove` would take with it.
    let store = bare.join("modules/nested/kept");
    assert!(
        Repository::new(&store).is_repo() || store.join("objects").is_dir(),
        "the repository should own a store at {}",
        store.display()
    );
    for checkout in [&bootstrap, &feature] {
        let kept_git = Repository::new(checkout.join("nested/kept"))
            .git_dir()
            .unwrap();
        let alternates =
            std::fs::read_to_string(kept_git.join("objects/info/alternates")).unwrap_or_default();
        assert!(
            alternates.contains(store.to_str().unwrap()),
            "{} should borrow from {}, got: {alternates}",
            kept_git.display(),
            store.display()
        );
    }

    // Even if another worktree changes its sparse shape and sorts before the
    // bootstrap, the bare symbolic-HEAD worktree remains the inheritance source.
    git(&feature, &["sparse-checkout", "disable"]);
    let other = fx.path("aaa-other");
    git(&bare, &["branch", "other", "origin/other"]);
    let created = fx.run_in(
        &bare,
        &["worktree", "create", "other", other.to_str().unwrap()],
    );
    assert!(created.success, "stderr: {}", created.stderr);
    assert!(!other.join("nested/work").exists());
    assert!(!other.join("vendor/blob/big.bin").exists());
    assert!(other.join("vendor/keep.c").exists());

    // With main checked out, update fetches into a remote-tracking ref and
    // fast-forwards that linked worktree.
    std::fs::write(upstream.join("top.c"), "v2").unwrap();
    git(&upstream, &["add", "top.c"]);
    git(&upstream, &["commit", "-q", "-m", "v2"]);
    fx.ok(&["update", "bare-wrap"]);
    assert_eq!(
        std::fs::read_to_string(bootstrap.join("top.c")).unwrap(),
        "v2"
    );

    // If the main worktree is gone, update advances the bare main ref directly.
    git(
        &bare,
        &["worktree", "remove", "--force", bootstrap.to_str().unwrap()],
    );
    git(&feature.join("nested/kept"), &["fsck", "--no-dangling"]);
    std::fs::write(upstream.join("top.c"), "v3").unwrap();
    git(&upstream, &["add", "top.c"]);
    git(&upstream, &["commit", "-q", "-m", "v3"]);
    let upstream_head = git_out(&upstream, &["rev-parse", "main"]);
    fx.ok(&["update", "bare-wrap"]);
    assert_eq!(
        git_out(&bare, &["rev-parse", "main"]).trim(),
        upstream_head.trim()
    );
    assert!(
        !bootstrap.exists(),
        "nested repo processing must not recreate a missing bootstrap checkout"
    );
}

/// The checkout that has `main_branch` checked out is advanced *through* a
/// fast-forward merge, whichever declared repo placed it. Two repos sharing one
/// bare host is amdllpc's layout: when main's branch sits in the checkout the
/// sibling places, moving the ref under it instead would leave that checkout
/// with HEAD ahead of its index and files.
#[test]
fn update_advances_main_in_the_checkout_a_sibling_repo_places() {
    let fx = Fixture::new();
    let upstream = Fixture::seed(&fx.root, "up-shared", &["top.c"]);
    let bare = fx.path("shared.git");
    let trees = fx.path("shared");
    std::fs::write(
        fx.config.join("shared.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "hybrid"
worktree_dir = "{trees}/work"
bootstrap_worktree_dir = "main"
[repos.main.remotes]
origin = "{upstream}"
[repos.review]
path = "{bare}"
main_branch = "main"
branch_strategy = "in-place"
worktree_dir = "{trees}/review"
"#,
            bare = bare.display(),
            trees = trees.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();
    fx.ok(&["update", "shared"]);

    let bootstrap = trees.join("main");
    let review = trees.join("review");
    git(&bootstrap, &["switch", "-q", "-c", "parked"]);
    git(
        &bare,
        &["worktree", "add", "-q", review.to_str().unwrap(), "main"],
    );
    std::fs::write(upstream.join("top.c"), "v2").unwrap();
    git(&upstream, &["commit", "-q", "-am", "v2"]);

    fx.ok(&["update", "shared"]);
    assert_eq!(
        git_out(&review, &["rev-parse", "HEAD"]),
        git_out(&upstream, &["rev-parse", "main"])
    );
    assert_eq!(std::fs::read_to_string(review.join("top.c")).unwrap(), "v2");
    assert_eq!(git_out(&review, &["status", "--porcelain"]), "");
}

/// A worktree moved off the `worktree_dir` template still holds its branch, so
/// it is still the checkout `update` advances main through.
#[test]
fn update_advances_main_in_a_worktree_moved_off_its_template() {
    let fx = Fixture::new();
    let upstream = Fixture::seed(&fx.root, "up-moved", &["top.c"]);
    let bare = fx.path("moved.git");
    std::fs::write(
        fx.config.join("moved.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "worktree"
worktree_dir = "{root}/moved.wt/{{{{branch.slug}}}}"
[repos.main.remotes]
origin = "{upstream}"
"#,
            bare = bare.display(),
            root = fx.root.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();
    fx.ok(&["update", "moved"]);

    let elsewhere = fx.path("elsewhere-main");
    git(
        &bare,
        &[
            "worktree",
            "move",
            fx.path("moved.wt/main").to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    std::fs::write(upstream.join("top.c"), "v2").unwrap();
    git(&upstream, &["commit", "-q", "-am", "v2"]);

    fx.ok(&["update", "moved"]);
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("top.c")).unwrap(),
        "v2"
    );
    assert_eq!(git_out(&elsewhere, &["status", "--porcelain"]), "");
}

/// A `skip`ped submodule must never be **materialised** on the way to being
/// masked. A bare host has no checkout for `git worktree add` to copy sparse
/// patterns from, so its bootstrap starts out full — and a mask applied only
/// afterwards means the submodule is cloned in its entirety and then thrown away.
///
/// Proven by pointing the skipped submodule at a source that no longer exists: a
/// clone of it can only fail, so an `update` that succeeds is one that never
/// tried. That is the real shape of the failure — a `skip`ped component is
/// typically one whose checkout this project has no business fetching at all.
#[test]
fn a_bare_backed_clone_masks_a_skipped_submodule_before_materialising_any() {
    let fx = Fixture::new();
    let kept = Fixture::seed(&fx.root, "up-kept", &["kept.c"]);
    let gone = Fixture::seed(&fx.root, "up-gone", &["gone.c"]);
    let upstream = Fixture::seed(&fx.root, "up-super", &["top.c"]);
    for (source, path) in [(&kept, "nested/kept"), (&gone, "nested/gone")] {
        git(
            &upstream,
            &["submodule", "add", "-q", source.to_str().unwrap(), path],
        );
    }
    git(&upstream, &["commit", "-q", "-m", "add submodules"]);
    std::fs::remove_dir_all(&gone).unwrap();

    let bare = fx.path("masked.git");
    let bootstrap = fx.path("masked.wt/main");
    std::fs::write(
        fx.config.join("masked.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "worktree"
worktree_dir = "{root}/masked.wt/{{{{branch.slug}}}}"
skip = ["/nested/gone"]
[repos.main.remotes]
origin = "{upstream}"
"#,
            bare = bare.display(),
            root = fx.root.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();

    fx.ok(&["update", "masked"]);
    assert!(
        bootstrap.join("nested/kept/kept.c").exists(),
        "the submodule the project does keep was materialised"
    );
    assert!(!bootstrap.join("nested/gone").exists());
    assert_eq!(index_tag(&bootstrap, "nested/gone"), Some('S'));
}

/// The two strategies **mixed in one project**, which is the shape that exposes
/// every way a repo's *repository* can be mistaken for its *checkout*: an ordinary
/// in-place shell (with a `skip` of its own) that takes its branch identity from a
/// bare-backed component it **borrows**.
///
/// Three things have to hold, and each was broken by reading `repos.<name>.path`
/// where `repos.<name>.workdir` was meant:
///
/// - nothing is switched. The identity repo's checkout already holds the branch, and
///   `git switch` against its `path` is a bare repository with no working tree —
///   which is the error this whole shape used to die on;
/// - the component's own `worktree_dir` resolves in **its** project's namespace, not
///   the borrower's, even for a branch no worktree holds yet (where the template is
///   what answers);
/// - the shell's `skip` is still verified, because `skip` belongs to the repo rather
///   than to a strategy.
#[test]
fn a_mixed_in_place_and_bare_backed_project_resolves_every_repo_to_its_checkout() {
    let fx = Fixture::new();

    // The component: bare-backed, with `feat` in a worktree of its own. Its
    // `worktree_dir` names `{{project.name}}`, the way a real one does — so a render
    // in the wrong project's namespace shows up as a wrong path rather than passing
    // by luck.
    let comp_up = Fixture::seed(&fx.root, "up-comp", &["lib.c"]);
    git(&comp_up, &["branch", "feat"]);
    let comp_bare = fx.path("comp.git");
    let comp_feat = fx.path("comp.wt/feat");
    std::fs::write(
        fx.config.join("comp.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "hybrid"
worktree_dir = "{root}/{{{{project.name}}}}.wt/{{{{branch.slug}}}}"
bootstrap_worktree_dir = "main"
[repos.main.remotes]
origin = "{upstream}"
"#,
            bare = comp_bare.display(),
            root = fx.root.display(),
            upstream = comp_up.display(),
        ),
    )
    .unwrap();

    // The shell: an ordinary in-place clone that builds its own tree, focused on the
    // borrowed component so the component carries the branch identity. `install_dir`
    // is pointed at `{{repos.component.workdir}}` so one assertion covers both the
    // resolved output path and the repo workdir it is derived from.
    let host_up = fx.path("up-host");
    std::fs::create_dir_all(host_up.join("src")).unwrap();
    git(&host_up, &["init", "-q", "-b", "main", "."]);
    std::fs::write(
        host_up.join("Cargo.toml"),
        "[package]\nname = \"host-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(host_up.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir_all(host_up.join("vendor")).unwrap();
    std::fs::write(host_up.join("vendor/blob.bin"), "x").unwrap();
    git(&host_up, &["add", "-A"]);
    git(&host_up, &["commit", "-q", "-m", "c1"]);
    let host = fx.path("src-host");
    std::fs::write(
        fx.config.join("host.toml"),
        format!(
            r#"
[project]
focus = "component"
build_system = "cargo"
[repos.main]
path = "{host}"
main_branch = "main"
skip = ["/vendor"]
build_dir = "{{{{repos.main.workdir}}}}/target/{{{{branch.slug}}}}"
install_dir = "{{{{repos.component.workdir}}}}"
[repos.main.remotes]
origin = "{upstream}"
[repos.component]
from = "comp"
anchor = "main"
"#,
            host = host.display(),
            upstream = host_up.display(),
        ),
    )
    .unwrap();

    fx.ok(&["update", "comp"]);
    fx.ok(&["update", "host"]);
    // The in-place shell's own `skip` took effect, alongside a bare-backed borrow.
    assert!(!host.join("vendor").exists());
    assert_eq!(index_tag(&host, "vendor/blob.bin"), Some('S'));

    // Add the fixed detached checkout as a separately selectable repo after the
    // initial lifecycle. This mirrors an owner exposing `repos.review` and a
    // consumer borrowing it as `--focus component-review`.
    let comp_review = fx.path("comp.wt/review");
    let comp_config = fx.config.join("comp.toml");
    let mut body = std::fs::read_to_string(&comp_config).unwrap();
    body.push_str(&format!(
        r#"
[repos.review]
path = "{}"
main_branch = "main"
branch_strategy = "in-place"
worktree_dir = "{}"
"#,
        comp_bare.display(),
        comp_review.display()
    ));
    std::fs::write(&comp_config, body).unwrap();

    let host_config = fx.config.join("host.toml");
    let mut body = std::fs::read_to_string(&host_config).unwrap();
    body.push_str(
        r#"
[repos.component-review]
from = "comp:review"
anchor = "main"
build_dir = "{{repos.main.workdir}}/target/component-review"
install_dir = "{{repos['component-review'].workdir}}"
"#,
    );
    std::fs::write(&host_config, body).unwrap();

    git(&comp_bare, &["branch", "feat", "origin/feat"]);
    let created = fx.run_in(
        &comp_bare,
        &["worktree", "create", "feat", comp_feat.to_str().unwrap()],
    );
    assert!(created.success, "stderr: {}", created.stderr);

    // The component's workdir, seen from the borrower: the live worktree for `feat`…
    // Repeatable `--get`: one resolve answers both the derived output path and
    // the borrowed component's own checkout, in flag order.
    let resolved = fx.ok(&[
        "project",
        "info",
        "--get",
        "install_dir",
        "--get",
        "repos.component.workdir",
        "host",
        "--branch",
        "feat",
    ]);
    let want = comp_feat.to_str().unwrap();
    assert_eq!(
        resolved.stdout.lines().collect::<Vec<_>>(),
        vec![want, want],
        "stdout: {}",
        resolved.stdout
    );
    // …and, for a branch no worktree holds, the location the component's *own*
    // `worktree_dir` names. Rendered in the borrower's namespace this would read
    // `src-host.wt/` or `host.wt/`, quietly relocating a shared component under
    // whoever consumes it.
    let absent = fx.ok(&[
        "project",
        "info",
        "--get",
        "install_dir",
        "host",
        "--branch",
        "later",
    ]);
    assert_eq!(
        absent.stdout.trim(),
        fx.path("comp.wt/later").to_str().unwrap()
    );

    let built = fx.run(&["build", "host", "--branch", "feat"]);
    assert!(built.success, "stderr: {}", built.stderr);
    assert!(host.join("target/feat/debug/host-fixture").exists());

    // The same mixed project can build a detached review snapshot of the
    // borrowed identity repo. The root remains the in-place build base, while
    // the component's current checkout supplies the detached HEAD.
    git(
        &comp_bare,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            comp_review.to_str().unwrap(),
            "origin/feat",
        ],
    );
    let comp_bootstrap = fx.path("comp.wt/main");
    let reviewed = fx.run_in(
        &comp_bootstrap,
        &["build", "host", "--detach", "--focus", "component-review"],
    );
    assert!(reviewed.success, "stderr: {}", reviewed.stderr);
    assert!(
        host.join("target/component-review/debug/host-fixture")
            .exists(),
        "focus-local build_dir did not override the branch-keyed anchor default"
    );

    // Nothing moved: the bare repository still points at its own main, the worktree
    // still holds `feat`, and the shell's checkout is where it was.
    assert_eq!(
        git_out(&comp_bare, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "main"
    );
    assert_eq!(
        git_out(&comp_feat, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "feat"
    );
    assert_eq!(
        git_out(&host, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "main"
    );

    // A branch with no worktree is refused by name, rather than by whatever `git
    // switch` would have said about a directory that is not a checkout.
    let missing = fx.run(&["build", "host", "--branch", "later"]);
    assert!(!missing.success);
    assert!(
        missing.stderr.contains("worktree for branch 'later'")
            && missing.stderr.contains("component"),
        "the error names the branch and the repo that carries identity: {}",
        missing.stderr
    );
}

#[test]
fn hybrid_uses_the_worktree_that_actually_holds_the_branch() {
    let fx = Fixture::new();
    let upstream = fx.path("up-work");
    std::fs::create_dir_all(upstream.join("src")).unwrap();
    std::fs::write(
        upstream.join("Cargo.toml"),
        "[package]\nname = \"hybrid-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(upstream.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&upstream, &["add", "Cargo.toml", "src/main.rs"]);
    git(&upstream, &["commit", "-q", "-m", "add cargo fixture"]);
    git(&upstream, &["branch", "feat"]);

    let bare = fx.path("hybrid.git");
    let bootstrap = fx.path("hybrid.wt/main");
    let suggested = fx.path("hybrid.wt/feat");
    std::fs::write(
        fx.config.join("hybrid.toml"),
        format!(
            r#"
[project]
build_system = "cargo"
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "hybrid"
worktree_dir = "{root}/hybrid.wt/{{{{branch.slug}}}}"
bootstrap_worktree_dir = "main"
build_dir = "{{{{repos.main.workdir}}}}/target"
[repos.main.remotes]
origin = "{upstream}"
"#,
            bare = bare.display(),
            root = fx.root.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();

    fx.ok(&["update", "hybrid"]);
    fn workdir(branch: &str) -> [&str; 7] {
        [
            "project",
            "info",
            "--get",
            "repos.main.workdir",
            "hybrid",
            "--branch",
            branch,
        ]
    }
    let main = fx.ok(&workdir("main"));
    assert_eq!(
        main.stdout.trim(),
        bootstrap.to_str().unwrap(),
        "hybrid discovers the fixed bootstrap rather than returning its suggestion"
    );

    let custom = fx.path("somewhere/custom-feature");
    git(&bare, &["branch", "feat", "origin/feat"]);
    let created = fx.run_in(
        &bare,
        &["worktree", "create", "feat", custom.to_str().unwrap()],
    );
    assert!(created.success, "stderr: {}", created.stderr);

    let queried = fx.ok(&workdir("feat"));
    assert_eq!(queried.stdout.trim(), custom.to_str().unwrap());

    // Reverse lookup includes linked worktrees, and the current worktree branch
    // wins over the bare repository's symbolic HEAD.
    let from_inside = fx.run_in(&custom, &["project", "info", "--get", "repos.main.workdir"]);
    assert!(from_inside.success, "stderr: {}", from_inside.stderr);
    assert_eq!(from_inside.stdout.trim(), custom.to_str().unwrap());

    let built = fx.run(&["build", "hybrid", "--branch", "feat"]);
    assert!(built.success, "stderr: {}", built.stderr);
    assert!(custom.join("target/debug/hybrid-fixture").exists());

    let missing = fx.run(&["build", "hybrid", "--branch", "absent"]);
    assert!(!missing.success);
    assert!(
        missing
            .stderr
            .contains("is not checked out in any worktree"),
        "stderr: {}",
        missing.stderr
    );
    assert!(
        missing
            .stderr
            .contains(suggested.parent().unwrap().to_str().unwrap()),
        "the error suggests the declared worktree_dir: {}",
        missing.stderr
    );
    assert!(!fx.path("hybrid.wt/absent").exists());
}

/// Review checkouts are deliberately detached snapshots. Building one must be
/// an explicit choice, must source from that checkout rather than the primary,
/// and must not fabricate `branch.*` for path templates.
#[test]
fn detached_head_builds_a_review_checkout_only_when_requested() {
    let fx = Fixture::new();
    let upstream = fx.path("up-reviewable");
    std::fs::create_dir_all(upstream.join("src")).unwrap();
    git(&upstream, &["init", "-q", "-b", "main", "."]);
    std::fs::write(
        upstream.join("Cargo.toml"),
        "[package]\nname = \"reviewable\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(upstream.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&upstream, &["add", "Cargo.toml", "src/main.rs"]);
    git(&upstream, &["commit", "-q", "-m", "add cargo fixture"]);

    let checkout = fx.path("src-reviewable");
    let config = fx.config.join("reviewable.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[project]
build_system = "cargo"
[repos.main]
path = "{checkout}"
main_branch = "main"
build_dir = "{{{{repos.main.workdir}}}}/target"
[repos.main.remotes]
origin = "{upstream}"
"#,
            checkout = checkout.display(),
            upstream = upstream.display(),
        ),
    )
    .unwrap();
    fx.ok(&["update", "reviewable"]);

    let review = fx.path("src-reviewable.review");
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            review.to_str().unwrap(),
            "HEAD",
        ],
    );

    let implicit = fx.run_in(&review, &["build"]);
    assert!(!implicit.success);
    assert!(
        implicit.stderr.contains("--detach") && implicit.stderr.contains("--branch"),
        "default detached HEAD error names both explicit choices: {}",
        implicit.stderr
    );

    let attached = fx.run_in(&checkout, &["build", "--detach"]);
    assert!(!attached.success);
    assert!(
        attached
            .stderr
            .contains("--detach requires a detached HEAD"),
        "stderr: {}",
        attached.stderr
    );

    let conflict = fx.run_in(&review, &["build", "--detach", "--branch", "main"]);
    assert!(!conflict.success);
    assert!(
        conflict.stderr.contains("--detach") && conflict.stderr.contains("--branch"),
        "clap reports the mutually exclusive selectors: {}",
        conflict.stderr
    );

    // If detached resolution accidentally fell back to the configured in-place
    // checkout, Cargo would now fail before producing the review binary.
    std::fs::remove_file(checkout.join("Cargo.toml")).unwrap();
    let built = fx.run_in(&review, &["build", "--detach"]);
    assert!(built.success, "stderr: {}", built.stderr);
    assert!(review.join("target/debug/reviewable").exists());

    // A branch-dependent template stays unavailable in detached mode, while an
    // explicit output override bypasses it. `--work-dir` remains an independent
    // location override rather than an implicit request for detached semantics.
    let body = std::fs::read_to_string(&config).unwrap().replace(
        r#"build_dir = "{{repos.main.workdir}}/target""#,
        r#"build_dir = "{{repos.main.workdir}}/_build/{{branch.slug}}"
install_dir = "{{repos.main.workdir}}/_install/{{branch.slug}}""#,
    );
    std::fs::write(&config, body).unwrap();
    let unresolved = fx.run_in(&review, &["build", "--detach"]);
    assert!(!unresolved.success);
    assert!(
        unresolved.stderr.contains("branch.slug"),
        "stderr: {}",
        unresolved.stderr
    );

    let override_dir = review.join("override-target");
    let overridden = fx.run(&[
        "build",
        "reviewable",
        "--detach",
        "--work-dir",
        review.to_str().unwrap(),
        "--build-dir",
        override_dir.to_str().unwrap(),
        "--install-dir",
        override_dir.to_str().unwrap(),
    ]);
    assert!(overridden.success, "stderr: {}", overridden.stderr);
    assert!(override_dir.join("debug/reviewable").exists());
}

/// A repo forked from an upstream whose default branch is `main` while the
/// project develops on `stable`, its tracking refs made by hand. Returns the
/// checkout and an empty registry root.
fn trunk_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let repo = Fixture::seed(root, "r", &["f"]);
    git(
        &repo,
        &["remote", "add", "upstream", "https://example.com/r.git"],
    );
    for branch in ["main", "stable"] {
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/remotes/upstream/{branch}"),
                "HEAD",
            ],
        );
    }
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/upstream/HEAD",
            "refs/remotes/upstream/main",
        ],
    );
    let config = root.join("config");
    std::fs::create_dir_all(&config).unwrap();
    (repo, config)
}

fn trunk_query(repo: &Path, config: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_wits"))
        .arg("__trunk")
        .args(args)
        .current_dir(repo)
        .envs(hermetic_git())
        .env("WITS_PROJECT_CONFIG", config)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The hooks and aliases ask `__trunk`, so it must give the commands' answer: a
/// project's declared `main_branch` over the server's default branch, measured
/// at the merge target's tracking ref.
#[test]
fn the_trunk_query_answers_a_declared_main_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, config) = trunk_fixture(tmp.path());
    std::fs::write(
        config.join("r.toml"),
        format!(
            "[project]\n[repos.main]\npath = \"{}\"\nmain_branch = \"stable\"\n\
             [repos.main.remotes]\nupstream = \"https://example.com/r.git\"\n",
            repo.display()
        ),
    )
    .unwrap();

    assert_eq!(trunk_query(&repo, &config, &[]), "stable");
    assert_eq!(trunk_query(&repo, &config, &["--rev"]), "upstream/stable");
}

/// A repo no project declares answers from its remotes alone.
#[test]
fn the_trunk_query_reads_an_undeclared_repos_remote_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, config) = trunk_fixture(tmp.path());

    assert_eq!(trunk_query(&repo, &config, &[]), "main");
    assert_eq!(trunk_query(&repo, &config, &["--rev"]), "upstream/main");
}

/// `__names` is the completion surface fish reads at TAB time: a scoped
/// answer offers only spellings the target's flag accepts, the no-target union
/// is the over-approximation completion uses while the target is still being
/// typed, and the hidden command must stay out of the help like the other
/// plumbing.
#[test]
fn the_names_query_lists_candidates_for_completion() {
    let fx = Fixture::new();

    std::fs::write(
        fx.config.join("toolchains.toml"),
        "[toolchains.clang]\ncc = \"clang\"\n[toolchains.gcc]\n",
    )
    .unwrap();
    std::fs::write(
        fx.config.join("presets.toml"),
        format!(
            r#"
[project]
org = "demo"
[project.presets.common]
environment = {{ CFLAGS = "-O2" }}
[project.presets.unix]
extends = "common"
[repos.main]
path = "{}"
main_branch = "main"
[org]
name = "demo"
[org.presets.shared]
environment = {{ CXXFLAGS = "-std=c++20" }}
"#,
            fx.path("src-presets").display()
        ),
    )
    .unwrap();
    // A second provider, so the union mode has something the scoped one does
    // not: `extra` lands inside work.toml's `[repos.main]` region, and the
    // table header opens its own namespace from there.
    Fixture::declare_work(&fx.root, "[project.presets.local]\n");

    // Toolchains are registry-wide.
    let out = fx.ok(&["project", "__names", "toolchains"]);
    assert_eq!(out.stdout, "clang\ngcc\n");

    // A scoped presets answer: bare names the project resolves (its own, and
    // its org's — bare org names merge on lookup), plus the org's qualified
    // `org/name` forms — the spellings `--preset` accepts.
    let out = fx.ok(&["project", "__names", "presets", "presets"]);
    assert_eq!(out.stdout, "common\ndemo/shared\nshared\nunix\n");

    // Union mode: every project's candidates, deduplicated and sorted.
    let out = fx.ok(&["project", "__names", "presets"]);
    assert_eq!(out.stdout, "common\ndemo/shared\nlocal\nshared\nunix\n");

    // Repos, scoped and union.
    let out = fx.ok(&["project", "__names", "repos", "work"]);
    assert_eq!(out.stdout, "main\n");
    let out = fx.ok(&["project", "__names", "repos"]);
    assert_eq!(out.stdout, "component\nmain\n");

    // An unknown target is an error, so completion can fall back to the
    // union instead of offering an empty list.
    let out = fx.run(&["project", "__names", "repos", "missing"]);
    assert!(!out.success);
    assert!(out.stderr.contains("no project 'missing'"));

    // Hidden plumbing: no help entry, like the other `__` queries.
    let help = fx.ok(&["project", "--help"]);
    assert!(!help.stdout.contains("__names"));
}

/// A project with no `build_dir` has nothing a build could produce. It says so
/// and returns success — picking a directory on its own would be a build the
/// config never named.
#[test]
fn a_build_without_a_build_dir_stops_with_a_warning() {
    let fx = Fixture::new();
    std::fs::write(
        fx.config.join("nobuild.toml"),
        format!(
            "[project]\nbuild_system = \"cargo\"\n\
             [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n",
            fx.path("src-nowhere").display()
        ),
    )
    .unwrap();

    let out = fx.ok(&["build", "nobuild", "-b", "main"]);
    assert!(
        out.stderr.contains("no build_dir configured"),
        "stderr: {}",
        out.stderr
    );
}

/// A `build_dir` with no backend to realise it is a configuration contradiction,
/// not a silent no-op.
#[test]
fn a_build_dir_without_a_build_system_is_refused() {
    let fx = Fixture::new();
    let src = fx.path("src-nosys");
    std::fs::write(
        fx.config.join("nosys.toml"),
        format!(
            "[project]\n\
             [repos.main]\npath = \"{}\"\nmain_branch = \"main\"\n\
             build_dir = \"{}/_build\"\n",
            src.display(),
            src.display()
        ),
    )
    .unwrap();

    let out = fx.run(&["build", "nosys", "-b", "main"]);
    assert!(!out.success);
    assert!(
        out.stderr
            .contains("build_dir is set but build_system is not"),
        "stderr: {}",
        out.stderr
    );
}

/// `WITS_PROJECT_TOOLCHAIN` is the top selection entry, and an explicit request
/// is never trusted away: setting it injects that toolchain even into an
/// already-configured tree, and it outranks the project's field.
#[test]
fn an_env_selected_toolchain_is_injected_into_a_configured_dir() {
    let fx = Fixture::new();
    let src = Fixture::seed(&fx.root, "src-envtool", &["meson.build"]);
    std::fs::create_dir_all(src.join("_build/meson-private")).unwrap();
    std::fs::write(src.join("_build/meson-private/coredata.dat"), "").unwrap();
    std::fs::write(
        fx.config.join("envtool.toml"),
        format!(
            r#"
[project]
build_system = "meson"
toolchain = "t"
[repos.main]
path = "{src}"
main_branch = "main"
build_dir = "{src}/_build"
[toolchains.t]
cc = "cc-from-project"
[toolchains.t2]
cc = "cc-from-env"
"#,
            src = src.display()
        ),
    )
    .unwrap();

    // Trusted: the configured tree is left alone, so no `CC` reaches the step.
    let trusted = fx.ok(&["-n", "build", "envtool", "-b", "main"]);
    assert!(
        !trusted.stdout.contains("cc-from-project") && !trusted.stdout.contains("cc-from-env"),
        "a configured tree without an explicit toolchain was not trusted: {}",
        trusted.stdout
    );

    // Explicit through the environment: injection happens, and the env entry
    // outranks the project field.
    let injected = fx.run_env(
        &["-n", "build", "envtool", "-b", "main"],
        &[("WITS_PROJECT_TOOLCHAIN", "t2")],
    );
    assert!(injected.success, "stderr: {}", injected.stderr);
    assert!(
        injected.stdout.contains("CC=cc-from-env"),
        "the env-selected toolchain was not injected: {}",
        injected.stdout
    );
    assert!(
        !injected.stdout.contains("cc-from-project"),
        "the project field won over WITS_PROJECT_TOOLCHAIN: {}",
        injected.stdout
    );
}

/// In-place owns one checkout, so building another branch is the classic
/// stash → switch → build → restore dance: the *switched* tree is what gets
/// built, and the caller's checkout — dirty or not — is where it was.
#[test]
fn an_in_place_build_switches_builds_and_restores_the_checkout() {
    let fx = Fixture::new();

    let up = fx.path("up-inplace");
    std::fs::create_dir_all(up.join("src")).unwrap();
    git(&up, &["init", "-q", "-b", "main", "."]);
    std::fs::write(
        up.join("Cargo.toml"),
        "[package]\nname = \"main-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(up.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "-q", "-m", "c1"]);
    // The branch under test compiles to a differently named binary, so a build
    // that never switched cannot pass by leaving a stale `main-fixture` behind.
    git(&up, &["switch", "-q", "-c", "feat"]);
    std::fs::write(
        up.join("Cargo.toml"),
        "[package]\nname = \"feat-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "-q", "-m", "feat"]);
    git(&up, &["switch", "-q", "main"]);

    let src = fx.path("src-inplace");
    std::fs::write(
        fx.config.join("inplace.toml"),
        format!(
            r#"
[project]
build_system = "cargo"
[repos.main]
path = "{src}"
main_branch = "main"
build_dir = "{{{{repos.main.workdir}}}}/target/{{{{branch.slug}}}}"
[repos.main.remotes]
origin = "{up}"
"#,
            src = src.display(),
            up = up.display()
        ),
    )
    .unwrap();

    fx.ok(&["update", "inplace"]);
    git(&src, &["branch", "feat", "origin/feat"]);
    // A dirty checkout: the auto-stash has to survive the round trip.
    std::fs::write(src.join("src/main.rs"), "fn main() { /* local */ }\n").unwrap();

    fx.ok(&["build", "inplace", "-b", "feat"]);
    assert!(
        src.join("target/feat/debug/feat-fixture").exists(),
        "the feat branch was not the tree that got built"
    );
    assert!(
        !src.join("target/feat/debug/main-fixture").exists(),
        "the build ran from main's tree while claiming to build feat"
    );
    assert_eq!(
        git_out(&src, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "main",
        "the in-place checkout was not restored to its branch"
    );
    assert_eq!(
        std::fs::read_to_string(src.join("src/main.rs")).unwrap(),
        "fn main() { /* local */ }\n",
        "the stashed working tree was not restored"
    );
}

/// A hook that fails stops the repo there: the default action must not run
/// behind a failed `pre_update`, or an override could be contradicted silently.
#[test]
fn a_failing_update_hook_stops_the_repo_before_the_default_action() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    Fixture::declare_work(&fx.root, "[repos.main.hooks]\npre_update = \"exit 7\"");

    // Move the upstream, so an update that ran the default action anyway would
    // leave a visible new `origin/main` behind.
    std::fs::write(fx.path("up-work/new.c"), "c").unwrap();
    git(&fx.path("up-work"), &["add", "-A"]);
    git(&fx.path("up-work"), &["commit", "-q", "-m", "moved"]);
    let upstream_head = git_out(&fx.path("up-work"), &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    let out = fx.run(&["update", "work"]);
    assert!(!out.success, "a failing hook was not fatal");
    assert!(
        out.stderr.contains("pre_update hook failed (exit 7)"),
        "stderr: {}",
        out.stderr
    );
    assert_ne!(
        git_out(&fx.path("src-work"), &["rev-parse", "origin/main"]).trim(),
        upstream_head,
        "the default action ran behind the failed hook"
    );
}

/// A declared `hooks.update` replaces the default action *in place*: it runs
/// between `pre_update` and `post_update`, and the default fetch/merge runs
/// nowhere behind it.
#[test]
fn an_update_hook_override_replaces_the_default_action() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    Fixture::declare_work(
        &fx.root,
        "[repos.main.hooks]\nupdate = \"touch update-override-ran\"\n\
         post_update = \"test -f update-override-ran && touch post-update-ran\"",
    );

    // Move the upstream: an update that still fetched would advance the
    // tracking ref, which is exactly what an override must not need.
    std::fs::write(fx.path("up-work/new.c"), "c").unwrap();
    git(&fx.path("up-work"), &["add", "-A"]);
    git(&fx.path("up-work"), &["commit", "-q", "-m", "moved"]);
    let upstream_head = git_out(&fx.path("up-work"), &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    fx.ok(&["update", "work"]);
    let src = fx.path("src-work");
    assert!(
        src.join("update-override-ran").exists(),
        "the update override did not run in the checkout"
    );
    assert!(
        src.join("post-update-ran").exists(),
        "post_update did not run after the override"
    );
    assert_ne!(
        git_out(&src, &["rev-parse", "origin/main"]).trim(),
        upstream_head,
        "the default action ran behind the override"
    );
}

/// Declared remotes converge — the fetch URL and the push-URL *set* — and a
/// remote the config never names is left exactly as it is.
#[test]
fn an_update_converges_declared_remotes_and_never_touches_undeclared_ones() {
    let fx = Fixture::new();
    fx.ok(&["update", "work"]);
    let src = fx.path("src-work");
    let mirror = "https://example.invalid/mirror.git";

    // A mirror gives the push-URL set something to be: with none, git falls back
    // to the fetch URL and a wrong set is unobservable.
    std::fs::write(
        fx.config.join("work.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{src}"
main_branch = "main"
build_dir = "{{{{repos.main.workdir}}}}/_build/{{{{branch.slug}}}}-{{{{build_type}}}}"
[repos.main.remotes]
origin = {{ url = "{up}", mirrors = ["{mirror}"] }}
"#,
            src = src.display(),
            up = fx.path("up-work").display()
        ),
    )
    .unwrap();

    git(
        &src,
        &[
            "remote",
            "add",
            "handmade",
            "https://example.invalid/handmade.git",
        ],
    );
    // Drift every declared half: the fetch URL, and the push-URL set itself —
    // two foreign entries, so convergence must replace a set, not just one URL.
    git(
        &src,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/wrong.git",
        ],
    );
    git(
        &src,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            "https://example.invalid/one.git",
        ],
    );
    git(
        &src,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            "https://example.invalid/two.git",
        ],
    );

    fx.ok(&["update", "work"]);
    let want = fx.path("up-work");
    assert_eq!(
        git_out(&src, &["remote", "get-url", "origin"]).trim(),
        want.to_str().unwrap(),
        "the declared fetch URL did not converge"
    );
    assert_eq!(
        git_out(&src, &["config", "--get-all", "remote.origin.pushurl"])
            .lines()
            .collect::<Vec<_>>(),
        vec![want.to_str().unwrap(), mirror],
        "the declared push-URL set did not converge"
    );
    assert_eq!(
        git_out(&src, &["remote", "get-url", "handmade"]).trim(),
        "https://example.invalid/handmade.git",
        "an undeclared remote was touched"
    );
}
