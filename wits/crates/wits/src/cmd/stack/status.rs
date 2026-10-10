//! `wits stack status` — the stack as it stands, and what each branch waits on.
//!
//! The view the other verbs act on: every stack branch in its place in the
//! forest, with the facts that say which verb it is waiting for. Not pushed, or
//! not matching its push: `push`. Its parent moved past it: a rebase onto the
//! parent. No open MR, or one based elsewhere than the stack says: `submit`. Its
//! MR merged or closed: the branch is done, and deleting it is all the cleanup
//! the stack needs. Local facts come from git; MR facts from the forge, one
//! lookup per branch in parallel, or with `--offline` from the MR each branch
//! last saw.
//!
//! `--json` prints the same rows as data, so a picker (fzf, an editor) can be
//! built on them without parsing the table.

use std::fmt::Write as _;
use std::io::IsTerminal as _;

use serde::Serialize;
use wits_util::forge::{HeadRepo, MergeRequest, MrState};
use wits_util::git::Repository;
use wits_util::project::remotes::Declared;

use super::topology::Topology;
use super::{map_parallel, resolution, store, BranchMrs, ForgeSession, StatusArgs};

#[derive(Debug, Serialize)]
struct Report {
    /// The base branch every stack sits on.
    base: String,
    /// The revision the base is measured at, when known.
    trunk: Option<String>,
    /// The remote branches are pushed to (the `origin` role).
    push_remote: Option<String>,
    /// Whether MR facts came from the forge (`false`: cached, or none).
    online: bool,
    branches: Vec<Row>,
}

#[derive(Debug, Serialize)]
struct Row {
    name: String,
    parent: String,
    /// Levels below the root of its tree, 1 for a branch on the base.
    depth: usize,
    /// The branch checked out here.
    current: bool,
    /// `false` for the checked-out branch when the stack does not record it:
    /// a one-branch stack, as every verb treats it.
    in_stack: bool,
    push: Push,
    /// Commits the parent has that this branch lacks, when the parent is a
    /// stack branch that moved past it.
    restack: Option<u32>,
    mr: Option<Mr>,
    /// The MR the branch was last seen with, as cached.
    cached_mr: Option<String>,
    /// The parent the stack recorded, when it is gone and history placed the
    /// branch instead.
    placed_by_history: Option<String>,
    /// The verbs this branch waits on, in the order they apply.
    needs: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Push {
    state: PushState,
    ahead: u32,
    behind: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum PushState {
    /// The remote branch is the local one.
    Pushed,
    /// Local commits the remote lacks.
    Ahead,
    /// Remote commits the local branch lacks.
    Behind,
    /// Both.
    Diverged,
    /// No remote branch.
    Unpushed,
    /// No push remote to compare with.
    Unknown,
}

#[derive(Debug, Serialize)]
struct Mr {
    display: String,
    state: &'static str,
    url: String,
    /// The base the MR targets.
    base: String,
    /// The base the stack says it should target.
    wants_base: String,
}

pub fn run(repo: &Repository, declared: &Declared, args: &StatusArgs) -> anyhow::Result<()> {
    let base = resolution::base_branch(repo, declared)?;
    let trunk = declared.trunk(repo).and_then(|trunk| trunk.rev);
    let stored = store::load(repo, &base);
    let topology = &stored.topology;
    let current = repo.current_branch();
    let push_remote = declared.roles.origin().map(str::to_owned);

    let mut names: Vec<String> = match &args.branch {
        Some(branch) => {
            if !topology.contains(branch) {
                anyhow::bail!("'{branch}' is not in the stack");
            }
            topology.whole_stack(branch, &base)
        }
        None => topology
            .roots()
            .iter()
            .flat_map(|root| topology.subtree(root))
            .collect(),
    };
    names.retain(|name| topology.parent(name).is_some());
    let outside = current
        .clone()
        .filter(|branch| args.branch.is_none() && *branch != base && !topology.contains(branch));

    let mut rows: Vec<Row> = names
        .iter()
        .map(|name| local_row(repo, topology, &base, name, push_remote.as_deref(), &stored))
        .collect();
    if let Some(branch) = &outside {
        let mut row = local_row(
            repo,
            topology,
            &base,
            branch,
            push_remote.as_deref(),
            &stored,
        );
        row.in_stack = false;
        row.parent = base.clone();
        rows.push(row);
    }
    for row in &mut rows {
        row.current = current.as_deref() == Some(row.name.as_str());
    }

    let online = !args.offline && attach_mrs(repo, declared, &mut rows);
    for row in &mut rows {
        row.needs = needs(row, online);
    }

    let report = Report {
        base,
        trunk,
        push_remote,
        online,
        branches: rows,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report, topology, Style::detect()));
    }
    Ok(())
}

