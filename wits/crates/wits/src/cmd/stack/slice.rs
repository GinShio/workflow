//! `wits stack slice` — cut the commits on top of a base into named branches.
//!
//! This is the only verb that authors local structure, and git does all of the
//! commit movement. We run `git rebase -i` as its sequence editor, but the todo
//! the user edits is the one git generated — its commits and their order, the
//! fixups `rebase.autoSquash` folded in, the commits it dropped as already
//! upstream — with only our naming layered on (see [`seed`]). `--update-refs` is
//! forced so git itself writes the `update-ref` line of every branch already on
//! the range, after the fixups folded into that branch's commit, and withholds
//! the lines it must not write: the checked-out branch's, and those of branches
//! checked out in other worktrees.
//!
//! The editor is this binary again (`wits __slice-editor`, see [`edit_todo`]),
//! because weaving names into git's todo means reading it. [`run`] resolves
//! everything beforehand and hands it over in a spec file, so the editor process
//! only transforms text and opens the user's own editor.
//!
//! Two constraints shape the rest. The todo the user saved is captured and read
//! back as the authoritative list of assignments, because the refs cannot tell
//! it afterwards: a branch whose line was removed still points into the range
//! wherever the rebase left commits unrewritten. And the checked-out branch
//! takes no `update-ref` of its own — git moves it to the end of the rebase by
//! itself, and an explicit line fails git's final ref update as soon as the
//! rebase rewrote anything — so its line is shown and captured like any other,
//! then withheld from what git executes (see [`without_current`]).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use wits_util::git::{Commit, Repository};
use wits_util::process::Command;
use wits_util::remote::RemoteRoles;

use super::resolution;
use super::topology::Topology;

/// Explains our layer of the todo; git's own help block follows it.
const FOOTER: &[&str] = &[
    "# wits stack slice: each active update-ref line puts its branch in the stack,",
    "# in the order listed; commented lines are suggestions to uncomment or rename.",
    "# The current branch's line must stay after the last commit: git moves the",
    "# checked-out branch to the end of the rebase itself.",
];

