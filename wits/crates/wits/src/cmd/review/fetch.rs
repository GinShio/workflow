//! `wits review fetch` — bring the local store in line with the forge.
//!
//! Idempotent, like `git fetch`. Fetching one MR is a *full* pull: metadata and
//! diff state into `info.json`, the discussion into `comments.json`, and the
//! objects pinned by a `refs/wits/review/*` ref so a later force-push can't GC
//! them. Fetching a feed is *light* — it refreshes only `info.json` for each
//! matching MR, leaving the expensive per-MR pull to `fetch <mr>`. With no
//! argument, every configured feed is refreshed (the RSS "refresh all").

use std::collections::{BTreeSet, HashSet};

use anyhow::{Context, Result};

use wits_util::forge::{DiffVersion, MergeRequest, MrState, MrSummary};
use wits_util::git::Repository;
use wits_util::time::now_secs;

use super::config::{self, Config};
use super::model::{range_artifacts, Comments, Info, StoredCommit, StoredFile, Thread, SCHEMA};
use super::store::{refs, Store};
use super::{online, parse_mr_handle, Online, StackMode};

pub fn run(repo: &Repository, args: &super::FetchArgs) -> Result<()> {
    let ctx = online(repo)?;

    if let Some(handle) = &args.mr {
        let id = parse_mr_handle(handle)?;
        let remote = target_remote(&ctx);
        // A single MR has no feed default, so the flag alone decides.
        let mode = args.stack.unwrap_or(StackMode::Auto);
        return fetch_mr(&ctx, &remote, &id, mode);
    }

    let cfg = Config::load()?;
    let key = config::repo_key(&ctx.local.target);
    match &args.feed {
        // A single feed refreshes just its own MRs; the known-MR sweep is only
        // for the bare "refresh everything" below, so a throwaway set suffices.
        Some(name) => fetch_feed(&ctx, &cfg, &key, name, args.stack, &mut HashSet::new()),
        None => fetch_all_feeds(&ctx, &cfg, &key, args.stack),
    }
}

/// The git remote name to fetch MR refs from — the merge target.
fn target_remote(ctx: &Online) -> String {
    if ctx.remotes.upstream.is_some() {
        "upstream".to_owned()
    } else {
        "origin".to_owned()
    }
}

/// Re-fetch one MR after a submit, so freshly-posted threads come back.
pub(crate) fn refresh(ctx: &Online, id: &str) -> Result<()> {
    fetch_one(ctx, &target_remote(ctx), id)
}

/// Fully fetch one MR: metadata + diff state, discussion, and pinned objects.
fn fetch_one(ctx: &Online, remote: &str, id: &str) -> Result<()> {
    let forge = ctx.forge.as_ref();
    let store = &ctx.local.store;
    let repo = &ctx.local.repo;

    let details = forge.mr_details(id)?;
    let v = details.version;
    let head_sha = v.head_sha.clone();

    if !head_sha.is_empty() {
        let mr_ref = forge.mr_ref(id)?;
        repo.fetch_ref(remote, &mr_ref, &refs::pin(id, &head_sha))
            .with_context(|| format!("fetching MR {id} objects"))?;
    }
    if !v.base_sha.is_empty() && repo.rev_parse(&v.base_sha).is_none() {
        repo.try_fetch_object(remote, &v.base_sha, &refs::base_pin(id, &head_sha));
    }

    let (commits, files) = local_range(repo, &v.base_sha, &head_sha);
    // Preserve the snapshot history across fetches; append only when the head
    // moved. Metadata and current-snapshot diff state are refreshed wholesale,
    // and `fetched_at` is stamped every time (even for an unchanged head) so
    // dormancy tracks real sync time.
    let mut info = Info {
        schema: SCHEMA,
        mr: details.summary,
        snapshots: store.load_info(id).map(|i| i.snapshots).unwrap_or_default(),
        fetched_at: now_secs(),
        commits,
        files,
    };
    info.record_snapshot(DiffVersion {
        base_sha: v.base_sha,
        start_sha: v.start_sha,
        head_sha,
    });
    store.save_info(id, &info)?;

    let threads: Vec<Thread> = forge
        .list_threads(id)?
        .into_iter()
        .map(Thread::from)
        .collect();
    store.save_comments(
        id,
        &Comments {
            schema: SCHEMA,
            threads,
        },
    )?;
    Ok(())
}