/// A row's git facts: where it sits, how it stands against its push, and
/// whether its parent moved past it.
fn local_row(
    repo: &Repository,
    topology: &Topology,
    base: &str,
    name: &str,
    push_remote: Option<&str>,
    stored: &store::Stored,
) -> Row {
    let parent = topology.parent(name).unwrap_or(base).to_owned();
    let push = push_state(repo, name, push_remote);
    // Only a parent that is itself a stack branch can leave a child needing a
    // rebase onto it; the base moving on is how a trunk behaves.
    let restack =
        (topology.parent(&parent).is_some() && !repo.is_ancestor(&parent, name)).then(|| {
            repo.ahead_behind(&parent, name)
                .map_or(0, |(ahead, _)| ahead)
        });
    Row {
        name: name.to_owned(),
        depth: topology.ancestors(name).len(),
        parent,
        current: false,
        in_stack: true,
        push,
        restack,
        mr: None,
        cached_mr: topology
            .annotation(name)
            .filter(|mr| !mr.is_empty())
            .map(str::to_owned),
        placed_by_history: stored
            .rehomed
            .iter()
            .find(|placed| placed.branch == name)
            .map(|placed| placed.recorded.clone()),
        needs: Vec::new(),
    }
}

fn push_state(repo: &Repository, branch: &str, remote: Option<&str>) -> Push {
    let unknown = |state| Push {
        state,
        ahead: 0,
        behind: 0,
    };
    let Some(remote) = remote else {
        return unknown(PushState::Unknown);
    };
    let tracking = format!("refs/remotes/{remote}/{branch}");
    if !repo.rev_exists(&tracking) {
        return unknown(PushState::Unpushed);
    }
    let Some((ahead, behind)) = repo.ahead_behind(branch, &tracking) else {
        return unknown(PushState::Unknown);
    };
    let state = match (ahead, behind) {
        (0, 0) => PushState::Pushed,
        (_, 0) => PushState::Ahead,
        (0, _) => PushState::Behind,
        _ => PushState::Diverged,
    };
    Push {
        state,
        ahead,
        behind,
    }
}

/// Look up every row's MR on the forge, in parallel. `false` — leaving the rows
/// as they were — when the forge cannot be reached at all, which is reported
/// once rather than per branch.
fn attach_mrs(repo: &Repository, declared: &Declared, rows: &mut [Row]) -> bool {
    if rows.is_empty() {
        return true;
    }
    let session = match ForgeSession::open(repo, &declared.roles) {
        Ok(session) => session,
        Err(e) => {
            log::warn!("MR state unavailable ({e:#}); showing the cached MRs");
            return false;
        }
    };
    let names: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
    let found = map_parallel(&names, |name| {
        session.forge.mrs_for_branch(HeadRepo::Origin, name)
    });
    for (row, result) in rows.iter_mut().zip(found) {
        match result {
            Ok(mrs) => {
                let sorted = BranchMrs::sort(mrs, Some(&row.parent));
                row.mr = sorted
                    .open
                    .or(sorted.newest_closed)
                    .map(|mr| describe(mr, &row.parent));
            }
            Err(e) => log::warn!("{}: {e:#}", row.name),
        }
    }
    true
}