pub fn run(repo: &Repository, roles: &RemoteRoles, base: Option<&str>) -> anyhow::Result<()> {
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
        None => resolution::base_branch(repo, roles)?,
    };

    let commits = repo.commits(&format!("{base}..HEAD"));
    if commits.is_empty() {
        log::info!("no commits between {base} and HEAD; nothing to slice");
        return Ok(());
    }

    let topology = resolution::load_topology(repo)?;
    let current = repo.current_branch();
    refuse_occupied(repo, &topology, &commits, current.as_deref())?;

    let editor = repo.sequence_editor().ok_or_else(|| {
        anyhow::anyhow!(
            "git has no editor to open the rebase todo in (set sequence.editor or core.editor)"
        )
    })?;
    let prefix = stack_prefix(repo);

    // The spec names the command the editor process runs, so it lives in a
    // directory only we can enter; the directory goes when this handle drops.
    let scratch = tempfile::Builder::new()
        .prefix("wits-slice-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .context("creating the slice scratch directory")?;
    let spec = Spec {
        editor,
        capture: scratch.path().join("todo"),
        current,
        stack: topology.all().iter().cloned().collect(),
        suggestions: commits
            .iter()
            .map(|commit| {
                let name = format!("{prefix}{}", slugify(&commit.subject));
                (commit.hash.clone(), name)
            })
            .collect(),
    };
    let spec_path = scratch.path().join("spec.json");
    fs::write(&spec_path, serde_json::to_vec(&spec)?)?;

    // `--update-refs` hands git the placement of every branch already on the
    // range (see the module note). `--no-rebase-merges` keeps the todo the linear
    // run of picks and fixups that `seed` reads, whatever the user's config says.
    let exe = std::env::current_exe().context("locating the wits binary to edit the todo")?;
    let code = Command::new("git")
        .args(["rebase", "-i", "--update-refs", "--no-rebase-merges", &base])
        .env(
            "GIT_SEQUENCE_EDITOR",
            format!(
                "{} __slice-editor {}",
                shell_quote(&exe),
                shell_quote(&spec_path)
            ),
        )
        .status()?;
    if code != 0 {
        anyhow::bail!(
            "rebase did not complete (run `git rebase --abort` if it is still in progress)"
        );
    }

    let saved = fs::read_to_string(&spec.capture).context("reading back the saved rebase todo")?;
    let branches = chain_branches(parse_assignments(&saved), &base);

    if branches.is_empty() {
        log::info!("no update-ref lines were uncommented; the machete file is unchanged");
        return Ok(());
    }

    // Lay the discovered branches down as a linear chain on the base, leaving any
    // unrelated stacks in the file untouched. The rebase ran for a while and the
    // forest may have changed underneath it (a branch deleted in another
    // terminal, the reference-transaction hook pruning), so the chain applies to
    // a fresh load under the machete lock, not to the pre-rebase snapshot.
    let _lock = resolution::MacheteLock::acquire(repo)?;
    let mut topology = resolution::load_topology(repo)?;
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

/// `wits __slice-editor <spec> <todo>`: the sequence editor [`run`] installs.
///
/// Seeds the todo git generated, opens the user's editor on it, captures what
/// they saved for [`run`] to read back, and leaves git the todo it should
/// execute. Any failure fails the editor, and git then aborts the rebase before
/// it has rewritten anything, restoring an autostash on the way out.
pub fn edit_todo(spec_path: &Path, todo: &Path) -> anyhow::Result<()> {
    let text = fs::read(spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec: Spec = serde_json::from_slice(&text)
        .with_context(|| format!("parsing {}", spec_path.display()))?;

    let generated = fs::read_to_string(todo)?;
    fs::write(todo, seed(&generated, &spec))?;

    // The way git itself runs an editor, so shell syntax in the setting still works.
    let script = format!("{} \"$@\"", spec.editor);
    let todo_arg = todo.display().to_string();
    let code = Command::new("sh")
        .args([
            "-c",
            script.as_str(),
            spec.editor.as_str(),
            todo_arg.as_str(),
        ])
        .status()?;
    anyhow::ensure!(code == 0, "the editor exited with status {code}");

    let saved = fs::read_to_string(todo)?;
    fs::write(&spec.capture, &saved)?;
    if let Some(current) = &spec.current {
        fs::write(todo, without_current(&saved, current)?)?;
    }
    Ok(())
}

/// What [`run`] hands its editor process: everything seeding needs, so that
/// process reads nothing but this and the todo.
#[derive(Serialize, Deserialize)]
struct Spec {
    /// The user's editor, resolved before we took `GIT_SEQUENCE_EDITOR` over.
    editor: String,
    /// Where the saved todo is copied for [`run`] to read back.
    capture: PathBuf,
    /// The checked-out branch; `None` on a detached HEAD.
    current: Option<String>,
    /// Every branch the machete file records.
    stack: HashSet<String>,
    /// The name to suggest for each commit of the range, keyed by full hash.
    suggestions: HashMap<String, String>,
}

impl Spec {
    /// The suggestion for the commit a todo line abbreviates as `hash`. Git's
    /// abbreviations are unique across the repository, so at most one commit of
    /// the range matches.
    fn suggestion(&self, hash: &str) -> Option<&str> {
        self.suggestions
            .iter()
            .find(|(full, _)| full.starts_with(hash))
            .map(|(_, name)| name.as_str())
    }
}

/// A todo line, reduced to what seeding and reading it back need. Only the
/// command word and its first operand are read — the grammar git-rebase(1)
/// documents, which git must parse from hand edits too — never the subject text
/// after them, whose presentation git varies (recent versions write
/// `pick <hash> # <subject>`).
enum Line<'a> {
    /// Starts a commit of the rewritten history: `pick`, `reword`, `edit`.
    Pick(&'a str),
    /// Adds to the commit being built: `fixup` (with or without `-C`/`-c`),
    /// `squash`.
    Fold,
    /// Records a merge commit of its own.
    Merge,
    /// `update-ref <ref>`: points the ref at the commit built so far.
    UpdateRef(&'a str),
    /// Comments, blank lines, and every command that commits nothing.
    Other,
}

impl<'a> Line<'a> {
    fn parse(line: &'a str) -> Self {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("pick" | "p" | "reword" | "r" | "edit" | "e") => {
                words.next().map_or(Line::Other, Line::Pick)
            }
            Some("fixup" | "f" | "squash" | "s") => Line::Fold,
            Some("merge" | "m") => Line::Merge,
            Some("update-ref" | "u") => words.next().map_or(Line::Other, Line::UpdateRef),
            _ => Line::Other,
        }
    }

    /// Whether executing this line adds a commit to the rewritten history.
    fn commits(&self) -> bool {
        matches!(self, Line::Pick(_) | Line::Fold | Line::Merge)
    }

    /// The branch an `update-ref` line points; `None` for any other line or ref.
    fn branch(&self) -> Option<&'a str> {
        match self {
            Line::UpdateRef(target) => target.strip_prefix("refs/heads/"),
            _ => None,
        }
    }
}