/// Fetch one MR (a full pull) and, per `mode`, the rest of its stack.
///
/// `none` fetches only this MR; `auto` completes the stack when this MR sits on
/// another (its base is a feature branch); `all` completes even from a bottom
/// MR. Members are discovered on the forge by walking the base/source links
/// (see [`discover_stack`]) and each gets its own full [`fetch_one`]. The walk
/// is bounded to the real stack — it climbs no higher than a trunk (and never
/// probes one) — and progress is logged, so a multi-MR stack isn't a silent wait.
fn fetch_mr(ctx: &Online, remote: &str, seed_id: &str, mode: StackMode) -> Result<()> {
    let noun = ctx.forge.noun();
    fetch_one(ctx, remote, seed_id)?;
    log::info!("fetched {noun} {seed_id}");

    if mode == StackMode::None {
        return Ok(());
    }

    // Reuse the fetch we just did: the seed's base/source are now in the store.
    let Some(info) = ctx.local.store.load_info(seed_id) else {
        return Ok(());
    };
    let forge = ctx.forge.as_ref();
    let trunk = ctx.local.repo.remote_default_branch(remote);
    let seeds = stack_seeds(std::slice::from_ref(&info.mr), trunk.as_deref(), mode);
    if seeds.is_empty() {
        // `auto` on a lone/bottom MR: nothing sits below it to complete.
        return Ok(());
    }
    let ids = discover_stack(
        seeds,
        |branch| climb(forge, branch, trunk.as_deref()),
        |branch| forge.find_children(branch),
    )?;

    for id in ids.iter().filter(|id| id.as_str() != seed_id) {
        fetch_one(ctx, remote, id)?;
        log::info!("  + stack member {noun} {id}");
    }
    Ok(())
}

/// The upward step of a stack walk: the parent MR whose source branch is
/// `branch`, or `None` when `branch` is a trunk — a trunk has no parent MR, so
/// this skips the forge call entirely rather than paying for a query that can
/// only come back empty.
fn climb(
    forge: &dyn wits_util::forge::Forge,
    branch: &str,
    trunk: Option<&str>,
) -> Result<Option<MergeRequest>> {
    if is_trunk(branch, trunk) {
        Ok(None)
    } else {
        forge.find_any(branch)
    }
}

/// The ids of every MR in the stack(s) containing the given seeds, each a
/// `(id, base, source)` triple. Discovered by a breadth-first walk of the
/// base/source links: `parent_of(base)` climbs toward the trunk, and
/// `children_of(source)` descends toward the leaves. The walk is bounded to the
/// real stack — a trunk is nobody's source (so the upward climb stops) and we
/// only ever ask for the children *of a source branch*, never of a trunk.
///
/// Seeding from many MRs at once (a whole feed) shares one visited set, so a
/// stack several of them belong to is walked exactly once. Each branch is
/// probed at most once in each direction, and `BTreeSet` gives a stable,
/// id-sorted result. Pure over the two link functions, so the graph logic is
/// testable without a forge.
fn discover_stack(
    seeds: impl IntoIterator<Item = (String, String, String)>,
    parent_of: impl Fn(&str) -> Result<Option<MergeRequest>>,
    children_of: impl Fn(&str) -> Result<Vec<MergeRequest>>,
) -> Result<Vec<String>> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    // Frontier of `(base, source)` links still to expand.
    let mut frontier = Vec::new();
    for (id, base, source) in seeds {
        if ids.insert(id) {
            frontier.push((base, source));
        }
    }

    // A branch need only be probed once per direction, even when several seeds
    // share the same stack.
    let mut climbed: HashSet<String> = HashSet::new();
    let mut descended: HashSet<String> = HashSet::new();

    while let Some((base, source)) = frontier.pop() {
        if !base.is_empty() && climbed.insert(base.clone()) {
            if let Some(parent) = parent_of(&base)? {
                if ids.insert(parent.id) {
                    frontier.push((parent.base, parent.source));
                }
            }
        }
        if !source.is_empty() && descended.insert(source.clone()) {
            for child in children_of(&source)? {
                if ids.insert(child.id) {
                    frontier.push((child.base, child.source));
                }
            }
        }
    }
    Ok(ids.into_iter().collect())
}