fn describe(mr: MergeRequest, wants_base: &str) -> Mr {
    Mr {
        display: mr.display,
        state: match mr.state {
            MrState::Open => "open",
            MrState::Merged => "merged",
            MrState::Closed => "closed",
        },
        url: mr.web_url,
        base: mr.base,
        wants_base: wants_base.to_owned(),
    }
}

/// The verbs a row waits on. A merged or closed MR makes the branch done, and
/// nothing else applies.
fn needs(row: &Row, online: bool) -> Vec<&'static str> {
    if let Some(mr) = &row.mr {
        if mr.state != "open" {
            return vec!["delete"];
        }
    }
    let mut needs = Vec::new();
    if row.restack.is_some() {
        needs.push("restack");
    }
    if matches!(
        row.push.state,
        PushState::Ahead | PushState::Diverged | PushState::Unpushed
    ) {
        needs.push("push");
    }
    let submit = match &row.mr {
        Some(mr) => mr.base != mr.wants_base,
        None => online,
    };
    if submit {
        needs.push("submit");
    }
    needs
}

/// How the table is drawn: escape sequences only for a terminal.
#[derive(Debug, Clone, Copy)]
struct Style {
    color: bool,
    links: bool,
}

impl Style {
    fn detect() -> Self {
        let tty = std::io::stdout().is_terminal();
        Self {
            color: tty && std::env::var_os("NO_COLOR").is_none(),
            links: tty,
        }
    }

