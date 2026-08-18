//! `wits review submit` — flush the local draft to the forge.
//!
//! The one network write. It reads `local.json`, merges and de-duplicates the
//! recorded actions, and hands the whole review to the forge as one
//! [`ReviewBatch`]. The forge folds as many actions as its native primitive
//! allows into one notification and reports a granular [`BatchOutcome`] keyed by
//! action, so reconciliation is **per action**: whatever landed is cleared,
//! whatever failed stays in the draft to retry. Only a fully-flushed draft
//! triggers a re-fetch, so a partial failure never loses unposted work.

use anyhow::{Context, Result};

use wits_util::forge::{BatchAction, DiffVersion, Forge, ReviewBatch};
use wits_util::git::Repository;
use wits_util::log as wits_log;

use super::model::{comment_anchor, Action, Local, StoredFile};
use super::{online, Online, SubmitArgs};

pub fn run(repo: &Repository, args: &SubmitArgs) -> Result<()> {
    let ctx = online(repo)?;

    let ids = target_ids(&ctx, args)?;
    if ids.is_empty() {
        log::info!("no drafts to submit");
        return Ok(());
    }

    // Submissions to different MRs are wholly independent — fan out over scoped
    // threads so network latency overlaps. Per-action reconciliation inside each
    // MR stays sequential (the store writes to distinct paths, so no races).
    let results = super::map_parallel(&ids, |id| {
        let result = submit_one(&ctx, id);
        (id.clone(), result)
    });

    let failures: Vec<_> = results
        .into_iter()
        .filter_map(|(id, r)| r.err().map(|e| (id, e)))
        .collect();
    if !failures.is_empty() {
        for (id, e) in &failures {
            log::warn!("MR {id}: {e}");
        }
        anyhow::bail!("{} MR(s) failed to submit", failures.len());
    }
    Ok(())
}

fn target_ids(ctx: &Online, args: &SubmitArgs) -> Result<Vec<String>> {
    if args.all {
        return Ok(ctx.local.store.local_ids());
    }
    let handle = args.mr.as_ref().context("give an MR to submit, or --all")?;
    let id = super::parse_mr_handle(handle)?;

    if args.stack {
        let infos = ctx.local.store.list_infos();
        let (chain, _) = super::stack_chain(&infos, &id);
        let drafted: std::collections::HashSet<String> =
            ctx.local.store.local_ids().into_iter().collect();
        Ok(chain.into_iter().filter(|n| drafted.contains(n)).collect())
    } else {
        Ok(vec![id])
    }
}

fn submit_one(ctx: &Online, id: &str) -> Result<()> {
    let store = &ctx.local.store;
    let forge = ctx.forge.as_ref();

    let local = store.load_local(id)?;
    let stale = store.load_inflight(id);
    if local.is_empty() && stale.is_empty() {
        log::info!("MR {id}: nothing to submit");
        return Ok(());
    }

    // Cleanup-only: the draft is gone but a prior submit deferred an in-flight
    // cleanup. Hand the forge an empty batch carrying just the stale ids, so its
    // pre-flight discards them; nothing else to post.
    if local.is_empty() {
        if wits_log::is_dry_run() {
            wits_log::dry_run(&format!(
                "submit {} {id}: clear {} deferred forge object(s)",
                forge.noun(),
                stale.len()
            ));
            return Ok(());
        }
        let batch = ReviewBatch {
            verdict: None,
            summary: None,
            actions: Vec::new(),
            version: DiffVersion::default(),
            stale,
        };
        let outcome = forge.submit(id, &batch)?;
        store.save_inflight(id, &outcome.inflight)?;
        if outcome.inflight.is_empty() {
            log::info!("MR {id}: cleaned up deferred forge state");
            Ok(())
        } else {
            anyhow::bail!("deferred cleanup did not complete; it will retry next submit")
        }
    } else {
        submit_draft(ctx, id, local, stale)
    }
}