/// Whether `base` is a trunk branch — the thing a stack roots on, which no MR is
/// stacked *below*. Resolved from the locally-tracked remote HEAD (no network);
/// on a fresh clone that lacks the symref we fall back to the conventional
/// names so detection still works. Used to (a) tell a stacked MR (base is a
/// feature branch) from a lone one and (b) skip the upward probe for a trunk,
/// which can have no parent MR.
fn is_trunk(base: &str, trunk: Option<&str>) -> bool {
    match trunk {
        Some(t) => base == t,
        None => matches!(base, "main" | "master" | "trunk" | "develop"),
    }
}

/// Refresh one feed's inbox summaries — cheap, `info.json` only. Where a full
/// pull already exists, only the summary is refreshed; its diff state and
/// discussion are left intact.
fn fetch_feed(
    ctx: &Online,
    cfg: &Config,
    key: &str,
    name: &str,
    cli_mode: Option<StackMode>,
    touched: &mut HashSet<String>,
) -> Result<()> {
    let query = cfg.feed(key, name).with_context(|| {
        let known = cfg.feed_names(key);
        if known.is_empty() {
            format!("no feed '{name}' — no feeds configured for {key}")
        } else {
            format!("no feed '{name}'; configured feeds: {}", known.join(", "))
        }
    })?;
    // The flag wins over the feed's own default, which wins over `auto`.
    let mode = cli_mode
        .or_else(|| cfg.feed_stack(key, name))
        .unwrap_or(StackMode::Auto);

    log::info!("feed '{name}': fetching…");
    let summaries = ctx.forge.list_mrs(&query)?;
    let store = &ctx.local.store;
    for summary in &summaries {
        touched.insert(summary.id.clone());
        store_summary(store, summary.clone())?;
    }

    // Complete stacks — but only for MRs that *are* stacked, so an inbox of
    // unrelated MRs costs no extra forge calls (the source of the "long silent
    // wait"). An MR is worth completing when it targets a feature branch (it has
    // a parent) or another matched MR is stacked on it (it has a child in the
    // feed); a lone MR — base is a trunk, nobody stacked on it — is skipped.
    // Only these seeds are then walked outward; the missing members they pull in
    // (light, summary-only) are exactly the ones the label/limit filter dropped.
    let forge = ctx.forge.as_ref();
    let remote = target_remote(ctx);
    let trunk = ctx.local.repo.remote_default_branch(&remote);

    let seeds = stack_seeds(&summaries, trunk.as_deref(), mode);

    let mut added = 0;
    if !seeds.is_empty() {
        log::info!("feed '{name}': completing {} stacked MR(s)…", seeds.len());
        let members = discover_stack(
            seeds,
            |branch| climb(forge, branch, trunk.as_deref()),
            |branch| forge.find_children(branch),
        )?;
        let matched: HashSet<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        for id in members.iter().filter(|id| !matched.contains(id.as_str())) {
            // A member's summary alone (no diff state / discussion) is enough for
            // the inbox and stack navigation; `mr_details` is one metadata call.
            store_summary(store, forge.mr_details(id)?.summary)?;
            touched.insert(id.clone());
            added += 1;
            log::info!("  + stack member {} {id}", forge.noun());
        }
    }

    let extra = if added > 0 {
        format!(" (+{added} stack member(s))")
    } else {
        String::new()
    };
    log::info!("feed '{name}': {} MR(s){extra}", summaries.len());
    Ok(())
}