    fn paint(self, text: &str, code: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    /// An OSC 8 hyperlink, which kitty and most terminals render as a link and
    /// the rest ignore.
    fn link(self, text: &str, url: &str) -> String {
        if self.links && !url.is_empty() {
            format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
        } else {
            text.to_owned()
        }
    }
}

/// The header the checked-out branch is listed under when the stack does not
/// record it.
const OUTSIDE: &str = "not in a stack";

/// The forest as a tree, one row per branch: its push state, its MR, and what
/// it waits on. Each tree opens with its root — the base, or another branch a
/// stack sits on.
fn render(report: &Report, topology: &Topology, style: Style) -> String {
    let mut out = String::new();
    if report.branches.is_empty() {
        let _ = writeln!(out, "no stack on {}", report.base);
        return out;
    }

    let root_of = |row: &Row| -> String {
        if row.in_stack {
            topology
                .ancestors(&row.name)
                .into_iter()
                .next()
                .unwrap_or_else(|| report.base.clone())
        } else {
            OUTSIDE.to_owned()
        }
    };
    // The tree prefix of a row, drawn from where each link of its chain sits
    // among its parent's children; the root is the header line above.
    let prefix = |row: &Row| -> String {
        if !row.in_stack {
            return "└── ".to_owned();
        }
        let mut chain = topology.ancestors(&row.name);
        chain.push(row.name.clone());
        let mut text = String::new();
        for pair in chain.windows(2) {
            let (parent, child) = (&pair[0], &pair[1]);
            let last = topology.children(parent).last() == Some(child);
            text.push_str(match (child == &row.name, last) {
                (true, true) => "└── ",
                (true, false) => "├── ",
                (false, true) => "    ",
                (false, false) => "│   ",
            });
        }
        text
    };
    let label = |row: &Row| {
        let mut name = format!("{}{}", prefix(row), row.name);
        if row.current {
            name.push_str(" *");
        }
        name
    };
    let push_text = |row: &Row| match row.push.state {
        PushState::Pushed => "pushed".to_owned(),
        PushState::Ahead => format!("ahead {}", row.push.ahead),
        PushState::Behind => format!("behind {}", row.push.behind),
        PushState::Diverged => format!("diverged +{}/-{}", row.push.ahead, row.push.behind),
        PushState::Unpushed => "not pushed".to_owned(),
        PushState::Unknown => "-".to_owned(),
    };
    let mr_text = |row: &Row| match (&row.mr, &row.cached_mr) {
        (Some(mr), _) => format!("{} {}", mr.display, mr.state),
        (None, Some(cached)) if !report.online => format!("{cached} (cached)"),
        (None, _) if report.online => "no MR".to_owned(),
        (None, _) => "-".to_owned(),
    };
    let width = |f: &dyn Fn(&Row) -> String| {
        report
            .branches
            .iter()
            .map(|row| f(row).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (name_w, push_w, mr_w) = (width(&label), width(&push_text), width(&mr_text));

    let mut root: Option<String> = None;
    for row in &report.branches {
        let this_root = root_of(row);
        if root.as_deref() != Some(this_root.as_str()) {
            let header = match &report.trunk {
                Some(trunk) if this_root == report.base && trunk != &report.base => {
                    format!("{this_root}  (at {trunk})")
                }
                _ => this_root.clone(),
            };
            let _ = writeln!(out, "{}", style.paint(&header, "1"));
            root = Some(this_root);
        }

        let name = label(row);
        let push = push_text(row);
        let push_painted = match row.push.state {
            PushState::Pushed => style.paint(&push, "32"),
            PushState::Diverged => style.paint(&push, "31"),
            PushState::Unknown => push.clone(),
            _ => style.paint(&push, "33"),
        };
        let mr = mr_text(row);
        let mr_painted = match &row.mr {
            Some(found) => {
                let state = match found.state {
                    "open" => style.paint(found.state, "32"),
                    "merged" => style.paint(found.state, "35"),
                    _ => style.paint(found.state, "31"),
                };
                format!("{} {state}", style.link(&found.display, &found.url))
            }
            None => mr.clone(),
        };

        let mut notes = Vec::new();
        if let Some(found) = row.mr.as_ref().filter(|mr| mr.state == "open") {
            if found.base != found.wants_base {
                notes.push(format!(
                    "based on {}, the stack says {}",
                    found.base, found.wants_base
                ));
            }
        }
        if let Some(lacking) = row.restack {
            notes.push(format!("{} has {lacking} commit(s) it lacks", row.parent));
        }
        if let Some(recorded) = &row.placed_by_history {
            notes.push(format!(
                "placed by history; recorded on {recorded}, which is gone"
            ));
        }
        if !row.needs.is_empty() {
            notes.push(format!("needs {}", row.needs.join(", ")));
        }

        let mut line = format!(
            "{name}{}  {push_painted}{}  {mr_painted}",
            pad(&name, name_w),
            pad(&push, push_w)
        );
        if !notes.is_empty() {
            let _ = write!(line, "{}  {}", pad(&mr, mr_w), notes.join("; "));
        }
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Spaces that pad `text` to `width` columns.
fn pad(text: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(text.chars().count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(push: PushState, restack: Option<u32>, mr: Option<(&'static str, &str)>) -> Row {
        Row {
            name: "b".into(),
            parent: "a".into(),
            depth: 2,
            current: false,
            in_stack: true,
            push: Push {
                state: push,
                ahead: 0,
                behind: 0,
            },
            restack,
            mr: mr.map(|(state, base)| Mr {
                display: "#1".into(),
                state,
                url: String::new(),
                base: base.into(),
                wants_base: "a".into(),
            }),
            cached_mr: None,
            placed_by_history: None,
            needs: Vec::new(),
        }
    }

    #[test]
    fn a_merged_or_closed_mr_leaves_only_the_deletion() {
        for state in ["merged", "closed"] {
            let done = row(PushState::Unpushed, Some(3), Some((state, "main")));
            assert_eq!(needs(&done, true), ["delete"]);
        }
    }

    #[test]
    fn the_verbs_come_in_the_order_they_apply() {
        let behind = row(PushState::Diverged, Some(2), Some(("open", "main")));
        assert_eq!(needs(&behind, true), ["restack", "push", "submit"]);
        let fine = row(PushState::Pushed, None, Some(("open", "a")));
        assert!(needs(&fine, true).is_empty());
    }

    #[test]
    fn a_missing_mr_needs_submit_only_when_the_forge_was_asked() {
        let unknown = row(PushState::Pushed, None, None);
        assert_eq!(needs(&unknown, true), ["submit"]);
        assert!(needs(&unknown, false).is_empty());
    }
}
