//! Path lookup and checkout selection when sibling repos share one git dir.
//!
//! A bare host and a linked worktree are different paths for the same
//! repository. Path lookup attributes such a worktree by the declared
//! placements of the repos over that repository (project-reference, "Several
//! repos over one repository"), and checkout selection must answer the same
//! question the same way.

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
    /// A worktree of `main`'s repository on branch `moved`, off the
    /// `worktree_dir` template: placed by no declaration.
    elsewhere: PathBuf,
    review_worktree: PathBuf,
    ws: Workspace,
}

fn fixture() -> Fixture {
    fixture_with("review")
}

/// `sibling` names the repo declared at the review worktree, so a test can show
/// the answer does not hang on which name sorts first.
fn fixture_with(sibling: &str) -> Fixture {
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
    git(&bare, &["branch", "moved", "main"]);
    let elsewhere = root.path().join("elsewhere/moved");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            elsewhere.to_str().unwrap(),
            "moved",
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
[repos.{sibling}]
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
        elsewhere,
        review_worktree,
        ws,
    }
}

/// Every worktree of the shared repository goes to the repo whose declaration
/// places it, and an unplaced one to the bare-backed repo — whatever the
/// sibling is called. A sibling declared at one checkout claims only that
/// checkout; claiming the rest made the answer hang on name order.
#[test]
fn attribution_follows_declarations_not_repo_names() {
    for sibling in ["review", "aaa-review"] {
        let fx = fixture_with(sibling);
        let owner = |path: &Path| {
            let (project, repo) = fx.ws.repo_for_path(path).unwrap();
            (project.name().to_owned(), repo.name().to_owned())
        };
        let p = |repo: &str| ("p".to_owned(), repo.to_owned());
        assert_eq!(owner(&fx.review_worktree.join("src")), p(sibling));
        for main_checkout in [&fx.main_worktree, &fx.bootstrap, &fx.elsewhere] {
            assert_eq!(owner(&main_checkout.join("src")), p("main"), "{sibling}");
        }

        let project = fx.ws.project("p").unwrap();
        assert_eq!(
            layout::checkout_holding(&fx.ws, project, project.repo("main").unwrap(), "moved")
                .unwrap(),
            Some(fx.elsewhere.clone()),
            "an unplaced worktree is the bare-backed repo's checkout"
        );
        assert_eq!(
            layout::checkout_holding(&fx.ws, project, project.repo(sibling).unwrap(), "moved")
                .unwrap(),
            None,
            "an in-place repo's checkout is its own path"
        );
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

/// amdllpc's shape: a hybrid `main` with a fixed `worktree_dir` and a bootstrap
/// at `trees/main`, and an in-place `review` over the same bare host whose fixed
/// checkout is `trees/review`. The caller arranges the worktrees.
fn shared_host(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main", "."]);
    std::fs::write(source.join("file"), "x").unwrap();
    git(&source, &["add", "file"]);
    git(&source, &["commit", "-q", "-m", "c1"]);
    let bare = root.join("llpc.git");
    git(
        root,
        &[
            "clone",
            "-q",
            "--bare",
            source.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let config = root.join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("llpc.toml"),
        format!(
            r#"
[project]
[repos.main]
path = "{bare}"
main_branch = "main"
branch_strategy = "hybrid"
worktree_dir = "{trees}/work"
bootstrap_worktree_dir = "main"
[repos.review]
path = "{bare}"
main_branch = "main"
branch_strategy = "in-place"
worktree_dir = "{trees}/review"
"#,
            bare = bare.display(),
            trees = root.join("trees").display(),
        ),
    )
    .unwrap();
    (bare, config)
}

/// A borrower sees the owner's repos only as copies, so attribution is asked in
/// the owner's frame: a branch checked out in the checkout the owner's `review`
/// places is not `main`'s, whether the owner or a borrower of `main` asks.
#[test]
fn owner_and_borrower_attribute_a_siblings_checkout_alike() {
    let root = tempfile::tempdir().unwrap();
    let (bare, config) = shared_host(root.path());
    let trees = root.path().join("trees");
    git(&bare, &["branch", "topic", "main"]);
    for (dir, branch) in [("main", "main"), ("review", "topic")] {
        let at = trees.join(dir);
        git(
            &bare,
            &["worktree", "add", "-q", at.to_str().unwrap(), branch],
        );
    }
    std::fs::write(
        config.join("ogl.toml"),
        format!(
            r#"
[project]
focus = "compiler"
[repos.main]
path = "{}"
main_branch = "main"
[repos.compiler]
from = "llpc"
anchor = "main"
"#,
            root.path().join("ogl").display()
        ),
    )
    .unwrap();
    let ws = Workspace::load_from(&config).unwrap();
    let llpc = ws.project("llpc").unwrap();
    let ogl = ws.project("ogl").unwrap();
    let owned = llpc.repo("main").unwrap();
    let borrowed = ogl.repo("compiler").unwrap();

    for (project, repo) in [(llpc, owned), (ogl, borrowed)] {
        assert_eq!(
            layout::checkout_holding(&ws, project, repo, "topic").unwrap(),
            None,
            "{}",
            project.name()
        );
        assert_eq!(
            layout::work_dir(&ws, project, repo, "topic").unwrap(),
            trees.join("work"),
            "{}",
            project.name()
        );
    }
}

/// Nested repos live in `main`'s own `main_branch` checkout. When that branch is
/// checked out in the checkout a sibling places, `main` has none of its own, so
/// the bootstrap is where nested repos are looked for — the same answer
/// `checkout_holding` gives, which a nested repo's path is derived from.
#[test]
fn nested_repos_live_in_mains_own_checkout() {
    let root = tempfile::tempdir().unwrap();
    let (bare, config) = shared_host(root.path());
    let trees = root.path().join("trees");
    git(&bare, &["branch", "parked", "main"]);
    for (dir, branch) in [("main", "parked"), ("review", "main")] {
        let at = trees.join(dir);
        git(
            &bare,
            &["worktree", "add", "-q", at.to_str().unwrap(), branch],
        );
    }
    let file = config.join("llpc.toml");
    let mut body = std::fs::read_to_string(&file).unwrap();
    body.push_str("[repos.sub]\npath = \"sub\"\nmain_branch = \"main\"\nanchor = \"main\"\n");
    std::fs::write(&file, body).unwrap();
    let ws = Workspace::load_from(&config).unwrap();
    let project = ws.project("llpc").unwrap();
    let main = project.repo("main").unwrap();

    assert_eq!(
        layout::checkout_holding(&ws, project, main, "main").unwrap(),
        None
    );
    assert_eq!(
        layout::nesting_root(&ws, project, main).unwrap(),
        trees.join("main")
    );
    assert_eq!(
        layout::primary_path(&ws, project, project.repo("sub").unwrap()).unwrap(),
        trees.join("main/sub")
    );
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
