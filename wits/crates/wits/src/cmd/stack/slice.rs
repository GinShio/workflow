//! `wits stack slice` — cut the commits on top of a base into named branches.
//!
//! This is the only verb that authors local structure, and it leans on git to do
//! the dangerous part. We drive `git rebase -i` with a sequence editor of our
//! own that seeds the todo with the range's commits and, per commit, an
//! `update-ref` line tuned to what we know: a branch already in the stack is
//! pre-filled active so re-slicing keeps it in place, while everything else is a
//! commented suggestion (see [`build_todo`]). `update-ref` lands the pointers at
//! the *end* of the rebase, which is the only branch-assignment that is safe when
//! the branch in question is checked out or used by a worktree.
//!
//! The subtlety that dictates the design: after the rebase, scanning `base..HEAD`
//! to learn the assignments is unreliable, because when the current branch is one
//! of the intermediate update-ref targets git leaves HEAD short of the top. So we
//! capture the final todo the user saved and read the assignments straight from
//! it — that text is authoritative regardless of where HEAD ends up.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;

use wits_util::git::{Commit, Repository};
use wits_util::process::Command;

use super::resolution;
use super::topology::Topology;

pub fn run(repo: &Repository, base: Option<&str>) -> anyhow::Result<()> {
    // `slice` is driven by an interactive `git rebase -i`; there is nothing to
    // preview and no safe way to run it non-interactively, so under `-n` it does
    // nothing rather than write temp files and then misreport an empty result.
    if wits_util::log::is_dry_run() {
        log::info!(
            "slice drives an interactive rebase and cannot be previewed; skipping under --dry-run"
        );
        return Ok(());
    }

    let base = match base {
        Some(b) => b.to_owned(),
        None => resolution::base_branch(repo)?,
    };

    let commits = repo.commits(&format!("{base}..HEAD"));
    if commits.is_empty() {
        log::info!("no commits between {base} and HEAD; nothing to slice");
        return Ok(());
    }

    let prefix = stack_prefix(repo);
    let editor = resolve_editor(repo);

    // The existing forest and the branches at each commit drive the suggestions:
    // a branch already in the stack is pre-filled active (preserved in place), a
    // branch that merely points here is offered commented, and a commit with no
    // branch gets a commented slug.
    let mut topology = resolution::load_topology(repo)?;
    let branches_at = branches_by_commit(repo);
    let todo = build_todo(&commits, &prefix, &topology, &branches_at);

    // Three short-lived files: our seed todo, the editor wrapper script, and the
    // place the wrapper copies the user's final todo so we can read it back.
    // Prefer `$XDG_RUNTIME_DIR` — a per-user, 0700 tmpfs — over the world-writable
    // `/tmp`, so the wrapper script we create and then execute can't be raced or
    // symlink-swapped by another local user; fall back to the system temp dir
    // when the runtime dir isn't set.
    let tmp = runtime_dir();
    let unique = std::process::id();
    let content_path = tmp.join(format!("wits-slice-{unique}.todo"));
    let capture_path = tmp.join(format!("wits-slice-{unique}.capture"));
    let script_path = tmp.join(format!("wits-slice-{unique}.sh"));

    let cleanup = || {
        let _ = fs::remove_file(&content_path);
        let _ = fs::remove_file(&capture_path);
        let _ = fs::remove_file(&script_path);
    };

    fs::write(&content_path, &todo)?;
    fs::write(
        &script_path,
        format!(
            "#!/bin/sh\ncp \"{content}\" \"$1\"\n{editor} \"$1\"\ncp \"$1\" \"{capture}\"\n",
            content = content_path.display(),
            capture = capture_path.display(),
        ),
    )?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

    let code = Command::new("git")
        .args(["rebase", "-i", &base])
        .env("GIT_SEQUENCE_EDITOR", script_path.display().to_string())
        .status();

    let code = match code {
        Ok(code) => code,
        Err(e) => {
            cleanup();
            return Err(e.into());
        }
    };
    if code != 0 {
        cleanup();
        anyhow::bail!(
            "rebase did not complete (run `git rebase --abort` if it is still in progress)"
        );
    }

    let branches = chain_branches(parse_assignments(&capture_path), &base);
    cleanup();

    if branches.is_empty() {
        log::info!("no update-ref lines were uncommented; .git/machete left unchanged");
        return Ok(());
    }

    // Lay the discovered branches down as a linear chain on the base, leaving any
    // unrelated stacks in the file untouched. (The rebase doesn't touch machete,
    // so the topology loaded above is still current.)
    topology.ensure(&base);
    let mut parent = base.clone();
    for branch in &branches {
        topology.ensure(branch);
        topology.reparent(branch, &parent);
        parent = branch.clone();
    }
    resolution::save_topology(repo, &topology)?;

    log::info!("sliced into: {}", branches.join(", "));
    Ok(())
}