/// From the given MRs, the ones to seed stack completion from, per `mode`:
///
/// - `none` — nothing; completion is off.
/// - `auto` — only MRs that sit on another (base is a feature branch, not the
///   trunk). A lone/bottom MR is left out, so an inbox of unrelated MRs triggers
///   no walk and no extra forge calls. A non-bottom seed's walk still reaches
///   the bottom, so seeding only the non-bottoms loses no members.
/// - `all` — every MR, so even a bottom one is walked (its children probed).
///
/// Pure, so the selection is testable without a forge.
fn stack_seeds(
    summaries: &[MrSummary],
    trunk: Option<&str>,
    mode: StackMode,
) -> Vec<(String, String, String)> {
    summaries
        .iter()
        .filter(|s| match mode {
            StackMode::None => false,
            StackMode::All => true,
            StackMode::Auto => !is_trunk(&s.base, trunk),
        })
        .map(|s| (s.id.clone(), s.base.clone(), s.source.clone()))
        .collect()
}

/// Store an MR's summary as a light `info.json`: refresh `mr` and `fetched_at`,
/// preserving any snapshot history a prior full fetch recorded. Shared by a feed
/// refresh and stack completion, so the light-touch shape can't drift between
/// them.
fn store_summary(store: &Store, summary: MrSummary) -> Result<()> {
    let mut info = match store.load_info(&summary.id) {
        Some(mut existing) => {
            existing.mr = summary;
            existing
        }
        None => Info {
            schema: SCHEMA,
            mr: summary,
            snapshots: Vec::new(),
            fetched_at: 0,
            commits: Vec::new(),
            files: Vec::new(),
        },
    };
    info.fetched_at = now_secs();
    store.save_info(&info.mr.id, &info)
}

/// Refresh every configured feed for the repo, then re-check any still-open MR
/// already in the store that no feed touched this run (see [`refresh_untouched`]).
fn fetch_all_feeds(
    ctx: &Online,
    cfg: &Config,
    key: &str,
    cli_mode: Option<StackMode>,
) -> Result<()> {
    let names = cfg.feed_names(key);
    if names.is_empty() {
        anyhow::bail!(
            "no feeds configured for {key}; give an MR number, or add a feed to review.toml"
        );
    }
    // Phase 1: the feeds, by their limits. Every MR any feed writes is recorded.
    let mut touched = HashSet::new();
    let mut failures = 0;
    for name in &names {
        if let Err(e) = fetch_feed(ctx, cfg, key, name, cli_mode, &mut touched) {
            failures += 1;
            log::warn!("feed '{name}': {e}");
        }
    }
    // Phase 2: catch what the feeds no longer see.
    refresh_untouched(ctx, &touched)?;
    if failures > 0 {
        anyhow::bail!("{failures} feed(s) failed to refresh");
    }
    Ok(())
}

/// Refresh the still-open MRs already in the store that no feed reported this
/// run. A feed only lists live work, so an MR that merged/closed since the last
/// fetch simply drops out of every feed and would otherwise sit `open` in the
/// inbox forever. Re-checking each catches that transition — bounded to the
/// non-terminal known MRs (a merged/closed one is already final, so it is
/// skipped), and light (one `mr_details` metadata call each, no objects).
fn refresh_untouched(ctx: &Online, touched: &HashSet<String>) -> Result<()> {
    let stale: Vec<String> = ctx
        .local
        .store
        .list_infos()
        .into_iter()
        .filter(|i| i.mr.state == MrState::Open && !touched.contains(&i.mr.id))
        .map(|i| i.mr.id)
        .collect();
    if stale.is_empty() {
        return Ok(());
    }

    log::info!("refreshing {} known MR(s) no feed covered…", stale.len());
    let store = &ctx.local.store;
    for id in &stale {
        match ctx.forge.mr_details(id) {
            Ok(details) => store_summary(store, details.summary)?,
            // A gone/unreachable MR stays as-is; `prune` can clear it later.
            Err(e) => log::warn!("MR {id}: refresh failed ({e})"),
        }
    }
    Ok(())
}

