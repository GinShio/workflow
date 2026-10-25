//! Path lookup and checkout selection when sibling repos share one git dir.
//!
//! A bare host and a linked worktree are different paths for the same
//! repository. Path lookup attributes such a worktree by re-resolving the
//! repo's strategy (project-design, "Path lookup"), and checkout selection must
//! answer the same question the same way.

use std::path::{Path, PathBuf};
use std::process::Command;

use wits_project::{layout, Workspace};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "core.hooksPath=/nonexistent-wits-test-hooks",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Fixture {
    _root: tempfile::TempDir,
    bootstrap: PathBuf,
    main_worktree: PathBuf,
    review_worktree: PathBuf,
    ws: Workspace,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main", "."]);
    std::fs::write(source.join("file"), "x").unwrap();
    git(&source, &["add", "file"]);
    git(&source, &["commit", "-q", "-m", "c1"]);

    let bare = root.path().join("p.git");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(&bare, &["branch", "feat", "main"]);
    git(&bare, &["branch", "review-branch", "main"]);

    let bootstrap = root.path().join("p.bootstrap");
    git(
        &bare,
        &["worktree", "add", "-q", bootstrap.to_str().unwrap(), "main"],
    );
    let main_worktree = root.path().join("p.wt/feat");
    std::fs::create_dir_all(main_worktree.parent().unwrap()).unwrap();
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            main_worktree.to_str().unwrap(),
            "feat",
        ],
    );
    let review_worktree = root.path().join("review");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            review_worktree.to_str().unwrap(),
            "review-branch",
        ],
    );

    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("p.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "worktree"
worktree_dir = "{root}/p.wt/{{{{branch.slug}}}}"
bootstrap_worktree_dir = "{root}/p.bootstrap"
[repos.review]
path = "{review}"
main_branch = "review-branch"
"#,
            bare = bare.display(),
            root = root.path().display(),
            review = review_worktree.display(),
        ),
    )
    .unwrap();
    let ws = Workspace::load_from(&config).unwrap();

    Fixture {
        _root: root,
        bootstrap,
        main_worktree,
        review_worktree,
        ws,
    }
}

#[test]
fn a_linked_worktree_is_attributed_to_its_own_sibling_repo() {
    let fx = fixture();
    let (project, repo) = fx
        .ws
        .repo_for_path(&fx.review_worktree.join("src"))
        .unwrap();
    assert_eq!((project.name(), repo.name()), ("p", "review"));
    let (project, repo) = fx.ws.repo_for_path(&fx.main_worktree.join("src")).unwrap();
    assert_eq!((project.name(), repo.name()), ("p", "main"));
}

#[test]
fn checkout_selection_does_not_borrow_a_siblings_worktree() {
    let fx = fixture();
    let project = fx.ws.project("p").unwrap();
    let main = project.repo("main").unwrap();
    let review = project.repo("review").unwrap();

    // `review` holds `review-branch`, but `main`'s own strategy places that
    // branch at `p.wt/review-branch`, not at the review worktree.
    assert_eq!(
        layout::checkout_holding(&fx.ws, project, main, "review-branch").unwrap(),
        None
    );
    assert_eq!(
        layout::checkout_holding(&fx.ws, project, main, "feat").unwrap(),
        Some(fx.main_worktree.clone())
    );
    assert_eq!(
        layout::checkout_holding(&fx.ws, project, main, "main").unwrap(),
        Some(fx.bootstrap.clone())
    );
    // `review` has no `worktree_dir`, so its own live worktree is the answer.
    assert_eq!(
        layout::checkout_holding(&fx.ws, project, review, "review-branch").unwrap(),
        Some(fx.review_worktree.clone())
    );
}

/// A hybrid repo searches the live inventory, but that search must not swallow a
/// checkout a sibling repo *places*. Here `main` is hybrid and `review` declares
/// its fixed checkout; without the sibling rule the review worktree is a live
/// checkout of `main`'s git dir, so the two become indistinguishable — the tie
/// the rule exists to break.
#[test]
fn a_hybrid_repo_does_not_claim_a_siblings_fixed_checkout() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main", "."]);
    std::fs::write(source.join("file"), "x").unwrap();
    git(&source, &["add", "file"]);
    git(&source, &["commit", "-q", "-m", "c1"]);

    let bare = root.path().join("p.git");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(&bare, &["branch", "dev"]);
    let main_worktree = root.path().join("p.wt/dev");
    std::fs::create_dir_all(main_worktree.parent().unwrap()).unwrap();
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            main_worktree.to_str().unwrap(),
            "dev",
        ],
    );
    // The sibling's checkout is detached (git refuses two worktrees on one
    // branch), so the branch cannot tell them apart: only the declared placement
    // can, which is what the sibling rule compares.
    let review_worktree = root.path().join("review");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            review_worktree.to_str().unwrap(),
            "dev",
        ],
    );

    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("p.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "dev"
branch_strategy = "hybrid"
worktree_dir = "{root}/p.wt/{{{{branch.slug}}}}"
bootstrap_worktree_dir = "main"
[repos.review]
path = "{bare}"
main_branch = "dev"
branch_strategy = "in-place"
worktree_dir = "{review}"
"#,
            bare = bare.display(),
            root = root.path().display(),
            review = review_worktree.display(),
        ),
    )
    .unwrap();
    let ws = Workspace::load_from(&config).unwrap();
    let project = ws.project("p").unwrap();
    let main = project.repo("main").unwrap();
    let review = project.repo("review").unwrap();

    let (found, repo) = ws
        .repo_for_path(&review_worktree.join("file"))
        .expect("the review checkout is a known project's");
    assert_eq!((found.name(), repo.name()), ("p", "review"));
    assert_eq!(
        layout::checkout_holding(&ws, project, main, "dev").unwrap(),
        Some(main_worktree.clone())
    );
    // A detached checkout holds no branch of its own, so no branch query selects
    // it; `current_checkout` is what must answer with it rather than with the
    // sibling's worktree, which is the caller's-checkout half of the same rule.
    assert_eq!(
        layout::primary_checkout(&ws, project, main).unwrap(),
        Some(main_worktree)
    );
    assert!(layout::checkout_holding(&ws, project, review, "dev")
        .unwrap()
        .is_none());
}

#[test]
fn a_bare_backed_declaration_on_a_conventional_checkout_still_answers() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("p");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("file"), "x").unwrap();
    git(&repo, &["add", "file"]);
    git(&repo, &["commit", "-q", "-m", "c1"]);

    let config = root.path().join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("p.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{}"
main_branch = "main"
branch_strategy = "worktree"
worktree_dir = "{}/p.wt/{{{{branch.slug}}}}"
"#,
            repo.display(),
            root.path().display()
        ),
    )
    .unwrap();
    let ws = Workspace::load_from(&config).unwrap();
    let project = ws.project("p").unwrap();
    assert_eq!(
        layout::checkout_holding(&ws, project, project.repo("main").unwrap(), "main").unwrap(),
        Some(repo)
    );
}