/// Flush a non-empty draft. `stale` is any deferred in-flight cleanup to run
/// first (see [`Store::load_inflight`]).
fn submit_draft(ctx: &Online, id: &str, mut local: Local, stale: Vec<String>) -> Result<()> {
    let store = &ctx.local.store;
    let forge = ctx.forge.as_ref();

    // A draft can contain only local tombstones. Compact those before requiring a
    // fetched snapshot, since there may be nothing left to anchor or submit.
    local.normalize("");
    if local.is_empty() && stale.is_empty() {
        save_empty_after_dedup(store, id, &local)?;
        log::info!("MR {id}: nothing to submit after dedup");
        return Ok(());
    }

    let info = store
        .load_info(id)
        .with_context(|| format!("MR {id} isn't fetched — run `wits review fetch {id}` first"))?;
    let version = info
        .current()
        .cloned()
        .filter(|v| !v.head_sha.is_empty())
        .with_context(|| {
            format!(
                "MR {id} has no reviewed snapshot; run `wits review fetch {id}` for full detail"
            )
        })?;

    // Stamp unstamped comments now that the current snapshot head is known.
    local.normalize(&version.head_sha);
    if local.is_empty() && stale.is_empty() {
        save_empty_after_dedup(store, id, &local)?;
        log::info!("MR {id}: nothing to submit after dedup");
        return Ok(());
    }

    if wits_log::is_dry_run() {
        preview(id, &local, forge.noun());
        return Ok(());
    }

    // Hand the whole review to the forge as one batch; it folds what it can into
    // one notification and reports each action's landing by key. A hard `Err`
    // means *nothing* landed — the whole (normalized) draft stays for retry.
    let batch = build_batch(&local, &version, &info.snapshots, &info.files, forge, stale);
    let outcome = match forge.submit(id, &batch) {
        Ok(o) => o,
        Err(e) => {
            log::warn!("MR {id}: submit failed (nothing landed): {e}");
            store.save_local(id, &local)?;
            anyhow::bail!("submit failed; the draft is unchanged");
        }
    };

    let reconciled = reconcile_local(&mut local, &outcome);
    store.save_local(id, &local)?;
    // Persist any forge-side objects this attempt left unpublished, so the next
    // submit's pre-flight cleans them (empty ⇒ the file is removed).
    store.save_inflight(id, &outcome.inflight)?;

    if reconciled.kept > 0 {
        log::warn!(
            "MR {id}: {} action(s) did not submit; they remain in the draft",
            reconciled.kept
        );
    }

    if local.is_empty() {
        if let Err(e) = super::fetch::refresh(ctx, id) {
            log::warn!("MR {id}: submitted, but refreshing the cache failed: {e}");
        }
        log::info!(
            "MR {id}: submitted ({} action(s), {} notification(s))",
            reconciled.posted,
            outcome.notifications
        );
        Ok(())
    } else {
        anyhow::bail!("some actions did not submit; they remain in the draft")
    }
}

fn save_empty_after_dedup(store: &super::store::Store, id: &str, local: &Local) -> Result<()> {
    if wits_log::is_dry_run() {
        wits_log::dry_run(&format!("submit MR {id}: compact local draft to empty"));
        Ok(())
    } else {
        store.save_local(id, local)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconcileStats {
    posted: usize,
    kept: usize,
}

/// Apply a forge outcome to a normalized local draft. Batch actions are keyed by
/// their persistent action ids; summary is tracked separately by the forge outcome
/// because it is submitted as the review body.
fn reconcile_local(local: &mut Local, outcome: &wits_util::forge::BatchOutcome) -> ReconcileStats {
    let posted = local
        .actions
        .iter()
        .filter(|a| match a {
            Action::Summary { .. } => outcome.summary_ok,
            _ => outcome.landed(action_key(a)),
        })
        .count();
    local.actions = local
        .actions
        .drain(..)
        .filter_map(|a| match &a {
            Action::Summary { .. } if outcome.summary_ok => None,
            _ if outcome.landed(action_key(&a)) => None,
            _ => Some(a),
        })
        .collect();
    if outcome.verdict_ok == Some(true) {
        local.verdict = None;
    }
    ReconcileStats {
        posted,
        kept: local.actions.len(),
    }
}

fn action_key(action: &Action) -> &str {
    action.id().expect("normalized submit actions have ids")
}

/// Build the forge-neutral [`ReviewBatch`] from the normalized draft. Every live
/// action becomes a [`BatchAction`] carrying its persistent action id, so the
/// forge can report each one's landing independently. A comment's
/// body has its `[[…]]` references expanded to forge permalinks, and it is
/// anchored to the [`DiffVersion`] of the snapshot it was written against —
/// resolved from `snapshots` by the comment's own `commit` (the snapshot head
/// stamped at ingest), falling back to the current version.
fn build_batch(
    local: &Local,
    version: &DiffVersion,
    snapshots: &[DiffVersion],
    files: &[StoredFile],
    forge: &dyn Forge,
    stale: Vec<String>,
) -> ReviewBatch {
    let actions = local
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::Comment {
                id,
                file,
                line,
                side,
                start_line,
                start_side,
                body,
                commit,
            } => {
                let cv = comment_version(commit.as_deref(), snapshots, version);
                let old_path = file.as_deref().and_then(|p| old_path_for(p, files));
                let anchor = comment_anchor(
                    file.as_deref(),
                    *line,
                    *side,
                    *start_line,
                    *start_side,
                    old_path,
                );
                Some(BatchAction::Comment {
                    key: id.clone().expect("normalized comment action has an id"),
                    anchor,
                    body: expand_refs(body, forge, &cv.head_sha),
                    version: cv,
                })
            }
            Action::Summary { .. } | Action::Drop { .. } => None,
            Action::Reply {
                id, thread, body, ..
            } => Some(BatchAction::Reply {
                key: id.clone().expect("normalized reply action has an id"),
                thread: thread.as_str().to_owned(),
                body: expand_refs(body, forge, &version.head_sha),
            }),
            Action::Resolve {
                id,
                thread,
                resolved,
                ..
            } => Some(BatchAction::Resolve {
                key: id.clone().expect("normalized resolve action has an id"),
                thread: thread.as_str().to_owned(),
                resolved: *resolved,
            }),
        })
        .collect();
    ReviewBatch {
        verdict: local.verdict,
        summary: local
            .summary()
            .map(|s| expand_refs(s, forge, &version.head_sha)),
        actions,
        version: version.clone(),
        stale,
    }
}

