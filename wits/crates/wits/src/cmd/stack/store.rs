//! Where a stack lives: in the git config of each of its branches.
//!
//! A branch in a stack carries, in its own `branch.<name>` section,
//! `witsParent` — the branch it sits on — and `witsOrder`, its place among that
//! parent's children; and `witsMr`, the MR it was last seen with, kept as a
//! cache for display (the forge stays the source of truth). The section is the
//! point: `git branch -m` moves it with the branch and `git branch -d`/`-D`
//! deletes it, so the stack follows a rename and forgets a deleted branch with no
//! hook and no file to keep in step. A shared file needed exactly those — a
//! cleanup hook, a lock against it, and a rename verb for the one event the hook
//! could not follow — and they went with it. Every worktree reads the
//! repository's one config, so the stack is repository-wide as before.
//!
//! What git does not carry along is the *other* end of a link: renaming or
//! deleting a parent leaves its children naming a branch that is gone. Such a
//! child is placed by history instead, under the deepest stack branch whose own
//! place is intact and whose tip is an ancestor of the child's, else on the base.
//! That finds a renamed parent, which kept its commit, and splices a deleted
//! one's children up to where `tree rm` would put them. Loading records these
//! placements without writing them; the next save writes them down.

use std::collections::{BTreeMap, HashMap, HashSet};

use wits_util::git::Repository;

use super::topology::Topology;

/// The keys, as written; git compares key names case-insensitively and reads
/// them back lowercased.
const PARENT: &str = "witsParent";
const ORDER: &str = "witsOrder";
const MR: &str = "witsMr";

/// One branch's stack keys as config holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Entry {
    parent: Option<String>,
    order: Option<String>,
    mr: Option<String>,
}

/// A child whose recorded parent is gone, and where history placed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rehomed {
    pub branch: String,
    /// The parent its config names, which no longer exists.
    pub recorded: String,
    /// Where it sits now.
    pub parent: String,
}

/// The stack as loaded: the forest, the placements history made, and the config
/// as read, against which a save computes its writes.
pub struct Stored {
    pub topology: Topology,
    pub rehomed: Vec<Rehomed>,
    entries: BTreeMap<String, Entry>,
}

