//! `wits review diff` — the diff's *coordinates*, not its rendering.
//!
//! The tool does not render diffs (the editor and `git` do that well); it owns
//! the coordinate layer — the snapshot SHAs, the commit list, and the changed
//! files a comment may anchor in. `--patch` is a terminal/debug convenience that
//! shells to `git`; `--json` is the coordinate payload an editor consumes.

use anyhow::{Context, Result};
use serde::Serialize;

use wits_util::git::Repository;

use super::model::{range_artifacts, short, StoredCommit, StoredFile, SCHEMA};
use super::{local, DiffArgs};

#[derive(Serialize)]
struct DiffView {
    schema: u32,
    mr: String,
    range: String,
    base_sha: String,
    head_sha: String,
    commits: Vec<StoredCommit>,
    files: Vec<StoredFile>,
}

pub fn run(repo: &Repository, args: &DiffArgs) -> Result<()> {
    let ctx = local(repo)?;
    let id = super::parse_mr_handle(&args.mr)?;
    let info = ctx.store.load_info(&id).with_context(|| {
        format!("MR {id} isn't in the store yet — run `wits review fetch {id}` first")
    })?;

    // Resolve the range from a chosen snapshot, the current snapshot (`all`),
    // or a verbatim git range.
    let (base_sha, head_sha) = if let Some(sha) = &args.snapshot {
        let snap = info
            .snapshots
            .iter()
            .find(|s| s.head_sha.starts_with(sha))
            .with_context(|| format!("MR {id} has no fetched snapshot matching '{sha}'"))?;
        (snap.base_sha.clone(), snap.head_sha.clone())
    } else {
        let current = info.current();
        (
            current.map(|s| s.base_sha.clone()).unwrap_or_default(),
            current.map(|s| s.head_sha.clone()).unwrap_or_default(),
        )
    };
    let range = if args.range == "all" {
        format!("{base_sha}..{head_sha}")
    } else {
        args.range.clone()
    };

    if args.patch {
        match ctx.repo.diff_patch(&range, None) {
            Some(patch) => println!("{patch}"),
            None => {
                anyhow::bail!("could not compute a diff for '{range}' (are the objects fetched?)")
            }
        }
        return Ok(());
    }

    let (commits, files) = range_artifacts(&ctx.repo, &range);

    let view = DiffView {
        schema: SCHEMA,
        mr: id,
        range,
        base_sha,
        head_sha,
        commits,
        files,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("{} {}", view.mr, view.range);
        for c in &view.commits {
            println!("  {} {}", short(&c.sha), c.subject);
        }
        for f in &view.files {
            println!("  {} {}", f.status, f.path);
        }
    }
    Ok(())
}