/// Map each commit hash to the local branches whose tip is exactly there, sorted
/// for a stable todo. Used to fill in `update-ref` suggestions intelligently.
fn branches_by_commit(repo: &Repository) -> HashMap<String, Vec<String>> {
    let mut by_commit: HashMap<String, Vec<String>> = HashMap::new();
    for (branch, sha) in repo.branch_tips() {
        by_commit.entry(sha).or_default().push(branch);
    }
    for names in by_commit.values_mut() {
        names.sort();
    }
    by_commit
}

/// Build the rebase todo: each commit as a `pick`, followed by one `update-ref`
/// line reflecting what we know about that commit.
///
/// The rule per commit, and the reason for each tier:
///   1. a branch already in the stack → an **active** line, so re-slicing keeps
///      it in place under its real name without retyping;
///   2. otherwise a branch that merely points here → a **commented** suggestion;
///   3. otherwise → a **commented** slug, the name to mint for fresh work.
///
/// Crucially, at most **one** line per commit is ever active. Several branches on
/// the same commit are not a fork (a fork diverges *later*); activating two would
/// make the linear record collapse them into a bogus parent→child chain (an empty
/// MR). So extra branches on a commit are demoted to commented suggestions.
fn build_todo(
    commits: &[Commit],
    prefix: &str,
    topology: &Topology,
    branches_at: &HashMap<String, Vec<String>>,
) -> String {
    let mut todo = String::new();
    for commit in commits {
        todo.push_str(&format!("pick {} {}\n", commit.hash, commit.subject));

        let here = branches_at.get(&commit.hash).cloned().unwrap_or_default();
        let (in_stack, others): (Vec<String>, Vec<String>) =
            here.into_iter().partition(|b| topology.contains(b));

        if let Some((primary, rest)) = in_stack.split_first() {
            todo.push_str(&format!("update-ref refs/heads/{primary}\n"));
            for other in rest.iter().chain(others.iter()) {
                todo.push_str(&format!("# update-ref refs/heads/{other}\n"));
            }
        } else if !others.is_empty() {
            for other in &others {
                todo.push_str(&format!("# update-ref refs/heads/{other}\n"));
            }
        } else {
            let slug = slugify(&commit.subject);
            todo.push_str(&format!("# update-ref refs/heads/{prefix}{slug}\n"));
        }
        todo.push('\n');
    }

    todo.push_str("# A line without '#' assigns a branch tip at that commit; refs are set at\n");
    todo.push_str("# the end of the rebase (safe for the current branch and for worktrees).\n");
    todo.push_str("# Active lines are branches already in your stack; commented lines are\n");
    todo.push_str("# suggestions — uncomment one to assign it, and edit the name to taste.\n");
    todo
}

/// Read the branch names the user actually committed to, in commit order, from
/// the saved todo — the authoritative source (see the module note).
fn parse_assignments(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let prefix = "update-ref refs/heads/";
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(prefix))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Clean the raw assignment list into the chain we will write: drop the base
/// itself (assigning it would be nonsense and would try to make it its own
/// parent) and collapse duplicates, which slug collisions on similar commit
/// subjects make entirely possible. Order is preserved — it is the commit order.
fn chain_branches(raw: Vec<String>, base: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|b| b != base && seen.insert(b.clone()))
        .collect()
}