/// Load the stack of `repo`, whose base branch is `base`. A repository with no
/// stack loads as an empty forest.
pub fn load(repo: &Repository, base: &str) -> Stored {
    let entries = read_entries(repo);
    let tips = repo.branch_tips();

    // A section whose branch has no ref is a leftover — git deletes the section
    // with the branch, but a ref removed by plumbing leaves it — and is no part of
    // the stack. A save clears it.
    let mut parents: BTreeMap<String, String> = entries
        .iter()
        .filter(|(branch, _)| tips.contains_key(*branch))
        .filter_map(|(branch, entry)| Some((branch.clone(), entry.parent.clone()?)))
        .collect();
    let exists = |name: &str| name == base || tips.contains_key(name);
    let dangling: Vec<String> = parents
        .iter()
        .filter(|(_, parent)| !exists(parent))
        .map(|(branch, _)| branch.clone())
        .collect();
    let rehomed = place_by_history(repo, base, &tips, &parents, &dangling);
    for placed in &rehomed {
        parents.insert(placed.branch.clone(), placed.parent.clone());
    }

    // Children are attached in (order, name) order, which is the order the forest
    // keeps them in: `reparent` appends, per parent.
    let mut links: Vec<(&String, &String)> = parents.iter().collect();
    let order_of = |branch: &str| {
        entries
            .get(branch)
            .and_then(|e| e.order.as_deref())
            .and_then(|o| o.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    };
    links.sort_by(|(a, _), (b, _)| order_of(a).cmp(&order_of(b)).then(a.cmp(b)));

    let mut topology = Topology::default();
    if !parents.is_empty() {
        topology.ensure(base);
    }
    for (branch, parent) in links {
        topology.ensure(parent);
        topology.ensure(branch);
        if !topology.reparent(branch, parent) {
            log::warn!(
                "{branch}: its recorded parent '{parent}' sits on top of it; treating it as a \
                 root until `wits stack tree mv` places it"
            );
        }
    }
    for (branch, entry) in &entries {
        if let (true, Some(mr)) = (topology.contains(branch), &entry.mr) {
            topology.set_annotation(branch, mr.clone());
        }
    }

    Stored {
        topology,
        rehomed,
        entries,
    }
}

/// Write `after` over the stack `stored` was loaded from: every stack branch's
/// parent, its place among its siblings, and its cached MR, and nothing for a
/// branch that has left the stack. Only keys whose value changes are written,
/// so a save of an unchanged forest touches nothing. Placements loading made by
/// history are written here, as is the removal of a gone branch's leftover keys.
pub fn save(repo: &Repository, stored: &Stored, after: &Topology) -> anyhow::Result<()> {
    let mut wanted: BTreeMap<String, Entry> = BTreeMap::new();
    for name in after.all() {
        let Some(parent) = after.parent(name) else {
            continue;
        };
        let order = after
            .children(parent)
            .iter()
            .position(|child| child == name)
            .expect("a node is among its parent's children");
        let mr = after.annotation(name).filter(|mr| !mr.is_empty());
        wanted.insert(
            name.clone(),
            Entry {
                parent: Some(parent.to_owned()),
                order: Some(order.to_string()),
                mr: mr.map(str::to_owned),
            },
        );
    }

    let none = Entry::default();
    let branches: HashSet<&String> = wanted.keys().chain(stored.entries.keys()).collect();
    let mut branches: Vec<&String> = branches.into_iter().collect();
    branches.sort();
    for branch in branches {
        let have = stored.entries.get(branch).unwrap_or(&none);
        let want = wanted.get(branch).unwrap_or(&none);
        for (key, have, want) in [
            (PARENT, &have.parent, &want.parent),
            (ORDER, &have.order, &want.order),
            (MR, &have.mr, &want.mr),
        ] {
            let name = format!("branch.{branch}.{key}");
            match (have, want) {
                (Some(have), Some(want)) if have == want => {}
                (_, Some(want)) => repo.set_config(&name, want)?,
                (Some(_), None) => repo.unset_config(&name)?,
                (None, None) => {}
            }
        }
    }
    for placed in &stored.rehomed {
        if after.parent(&placed.branch) == Some(placed.parent.as_str()) {
            log::info!(
                "{}: recorded on {} (its parent {} is gone)",
                placed.branch,
                placed.parent,
                placed.recorded
            );
        }
    }
    Ok(())
}

/// Every branch's stack keys, read in one call. Branch names may hold dots, so
/// the key is split off at the last one.
fn read_entries(repo: &Repository) -> BTreeMap<String, Entry> {
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    for (name, value) in repo.config_entries(r"^branch\..+\.wits(parent|order|mr)$") {
        let Some(rest) = name.strip_prefix("branch.") else {
            continue;
        };
        let Some((branch, key)) = rest.rsplit_once('.') else {
            continue;
        };
        let entry = entries.entry(branch.to_owned()).or_default();
        // A key repeated across config files reads once per file; the last one
        // read is the one git itself would answer with.
        let slot = if key.eq_ignore_ascii_case(PARENT) {
            &mut entry.parent
        } else if key.eq_ignore_ascii_case(ORDER) {
            &mut entry.order
        } else if key.eq_ignore_ascii_case(MR) {
            &mut entry.mr
        } else {
            continue;
        };
        *slot = Some(value);
    }
    entries
}

/// Place each branch in `dangling` (whose recorded parent is gone) by history:
/// under the deepest stack branch with an intact place whose tip is an ancestor
/// of its own, else on `base`. Branches below it in the forest are never
/// candidates, which is what keeps a placement from making a cycle; nor are the
/// other dangling branches, which keeps siblings left by one deleted parent from
/// being stacked onto each other.
fn place_by_history(
    repo: &Repository,
    base: &str,
    tips: &HashMap<String, String>,
    parents: &BTreeMap<String, String>,
    dangling: &[String],
) -> Vec<Rehomed> {
    let lost: HashSet<&str> = dangling.iter().map(String::as_str).collect();
    let depth = |branch: &str| {
        let mut depth = 0;
        let mut cur = branch;
        while let Some(parent) = parents.get(cur).filter(|_| !lost.contains(cur)) {
            depth += 1;
            cur = parent;
            if depth > parents.len() {
                break;
            }
        }
        depth
    };
    let below = |branch: &str, ancestor: &str| {
        let mut cur = branch;
        let mut steps = 0;
        while let Some(parent) = parents.get(cur) {
            if parent == ancestor {
                return true;
            }
            cur = parent;
            steps += 1;
            if steps > parents.len() {
                break;
            }
        }
        false
    };

    let mut placed = Vec::new();
    for branch in dangling {
        let tip = &tips[branch];
        let mut best: Option<(&String, usize)> = None;
        for candidate in parents.keys() {
            if candidate == branch || lost.contains(candidate.as_str()) || below(candidate, branch)
            {
                continue;
            }
            let candidate_tip = &tips[candidate];
            if !repo.is_ancestor(candidate_tip, tip) {
                continue;
            }
            let deeper = match best {
                None => true,
                Some((current, current_depth)) => {
                    let current_tip = &tips[current];
                    if candidate_tip == current_tip {
                        depth(candidate) > current_depth
                    } else {
                        repo.is_ancestor(current_tip, candidate_tip)
                    }
                }
            };
            if deeper {
                best = Some((candidate, depth(candidate)));
            }
        }
        let parent = best.map_or_else(|| base.to_owned(), |(found, _)| found.clone());
        placed.push(Rehomed {
            branch: branch.clone(),
            recorded: parents[branch].clone(),
            parent,
        });
    }
    placed
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let mut all = vec![
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e.com",
            "-c",
            "core.hooksPath=/nonexistent-wits-test-hooks",
        ];
        all.extend_from_slice(args);
        wits_util::process::Command::new("git")
            .args(all)
            .current_dir(dir)
            .force_run()
            .exec()
            .unwrap();
    }

    /// `main` → `a` → `b`, each with a commit of its own, and `c` forking off
    /// `a`.
    fn stacked() -> (tempfile::TempDir, Repository) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main", "."]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
        for (branch, from) in [("a", "main"), ("b", "a"), ("c", "a")] {
            git(dir, &["switch", "-q", "-c", branch, from]);
            git(dir, &["commit", "-q", "--allow-empty", "-m", branch]);
        }
        git(dir, &["switch", "-q", "main"]);
        let repo = Repository::new(dir);
        (tmp, repo)
    }

    fn save_text(repo: &Repository, text: &str) {
        let stored = load(repo, "main");
        save(repo, &stored, &Topology::parse(text)).unwrap();
    }

    #[test]
    fn a_saved_forest_loads_back_with_its_order_and_annotations() {
        let (_tmp, repo) = stacked();
        let text = "main\n    a PR #1\n        c\n        b PR #2\n";
        save_text(&repo, text);
        let stored = load(&repo, "main");
        assert_eq!(stored.topology.render(), text);
        assert!(stored.rehomed.is_empty());
        assert_eq!(
            repo.get_config("branch.b.witsParent").unwrap().as_deref(),
            Some("a")
        );
    }

    #[test]
    fn saving_an_unchanged_forest_writes_nothing() {
        let (tmp, repo) = stacked();
        save_text(&repo, "main\n    a\n        b\n");
        let config = tmp.path().join(".git/config");
        let before = std::fs::read_to_string(&config).unwrap();
        save_text(&repo, "main\n    a\n        b\n");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    }

    #[test]
    fn a_branch_that_leaves_the_forest_loses_its_keys() {
        let (_tmp, repo) = stacked();
        save_text(&repo, "main\n    a PR #1\n        b\n");
        save_text(&repo, "main\n    b\n");
        assert_eq!(repo.get_config("branch.a.witsParent").unwrap(), None);
        assert_eq!(repo.get_config("branch.a.witsMr").unwrap(), None);
        assert_eq!(
            repo.get_config("branch.b.witsParent").unwrap().as_deref(),
            Some("main")
        );
    }

    #[test]
    fn a_rename_is_followed_by_git_and_its_children_by_history() {
        let (tmp, repo) = stacked();
        save_text(&repo, "main\n    a PR #1\n        b\n        c\n");
        git(tmp.path(), &["branch", "-q", "-m", "a", "a2"]);

        let stored = load(&repo, "main");
        assert_eq!(
            stored.topology.render(),
            "main\n    a2 PR #1\n        b\n        c\n"
        );
        assert_eq!(stored.rehomed.len(), 2);
        assert!(stored
            .rehomed
            .iter()
            .all(|r| r.recorded == "a" && r.parent == "a2"));
        // A save writes the placements down, after which none are left to make.
        save(&repo, &stored, &stored.topology).unwrap();
        assert!(load(&repo, "main").rehomed.is_empty());
    }

    #[test]
    fn a_deleted_parents_children_splice_up_to_its_parent() {
        let (tmp, repo) = stacked();
        save_text(&repo, "main\n    a\n        b\n");
        git(tmp.path(), &["branch", "-q", "-D", "a"]);

        let stored = load(&repo, "main");
        assert_eq!(stored.topology.render(), "main\n    b\n");
        // Git took a's section with it.
        assert_eq!(repo.get_config("branch.a.witsParent").unwrap(), None);
    }

    #[test]
    fn siblings_left_by_one_deleted_parent_are_not_stacked_on_each_other() {
        // b and c both sit on a, and c has no commit beyond a's — an ancestor of b.
        let (tmp, repo) = stacked();
        git(tmp.path(), &["branch", "-f", "c", "a"]);
        save_text(&repo, "main\n    a\n        b\n        c\n");
        git(tmp.path(), &["branch", "-q", "-D", "a"]);

        let stored = load(&repo, "main");
        assert_eq!(stored.topology.parent("b"), Some("main"));
        assert_eq!(stored.topology.parent("c"), Some("main"));
    }

    #[test]
    fn keys_of_a_branch_without_a_ref_are_ignored_and_cleared() {
        let (tmp, repo) = stacked();
        save_text(&repo, "main\n    a\n        b\n");
        // A ref removed by plumbing leaves its config section behind.
        git(tmp.path(), &["update-ref", "-d", "refs/heads/b"]);

        let stored = load(&repo, "main");
        assert!(!stored.topology.contains("b"));
        save(&repo, &stored, &stored.topology).unwrap();
        assert_eq!(repo.get_config("branch.b.witsParent").unwrap(), None);
    }

    #[test]
    fn a_repository_with_no_stack_loads_empty() {
        let (_tmp, repo) = stacked();
        assert!(load(&repo, "main").topology.all().is_empty());
    }
}