/// The todo the user edits: every command git wrote, in git's order, with our
/// naming layered on at each *position* — the point after a pick and the fixups
/// folded into it, where git put the `update-ref` lines of the branches already
/// there. At each position:
///
///   1. one of git's branches that is already in the stack stays active, and the
///      rest are commented, so re-slicing keeps the stack in place without
///      retyping yet never activates two lines at once — two branches on one
///      commit are not a fork, and activating both would record a bogus
///      parent→child chain (an empty MR);
///   2. a position git named no branch at gets a commented `<prefix><slug>`
///      suggestion, the name to mint for fresh work;
///   3. the last position also carries the checked-out branch, which git leaves
///      out because the rebase moves it there by itself; that line is active
///      when the branch is already in the stack, ahead of any other stack
///      branch there.
///
/// Git's blank lines inside the command list are dropped for one blank line per
/// position; blank lines mean nothing to git.
fn seed(todo: &str, spec: &Spec) -> String {
    let lines: Vec<&str> = todo.lines().collect();
    // Git closes the todo with a help block of comments; our lines go above it.
    let end = lines
        .iter()
        .rposition(|line| {
            let line = line.trim_start();
            !line.is_empty() && !line.starts_with('#')
        })
        .map_or(0, |last| last + 1);

    let mut out = Vec::new();
    let mut position: Option<Position> = None;
    for line in &lines[..end] {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = Line::parse(line);
        if let (Some(open), Some(branch)) = (position.as_mut(), parsed.branch()) {
            open.branches.push(branch);
            continue;
        }
        if let Line::Pick(hash) = parsed {
            let opened = Position {
                pick: hash,
                branches: Vec::new(),
            };
            if let Some(done) = position.replace(opened) {
                done.close(spec, None, &mut out);
                out.push(String::new());
            }
        }
        out.push((*line).to_owned());
    }
    if let Some(last) = position {
        last.close(spec, spec.current.as_deref(), &mut out);
    }
    out.push(String::new());
    out.extend(FOOTER.iter().map(|line| (*line).to_owned()));
    out.extend(lines[end..].iter().map(|line| (*line).to_owned()));
    out.join("\n") + "\n"
}

/// One position of the todo being seeded: the pick that opened it, and the
/// branches git wrote an `update-ref` line for before the next one.
struct Position<'a> {
    pick: &'a str,
    branches: Vec<&'a str>,
}

impl Position<'_> {
    /// Emit this position's naming lines per [`seed`]'s rules. `current` is the
    /// checked-out branch, passed for the last position only.
    fn close(self, spec: &Spec, current: Option<&str>, out: &mut Vec<String>) {
        let in_stack = |name: &str| spec.stack.contains(name);
        let active = current.filter(|name| in_stack(name)).or_else(|| {
            self.branches
                .iter()
                .copied()
                .filter(|name| in_stack(name))
                .min()
        });
        let named: Vec<&str> = self.branches.iter().copied().chain(current).collect();
        for &name in &named {
            out.push(update_ref(name, Some(name) == active));
        }
        if named.is_empty() {
            if let Some(name) = spec.suggestion(self.pick) {
                out.push(update_ref(name, false));
            }
        }
    }
}