/// Resolve the per-snapshot [`DiffVersion`] a comment was written against. The
/// comment's `commit` is a snapshot head SHA (stamped at ingest); look it up in
/// the history so GitLab's `position` gets the right `base`/`start`/`head`
/// triple. An unset or un-found `commit` falls back to the current version.
fn comment_version(
    action_commit: Option<&str>,
    snapshots: &[DiffVersion],
    version: &DiffVersion,
) -> DiffVersion {
    if let Some(sha) = action_commit {
        if let Some(s) = snapshots.iter().rev().find(|s| s.head_sha == sha) {
            return s.clone();
        }
    }
    version.clone()
}

/// Look up a changed file's pre-image path (`old_path`) by its new path. Renames
/// and copies carry `old_path`; for any other status it is `None`. Used to carry
/// the pre-image path through to the forge anchor (GitLab needs it for a comment
/// on a renamed/deleted file's old side).
fn old_path_for(path: &str, files: &[super::model::StoredFile]) -> Option<String> {
    files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.old_path.clone())
}

/// Expand every `[[path:line]]` reference in `body` to a forge permalink. The
/// grammar: `path` (repo-relative), optional `:line` or `:start-end`, optional
/// `@ref` to pin a commit/branch/tag (default: the reviewed head). Unterminated
/// or unparseable tokens are left as written.
fn expand_refs(body: &str, forge: &dyn Forge, default_ref: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                out.push_str(&expand_one(after[..end].trim(), forge, default_ref));
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("[[");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn expand_one(token: &str, forge: &dyn Forge, default_ref: &str) -> String {
    let (locpart, r#ref) = match token.rsplit_once('@') {
        Some((l, r)) if !r.is_empty() => (l, r),
        _ => (token, default_ref),
    };
    let (path, lines) = match locpart.rsplit_once(':') {
        Some((p, spec)) => match parse_lines(spec) {
            Some(lines) => (p, Some(lines)),
            None => (locpart, None),
        },
        None => (locpart, None),
    };
    forge.permalink(r#ref, path, lines)
}

/// Parse a `N` or `N-M` line spec.
fn parse_lines(spec: &str) -> Option<(u32, Option<u32>)> {
    match spec.split_once('-') {
        Some((a, b)) => Some((a.parse().ok()?, Some(b.parse().ok()?))),
        None => Some((spec.parse().ok()?, None)),
    }
}

/// Print what a submit would do, without touching the forge.
fn preview(id: &str, local: &Local, noun: &str) {
    if let Some(v) = local.verdict {
        wits_log::dry_run(&format!("submit {noun} {id}: verdict {}", v.display_str()));
    }
    for action in &local.actions {
        let line = match action {
            Action::Comment {
                file: Some(f),
                line: Some(l),
                ..
            } => format!("comment on {f}:{l}"),
            Action::Comment { file: Some(f), .. } => format!("comment on file {f}"),
            Action::Comment { .. } => "conversation comment".to_owned(),
            Action::Summary { .. } => "summary".to_owned(),
            Action::Reply { thread, .. } => format!("reply to {thread}"),
            Action::Resolve {
                thread, resolved, ..
            } => {
                let verb = if *resolved { "resolve" } else { "unresolve" };
                format!("{verb} {thread}")
            }
            Action::Drop { id } => format!("drop {id}"),
        };
        wits_log::dry_run(&format!("submit {noun} {id}: {line}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wits_util::forge::github::GitHub;
    use wits_util::forge::{RemoteInfo, Service};

    fn github() -> GitHub {
        GitHub::new(
            RemoteInfo {
                host: "github.com".into(),
                owner: "o".into(),
                repo: "r".into(),
                service: Service::GitHub,
            },
            None,
            "t".into(),
            None,
        )
    }

    fn comment(id: &str) -> Action {
        Action::Comment {
            id: Some(id.into()),
            file: Some("a.c".into()),
            line: Some(1),
            side: None,
            start_line: None,
            start_side: None,
            body: id.into(),
            commit: Some("head".into()),
        }
    }

    fn summary(id: &str) -> Action {
        Action::Summary {
            id: Some(id.into()),
            body: id.into(),
        }
    }

    fn outcome(
        landed: &[(&str, bool)],
        summary_ok: bool,
        verdict_ok: Option<bool>,
    ) -> wits_util::forge::BatchOutcome {
        wits_util::forge::BatchOutcome {
            landed: landed
                .iter()
                .map(|(key, ok)| ((*key).to_owned(), *ok))
                .collect(),
            summary_ok,
            verdict_ok,
            notifications: 1,
            inflight: Vec::new(),
        }
    }

    #[test]
    fn reconcile_clears_fully_landed_draft() {
        let mut local = Local {
            schema: super::super::model::SCHEMA,
            verdict: Some(wits_util::forge::Verdict::Approve),
            actions: vec![summary("s"), comment("c")],
        };

        let stats = reconcile_local(&mut local, &outcome(&[("c", true)], true, Some(true)));

        assert_eq!(stats, ReconcileStats { posted: 2, kept: 0 });
        assert!(local.is_empty());
    }

    #[test]
    fn reconcile_keeps_failed_actions_summary_and_verdict() {
        let mut local = Local {
            schema: super::super::model::SCHEMA,
            verdict: Some(wits_util::forge::Verdict::RequestChanges),
            actions: vec![summary("s"), comment("ok"), comment("fail")],
        };

        let stats = reconcile_local(
            &mut local,
            &outcome(&[("ok", true), ("fail", false)], false, Some(false)),
        );

        assert_eq!(stats, ReconcileStats { posted: 1, kept: 2 });
        assert_eq!(
            local.verdict,
            Some(wits_util::forge::Verdict::RequestChanges)
        );
        assert!(matches!(local.actions[0], Action::Summary { .. }));
        assert!(matches!(
            &local.actions[1],
            Action::Comment { id: Some(id), .. } if id == "fail"
        ));
    }

    #[test]
    fn expands_line_references_to_permalinks() {
        let gh = github();
        let go = |b: &str| expand_refs(b, &gh, "deadbeef");

        assert_eq!(
            go("see [[src/y.c:20]] here"),
            "see https://github.com/o/r/blob/deadbeef/src/y.c#L20 here"
        );
        assert_eq!(
            go("[[src/y.c:20-25]]"),
            "https://github.com/o/r/blob/deadbeef/src/y.c#L20-L25"
        );
        // A whole-file reference has no line fragment.
        assert_eq!(
            go("[[README.md]]"),
            "https://github.com/o/r/blob/deadbeef/README.md"
        );
        // `@ref` pins another commit/branch.
        assert_eq!(
            go("[[src/y.c:9@main]]"),
            "https://github.com/o/r/blob/main/src/y.c#L9"
        );
        // A non-reference is untouched; an unterminated token is left as written.
        assert_eq!(go("no refs here"), "no refs here");
        assert_eq!(go("dangling [[oops"), "dangling [[oops");
    }
}