/// Commits (oldest-first) and changed files in `base..head`, derived from the
/// fetched objects. Empty when the range can't be computed locally (a missing
/// endpoint), delegating the mapping to [`range_artifacts`].
fn local_range(repo: &Repository, base: &str, head: &str) -> (Vec<StoredCommit>, Vec<StoredFile>) {
    if base.is_empty() || head.is_empty() {
        return (Vec::new(), Vec::new());
    }
    range_artifacts(repo, &format!("{base}..{head}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wits_util::forge::MrState;

    /// A tiny in-memory stack: `(id, base, source)` per MR. The link functions
    /// stand in for the forge — `parent_of(branch)` = the MR whose source is
    /// `branch`, `children_of(branch)` = the MRs whose base is `branch`.
    #[allow(clippy::type_complexity)]
    fn links(
        stack: &'static [(&'static str, &'static str, &'static str)],
    ) -> (
        impl Fn(&str) -> Result<Option<MergeRequest>>,
        impl Fn(&str) -> Result<Vec<MergeRequest>>,
    ) {
        let mk = |&(id, base, source): &(&str, &str, &str)| MergeRequest {
            id: id.to_owned(),
            display: format!("#{id}"),
            state: MrState::Open,
            base: base.to_owned(),
            source: source.to_owned(),
            head_sha: None,
            body: String::new(),
            web_url: String::new(),
        };
        let parent = move |branch: &str| {
            Ok(stack
                .iter()
                .find(|(_, _, source)| *source == branch)
                .map(mk))
        };
        let children = move |branch: &str| {
            Ok(stack
                .iter()
                .filter(|(_, base, _)| *base == branch)
                .map(mk)
                .collect())
        };
        (parent, children)
    }

    /// Discover from a single `(id, base, source)` seed — the `fetch <mr>` shape.
    fn from_seed(
        seed: (&str, &str, &str),
        parent: impl Fn(&str) -> Result<Option<MergeRequest>>,
        children: impl Fn(&str) -> Result<Vec<MergeRequest>>,
    ) -> Vec<String> {
        let (id, base, source) = seed;
        discover_stack(
            [(id.to_owned(), base.to_owned(), source.to_owned())],
            parent,
            children,
        )
        .unwrap()
    }

    #[test]
    fn discovers_the_whole_stack_from_any_seed() {
        // main <- a <- b <- c (a linear stack of three MRs).
        let stack: &[(&str, &str, &str)] = &[("1", "main", "a"), ("2", "a", "b"), ("3", "b", "c")];
        let (parent, children) = links(stack);
        // Seeding from the middle still finds both ends.
        assert_eq!(
            from_seed(("2", "a", "b"), &parent, &children),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn discovers_both_arms_of_a_fork() {
        // main <- a, then a forks into b and c.
        let stack: &[(&str, &str, &str)] = &[
            ("1", "main", "a"),
            ("2", "a", "b"),
            ("3", "a", "c"),
            ("4", "c", "d"),
        ];
        let (parent, children) = links(stack);
        // From the base MR, every descendant on both arms is discovered.
        assert_eq!(
            from_seed(("1", "main", "a"), &parent, &children),
            ["1", "2", "3", "4"]
        );
        // And from a leaf, the whole tree is still reached via the shared trunk.
        assert_eq!(
            from_seed(("4", "c", "d"), &parent, &children),
            ["1", "2", "3", "4"]
        );
    }

    #[test]
    fn a_lone_mr_is_its_own_stack() {
        let stack: &[(&str, &str, &str)] = &[("9", "main", "solo")];
        let (parent, children) = links(stack);
        assert_eq!(from_seed(("9", "main", "solo"), &parent, &children), ["9"]);
    }

    fn summ(id: &str, base: &str, source: &str) -> MrSummary {
        MrSummary {
            id: id.into(),
            display: format!("#{id}"),
            state: MrState::Open,
            draft: false,
            title: String::new(),
            author: String::new(),
            base: base.into(),
            source: source.into(),
            head_sha: None,
            updated_at: String::new(),
            labels: Vec::new(),
            web_url: String::new(),
        }
    }

    fn seed_ids(summaries: &[MrSummary], trunk: Option<&str>, mode: StackMode) -> Vec<String> {
        stack_seeds(summaries, trunk, mode)
            .into_iter()
            .map(|(id, ..)| id)
            .collect()
    }

    #[test]
    fn auto_seeds_only_non_bottom_mrs() {
        // Three unrelated MRs on the trunk, plus a two-MR stack (#4 sits on #3).
        let summaries = [
            summ("1", "main", "fix-a"),
            summ("2", "main", "fix-b"),
            summ("3", "main", "feat"),   // bottom of the stack
            summ("4", "feat", "feat-2"), // sits on #3 (base is a feature branch)
            summ("9", "main", "solo"),
        ];
        // `auto` seeds only #4 (the one sitting on another). The bottom #3 is not
        // a seed, but #4's walk climbs to it, so it is not lost; and the three
        // lone MRs are left alone, costing no forge calls.
        assert_eq!(seed_ids(&summaries, Some("main"), StackMode::Auto), ["4"]);
    }

    #[test]
    fn modes_none_and_all() {
        let summaries = [
            summ("1", "main", "a"),      // lone
            summ("2", "feat", "feat-2"), // stacked
        ];
        // `none` never seeds anything.
        assert!(seed_ids(&summaries, Some("main"), StackMode::None).is_empty());
        // `all` seeds every MR, including the lone one — the zero-miss mode.
        assert_eq!(
            seed_ids(&summaries, Some("main"), StackMode::All),
            ["1", "2"]
        );
    }

    #[test]
    fn auto_on_a_lone_inbox_needs_no_completion() {
        // The common case that used to wait silently: nothing is stacked, so
        // `auto` seeds nothing and the feed does zero completion probing.
        let on_main = [summ("1", "main", "a"), summ("2", "main", "b")];
        assert!(seed_ids(&on_main, Some("main"), StackMode::Auto).is_empty());

        // With no trunk symref, the conventional names still count as trunks.
        let conventional = [summ("1", "main", "a"), summ("2", "master", "b")];
        assert!(seed_ids(&conventional, None, StackMode::Auto).is_empty());
    }

    #[test]
    fn many_seeds_in_one_stack_are_walked_once() {
        // A feed matched two MRs of the same three-MR stack. Seeding both must
        // yield the whole stack (the third member included) while probing each
        // branch only once — the shared visited set, not one walk per seed.
        let stack: &[(&str, &str, &str)] = &[("1", "main", "a"), ("2", "a", "b"), ("3", "b", "c")];
        let (parent, children) = links(stack);
        let probes = std::cell::Cell::new(0u32);
        let counted_children = |branch: &str| {
            probes.set(probes.get() + 1);
            children(branch)
        };
        let ids = discover_stack(
            [
                ("1".into(), "main".into(), "a".into()),
                ("2".into(), "a".into(), "b".into()),
            ],
            parent,
            counted_children,
        )
        .unwrap();
        assert_eq!(ids, ["1", "2", "3"]);
        // Three distinct source branches (a, b, c) => three children probes,
        // never one per seed.
        assert_eq!(probes.get(), 3, "each source branch probed exactly once");
    }
}