/// An `update-ref` line for `branch`, active or commented out.
fn update_ref(branch: &str, active: bool) -> String {
    let line = format!("update-ref refs/heads/{branch}");
    if active {
        line
    } else {
        format!("# {line}")
    }
}

/// The todo git should execute: `saved` without the checked-out branch's
/// `update-ref` lines, whose ref update fails once the rebase rewrites anything
/// (see the module note). Refused when such a line precedes a commit: the
/// branch would be recorded mid-stack while git leaves it at the end.
fn without_current(saved: &str, current: &str) -> anyhow::Result<String> {
    let lines: Vec<&str> = saved.lines().collect();
    let last_commit = lines.iter().rposition(|line| Line::parse(line).commits());
    let mut kept = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if Line::parse(line).branch() == Some(current) {
            anyhow::ensure!(
                last_commit.is_none_or(|last| index > last),
                "keep the update-ref line of the current branch '{current}' after the last \
                 commit: git moves the checked-out branch to the end of the rebase itself"
            );
            continue;
        }
        kept.push(*line);
    }
    Ok(kept.join("\n") + "\n")
}

/// Read the branch names the user actually committed to, in todo order, from
/// the saved todo — the authoritative source (see the module note).
fn parse_assignments(todo: &str) -> Vec<String> {
    todo.lines()
        .filter_map(|line| Line::parse(line).branch())
        .map(str::to_owned)
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

/// Refuse to slice while a stack branch on the range is checked out in another
/// worktree. Git's `--update-refs` leaves such a branch where it is, since moving
/// it would leave that worktree's index and files describing the old commit; the
/// slice could then neither move the branch nor record it, and the chain written
/// would skip it.
fn refuse_occupied(
    repo: &Repository,
    stack: &Topology,
    range: &[Commit],
    current: Option<&str>,
) -> anyhow::Result<()> {
    let in_range: HashSet<&str> = range.iter().map(|commit| commit.hash.as_str()).collect();
    let tips = repo.branch_tips();
    for worktree in repo.worktrees() {
        let Some(branch) = worktree.branch.as_deref() else {
            continue;
        };
        let on_range = tips
            .get(branch)
            .is_some_and(|tip| in_range.contains(tip.as_str()));
        if Some(branch) != current && stack.contains(branch) && on_range {
            anyhow::bail!(
                "stack branch '{branch}' is checked out in {}, and git will not move it during \
                 the rebase; switch that worktree to another branch first",
                worktree.path.display()
            );
        }
    }
    Ok(())
}

/// Quote `path` for the `sh -c` git runs its sequence editor through.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
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

    fn spec(stack: &[&str], current: Option<&str>) -> Spec {
        Spec {
            editor: String::new(),
            capture: PathBuf::new(),
            current: current.map(str::to_owned),
            stack: stack.iter().map(|name| (*name).to_owned()).collect(),
            suggestions: [
                ("188d5fae0086", "me/add-a"),
                ("5f9b18c3b336", "me/fixup-add-a"),
                ("34b141dfd101", "me/add-b"),
                ("f3031363c52d", "me/add-c"),
            ]
            .into_iter()
            .map(|(hash, name)| (hash.to_owned(), name.to_owned()))
            .collect(),
        }
    }

    const HELP: &str = "# Rebase 17b83ac..5f9b18c onto 17b83ac (4 commands)\n#\n# Commands:\n";

    /// The command list of a seeded todo: everything above our footer.
    fn commands(seeded: &str) -> &str {
        let footer = seeded
            .find(FOOTER[0])
            .expect("the footer is always written");
        &seeded[..footer]
    }

    #[test]
    fn suggestions_land_after_the_fixups_folded_into_a_pick() {
        // What `git rebase -i --autosquash` writes for A, B, C and `fixup! A` on
        // top, with no branch on the range but the checked-out one.
        let todo = format!(
            "pick 188d5fa # Add A\nfixup 5f9b18c # fixup! Add A\npick 34b141d # Add B\n\
             pick f303136 # Add C\n\n{HELP}"
        );
        let seeded = seed(&todo, &spec(&[], Some("work")));
        assert_eq!(
            commands(&seeded),
            "pick 188d5fa # Add A\nfixup 5f9b18c # fixup! Add A\n\
             # update-ref refs/heads/me/add-a\n\n\
             pick 34b141d # Add B\n# update-ref refs/heads/me/add-b\n\n\
             pick f303136 # Add C\n# update-ref refs/heads/work\n\n"
        );
        assert!(
            seeded.ends_with(&format!("\n\n{HELP}")),
            "git's help block survives"
        );
    }

    #[test]
    fn stack_branches_stay_active_where_git_placed_them() {
        // Git's own todo for the same range once feat-a and feat-b are stacked:
        // feat-a's line already sits after the fixup, and feat-c, checked out,
        // has none.
        let todo = format!(
            "pick 188d5fa # Add A\nfixup 5f9b18c # fixup! Add A\n\
             update-ref refs/heads/feat-a\n\npick 34b141d # Add B\n\
             update-ref refs/heads/feat-b\n\npick f303136 # Add C\n\n{HELP}"
        );
        let stack = ["main", "feat-a", "feat-b", "feat-c"];
        let seeded = seed(&todo, &spec(&stack, Some("feat-c")));
        assert_eq!(
            commands(&seeded),
            "pick 188d5fa # Add A\nfixup 5f9b18c # fixup! Add A\n\
             update-ref refs/heads/feat-a\n\n\
             pick 34b141d # Add B\nupdate-ref refs/heads/feat-b\n\n\
             pick f303136 # Add C\nupdate-ref refs/heads/feat-c\n\n"
        );
    }

    #[test]
    fn one_line_per_position_stays_active() {
        // A branch on a fixup commit is placed by git at that commit's original
        // spot, which can coincide with another branch: here `mid` sat on a
        // `fixup! A` committed after B.
        let todo = format!(
            "pick 188d5fa # Add A\nfixup 5f9b18c # fixup! Add A\n\
             update-ref refs/heads/feat-a\n\npick 34b141d # Add B\n\
             update-ref refs/heads/feat-b\n\nupdate-ref refs/heads/mid\n\n\
             pick f303136 # Add C\n\n{HELP}"
        );
        let stack = ["feat-a", "feat-b", "mid"];
        let seeded = seed(&todo, &spec(&stack, None));
        assert!(commands(&seeded).contains(
            "pick 34b141d # Add B\nupdate-ref refs/heads/feat-b\n\
             # update-ref refs/heads/mid\n\n"
        ));
    }

    #[test]
    fn a_branch_outside_the_stack_is_only_suggested() {
        let todo = format!("pick 188d5fa # Add A\nupdate-ref refs/heads/random\n\n{HELP}");
        let seeded = seed(&todo, &spec(&[], None));
        assert_eq!(
            commands(&seeded),
            "pick 188d5fa # Add A\n# update-ref refs/heads/random\n\n"
        );
    }

    #[test]
    fn the_current_branch_line_is_withheld_from_git() {
        let saved = "pick 188d5fa # Add A\nupdate-ref refs/heads/feat-a\n\n\
                     pick 34b141d # Add B\nupdate-ref refs/heads/feat-c\n\n# help\n";
        assert_eq!(
            without_current(saved, "feat-c").unwrap(),
            "pick 188d5fa # Add A\nupdate-ref refs/heads/feat-a\n\npick 34b141d # Add B\n\n# help\n"
        );
    }

    #[test]
    fn the_current_branch_cannot_be_assigned_mid_stack() {
        let saved = "pick 188d5fa # Add A\nupdate-ref refs/heads/feat-c\n\n\
                     pick 34b141d # Add B\n";
        assert!(without_current(saved, "feat-c").is_err());
    }

    #[test]
    fn parse_assignments_reads_only_active_lines() {
        let saved = "pick abc # subject\n# update-ref refs/heads/commented\n\
                     update-ref refs/heads/me/one\n\nu refs/heads/me/two\n\
                     update-ref refs/tags/not-a-branch\n";
        assert_eq!(parse_assignments(saved), ["me/one", "me/two"]);
    }
}