/// The branch-name prefix for suggestions: an explicit setting
/// (`wits.stack.prefix`), else a slug of the user's name, else a neutral
/// `stack/`.
fn stack_prefix(repo: &Repository) -> String {
    if let Some(prefix) = repo.get_config("wits.stack.prefix").ok().flatten() {
        if !prefix.is_empty() {
            return prefix;
        }
    }
    if let Some(name) = repo.get_config("user.name").ok().flatten() {
        let slug = slugify(&name);
        if !slug.is_empty() {
            return format!("{slug}/");
        }
    }
    "stack/".to_owned()
}

/// The directory for our short-lived scratch files: `$XDG_RUNTIME_DIR` when set
/// (a per-user, 0700 runtime dir — the safe home for an executable we create and
/// run), otherwise the system temp dir (`/tmp`).
fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Resolve the editor the wrapper should open, following git's own precedence.
fn resolve_editor(repo: &Repository) -> String {
    std::env::var("GIT_EDITOR")
        .ok()
        .or_else(|| repo.get_config("core.editor").ok().flatten())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_owned())
}

/// Lowercase, collapse non-alphanumerics to single dashes, trim, cap length —
/// enough to turn a commit subject into a passable branch name.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    out.chars().take(50).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_makes_a_branch_safe_name() {
        assert_eq!(slugify("Add: the Foo  (v2)!"), "add-the-foo-v2");
        assert_eq!(slugify("   "), "");
    }

    #[test]
    fn chain_branches_drops_base_and_duplicates() {
        let raw = vec![
            "main".to_owned(), // the base, must be dropped
            "a".to_owned(),
            "b".to_owned(),
            "a".to_owned(), // a slug collision repeating an earlier name
        ];
        assert_eq!(chain_branches(raw, "main"), ["a", "b"]);
    }

    fn commit(hash: &str, subject: &str) -> Commit {
        Commit {
            hash: hash.into(),
            subject: subject.into(),
            body: String::new(),
        }
    }

    #[test]
    fn todo_tiers_machete_then_existing_then_slug() {
        let topology = Topology::parse("main\n    feat-a\n");
        let commits = [
            commit("aaa", "Add A"),
            commit("bbb", "Add B"),
            commit("ccc", "Add C"),
        ];
        let mut at = HashMap::new();
        at.insert("aaa".to_string(), vec!["feat-a".to_string()]); // in the stack → active
        at.insert("bbb".to_string(), vec!["random".to_string()]); // a branch, not in stack → commented
                                                                  // ccc has no branch → commented slug

        let todo = build_todo(&commits, "me/", &topology, &at);
        assert!(todo.contains("\nupdate-ref refs/heads/feat-a\n"));
        assert!(todo.contains("# update-ref refs/heads/random\n"));
        assert!(todo.contains("# update-ref refs/heads/me/add-c\n"));
    }

    #[test]
    fn todo_keeps_only_one_active_ref_per_commit() {
        // Two in-stack branches on the same commit must not both be activated —
        // that would record a bogus chain. One active, the other commented.
        let topology = Topology::parse("main\n    x\n    y\n");
        let commits = [commit("aaa", "Shared tip")];
        let mut at = HashMap::new();
        at.insert("aaa".to_string(), vec!["x".to_string(), "y".to_string()]);

        let todo = build_todo(&commits, "p/", &topology, &at);
        let active = todo
            .lines()
            .filter(|l| l.starts_with("update-ref "))
            .count();
        let commented = todo
            .lines()
            .filter(|l| l.starts_with("# update-ref "))
            .count();
        assert_eq!(active, 1);
        assert_eq!(commented, 1);
    }

    #[test]
    fn parse_assignments_reads_only_uncommented_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo");
        fs::write(
            &path,
            "pick abc subject\n# update-ref refs/heads/commented\nupdate-ref refs/heads/me/one\n\nupdate-ref refs/heads/me/two\n",
        )
        .unwrap();
        assert_eq!(parse_assignments(&path), ["me/one", "me/two"]);
    }
}
