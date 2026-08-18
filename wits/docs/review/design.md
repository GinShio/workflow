# `wits review` — Design

> Status: **implemented.** This records the agreed shape and the *why* behind it;
> the code lives in `crates/wits/src/cmd/review/` and the review half of
> `crates/wits-util/src/forge/`. Where a detail evolved during implementation the
> code is authoritative and this file has been kept in step (see §17 for what was
> deliberately scoped out).
>
> The *forge boundary* (§10/§11) and outdating (§6) were revised once during
> implementation to be **API-native** — grounded in the current GitHub GraphQL
> and GitLab draft-note APIs rather than one shape smeared over both. The sections
> below reflect that outcome; the rationale for the revision and the verified,
> field-by-field **API ground truth** it rests on are folded in as
> [Appendix A](#appendix-a--why-the-forge-boundary-is-api-native) and
> [Appendix B](#appendix-b--verified-api-ground-truth) at the end of this file
> (this doc is self-contained — there is no separate revision doc).
>
> This file explains *why the tool is shaped the way it is*. The companion usage
> document (`docs/review.md`) explains *how to drive it* and carries the full,
> reader-facing reference; the editor contract is in `docs/review/json.md` and the
> on-disk store in `docs/review/store.md`. Neither restates the other;
> behaviour-for-users goes there, rationale goes here.

---

## 1. What the tool is, and what it deliberately is not

`review` is the mirror image of `stack`. `stack`'s creed is *local topology is
given to us; we own the remote* — it manages the **existence and structure** of a
set of MRs. `review` manages the **review content** of an MR: the diff a reviewer
reads, the threads they leave, the verdict they render.

> One tool's job stated in one sentence:
>
> **`review` owns a local, plain-text model of review state (a pinned snapshot,
> its threads, and a pending verdict), and reconciles it against the forge. It
> never owns the code, never rebases, never pushes branches — `git` and `stack`
> do that.**

Two consequences fall straight out of that framing, and they are the spine of
everything below:

- **The target is any MR in the repo, not just my own stack.** I review other
  people's work far more than I re-read mine, so the tool cannot assume a local
  branch, a `.git/machete` entry, or authorship. Acquisition is therefore
  **forge-first** (§4): address an MR by number, ask the forge what it is, fetch
  its objects. Machete/`stack` integration (§13) is a convenience for reviewing
  *my own* stacks, not a prerequisite.
- **A review is pinned to a snapshot, and we never assume HEAD is latest.** The
  natural sequence is `commit A → I review → author pushes B, C → I sync → I
  submit`. My comments were about the code at `A`; they must be submittable
  against `A` even though the branch has moved. Snapshots and outdating are
  therefore first-class (§5, §6), not an afterthought.

Non-goals, stated once: no diff *rendering* (the editor and `git` do that; we own
diff *coordinates*, §5.2), no rebase/restack engine, no conflict resolution, and
no multi-user collaboration layer (the forge is that layer — our local store is
for carrying *my own* in-progress review across *my own* machines, §7).

(On naming: GitHub says PR, GitLab says MR. Internally and throughout this
document we say **MR**; the user-facing noun is a per-forge presentation detail,
already supplied by `Forge::noun`.)

## 2. CLI surface

The verb set is small because of two principles: **only two verbs touch the
network** (`fetch` reads, `submit` writes), and **authoring is not a command at
all** — you edit a plain `local.json` draft (§5.4), which is the sole place the
tool reads authored intent from. There is no `comment`/`verdict`/`resolve` verb;
that keeps a review from spraying a notification per keystroke, keeps the write
surface a single well-specified file, and matches the "plain-text formats"
preference. It is the `prr` model, generalized to threads, outdating, and stacks.

```
# Network — the only two verbs that talk to the forge.
wits review fetch  [mr | --feed name] [--stack auto|all|none]   # an MR+stack (full), a feed+stacks (light), or every feed (bare)
wits review submit [mr | --stack | --all]   # merge + flush the local.json draft, batched

# Reading — from the local files, no network; each supports --json.
wits review show  [mr] [--outdated|--resolved|--unread|--file P]   # inbox, or one MR (merged)
wits review diff  <mr> [--range S | --snapshot SHA] [--patch|--json]   # diff coordinates
wits review draft <mr> [FILE|-] [--dedup]                          # show, append, or compact local.json

# Materialize / housekeep.
wits review checkout <mr> [--next|--prev] [--in-place|--worktree DIR]
wits review prune [--older-than DAYS|DATE]
```

The split earns its keep the way `stack`'s does: when `submit` fails on the third
of five MRs you want to know exactly where you were and re-run only that, and the
reads are pure.

Notes on shape, each with its reason:

- **Authoring by editing `local.json`, not commands.** The draft is a public,
  versioned file (§5.4, `docs/review/json.md`). The tool owns the write: a
  front-end pipes a batch of actions to `draft <mr> -` (no need to know the store
  path), which appends, fills in missing action ids, and validates them; a human
  can edit the file directly. This one file is the write contract — the store's
  *read* layout is otherwise private.
- **`fetch` subsumes "pull" and "sync"** — one idempotent verb like `git fetch`.
  Bare `fetch` refreshes every configured feed (the RSS "refresh all"); `--feed`
  one feed; a number/URL one MR in full. Any of them **completes stacks** (§13):
  the members a feed's filter missed are pulled in so a stack is never left half
  in the store.
- **`show` with no MR is the inbox; with an MR it is the merged detail view.** No
  separate `list`; the human print is secondary to `--json` (§12).
- **Navigation is a flag on `checkout`.** `--next`/`--prev` walk the stack from
  the last checkout (§13); `show`/`diff` take an explicit `<mr>`, since the editor
  computes "next" from the `neighbors` block `show` returns.

Global `-v/--verbose` and `-n/--dry-run` come from the `wits` process layer for
free; every network call respects dry-run (`submit -n` prints what it would post),
every read still runs.

## 3. Code organization

`review` follows `stack`'s `core`/`util`/`cmd` line and reuses that layer whole.
The command's own logic lives under `cmd/review/`; the git-hosting concerns it
leans on stay in `wits_util::forge` and grow there (§10). Concretely:

- **Reuse unchanged:** `wits_util::forge::remote` (URL parsing, origin/upstream
  roles), `forge::detect` (which platform, which token), the `Repository` git
  floor, the `log`/`process` dry-run machinery, and `stack::resolution` for the
  "what is a stack" question when reviewing my own (§13).
- **Extend:** the `Forge` trait gains a review-facing half (§10) — added to the
  same trait, not a parallel one, so "add a forge" stays one self-contained
  mapping.
- **New, under `cmd/review/`:** the review model (§5), the local store (§7), the
  feed engine (§9), the anchor/outdate logic (§6), and the JSON contract (§12).

## 4. Acquisition — forge-first, and how objects are kept alive

Because the target is any MR, acquisition inverts `stack`'s branch-first model.

**Addressing.** The primary handle is the **MR number** (a URL is also accepted
and parsed to the same thing). Within one checkout we talk to exactly one target
repo — resolved as `stack` does, `Remotes::resolve` then `forge::detect` — and an
MR number is unique within that target, so the number alone is a complete
address. Batch acquisition names a **feed** instead (§9).

**Fetching objects, including across forks.** The forge exposes an MR's head
under a special ref on the *target* repo — `refs/pull/<n>/head` (GitHub),
`refs/merge-requests/<iid>/head` (GitLab) — and this works even when the source
branch lives in a fork we have no remote for. So `fetch` issues an explicit
refspec against the target remote (these refs are not in the default fetch set)
and pulls the snapshot's objects down. The fork's `owner/repo` never has to be a
local remote.

**Keeping snapshots alive without depending on another tool.** Once the author
force-pushes and we `fetch` the new head, the old snapshot's commit is no longer
reachable from any branch and becomes a GC candidate. We keep it alive by holding
our own ref:

```
refs/wits/review/<mr>/<snapshot-sha>        # → the snapshot's head commit
refs/wits/review/<mr>/<snapshot-sha>-base   # → its base, when not an ancestor of head
```

- The ref name carries only the **MR number** and the **snapshot SHA** — the two
  things that are actually required for uniqueness within a clone. Host and
  `owner/repo` are contextual to the target remote and live in the cache
  metadata, not the ref path.
- These refs *are* the record of "which snapshots we have pinned locally"
  (enumerable with `git for-each-ref`), so nothing duplicates that list into the
  MR's cached metadata (§7). Three distinct concerns, three distinct sources:
  **refs** = what we pinned; the **remote cache** = the forge's version history;
  the **MR info** = a pointer to the snapshot currently under review.
- This is unconditional and self-contained: our behaviour must not depend on
  whether any other tool happens to be installed, so these refs are the whole
  mechanism, always present.

`prune` (§15) is the other end of this: it deletes these refs when a snapshot is
no longer needed, letting git GC the objects.

## 5. The review model

Four types, no more, and each maps onto something a forge can actually represent.

### 5.1 `Snapshot` — outdating made structural

A review is always pinned to a **snapshot** — the `base/head` pair we diff and
the SHAs comments anchor against. It needs no type of its own: a snapshot *is* a
`DiffVersion { base_sha, start_sha, head_sha }` (the same triple the forge
boundary speaks), and the history is a `Vec<DiffVersion>` on `info.json`. When
each was first synced is deliberately not per-snapshot — that would make a
re-fetch of an unchanged head look dormant — so the last-sync time lives once on
the MR (`Info::fetched_at`) and `prune` reads it. **Branch outdate** is then just
`reviewed.head_sha != current head`; **comment outdate** is just "the anchored
line does not exist at the current head." Neither needs a state machine; both are
inferences from "the pinned snapshot is no longer the tip." We never assume the
tip is latest, so submitting against an old snapshot is the normal path, not an
error path (§6).

For GitLab, a snapshot also carries the forge's version SHAs
(`base_sha / start_sha / head_sha`) captured at `fetch`, because GitLab's comment
`position` requires them and they are not derivable from local git alone (§10,
capability A1).

A **snapshot is a distinct property from a diff range**: a snapshot is a
historical identity whose objects are pinned (§4), whereas a range is a throwaway
query. So `info.json` keeps the *history* of snapshots (each `fetch` that sees a
new head appends one), not just the latest — which is what lets you browse an
outdated context freely: `diff --snapshot <sha>` resolves to that pinned point's
`base..head`, and `show` lists the history for an editor to switch between.

### 5.2 `Anchor` — file coordinates, computed locally, translated at submit

`Anchor` is one enum, shared by the local read model and the forge boundary, with
each endpoint carrying its own side so a multi-line span can cross sides. Its
*absence* (`Option<Anchor>::None`) is the MR-level conversation:

```
LineRef { line: u32, side: Old | New }          // one endpoint

enum Anchor {
    Line {
        path,                       // new_path; old_path kept for renames/deletes
        old_path: Option<String>,
        end: LineRef,               // the anchor line (a single line when start is None)
        start: Option<LineRef>,     // the first line of a multi-line span, if any
    },
    File { path },                  // a whole changed file, no line
}
```

The snapshot a comment was written against (`DiffVersion {base, start, head}`) is
**not** part of the anchor: it rides on the thread (`commit`) and on the
`BatchAction` at submit time, resolved once from the snapshot history — so one
anchor shape serves the read view, the store, and both forge mappers unchanged.

Each endpoint carries its own `side` because a span can *cross sides* — a
comment starting on a deleted (old-side) line and ending on an added (new-side)
one. A flat `start_line` plus a single `side` could not express that, and GitLab's
`position.line_range` is inherently two-sided (`start`/`end`, each `{type,
old_line, new_line}`). The nested shape is the lowest common representation both
forges speak: GitHub gets `line`/`side`/`start_line`/`start_side`; GitLab gets
`line_range{start, end}`.

The deliberate choice is **file line numbers, not diff-hunk positions.** Modern
forges anchor by file line (GitHub, with `line`/`original_line`; GitLab's
`old_line`/`new_line`), so an anchor expressed in file coordinates lines up with
whatever diff the editor renders — the tool and the editor never have to agree on
one canonical diff text. The tool computes diffs internally only to *judge*
things (is this line changed, which commit last touched it, is this anchor now
outdated), never to *render* for the user. `review diff` therefore emits
coordinates and anchors; only `diff --patch` shells to `git` for a terminal
convenience.

### 5.3 `Thread` + `Comment`, and the three placements

```
Thread  { id, anchor: Option<Anchor>, commit: Option<Sha>, resolved: bool, outdated: bool, comments: Vec<Comment> }
Comment { id, author, body, origin: Local | Remote, created_at, state: Pending | Published }
```

A thread's **anchor** is one of three placements — `Anchor::Line`, `Anchor::File`,
or *absent* (`None`, the MR-level conversation) — and the boundary between them is
a forge reality, not a preference:

- **`line`** — anchored to a code line (a review comment). Allowed on any line of
  a file the MR *touches*, including unchanged/context lines — this is the
  "comment outside the diff hunk" requirement, and both target forges now support
  it (GitHub dropped the in-hunk restriction; GitLab shows the full changed
  file).
- **`file`** — anchored to a changed file but not a line (GitHub
  `subject_type: file`; GitLab a `position_type: file` diff note with the version
  SHAs and path, no line).
- **`mr`** — the MR-level conversation comment (GitHub's issue comment; a GitLab
  note with no position). This is "just leave a remark on the MR."

A line/file comment can only land on a file the MR changed; anything else is an
`mr` comment. In the read view, forge-owned ids are prefixed (`remote:9987`);
pending local items use their action id directly, so the editor can replace or
drop them without cross-referencing another payload. You address a remote thread
by its id when you write a reply or resolve into the draft.

### 5.4 `Local` — the editable draft file

```
Local {                          // local.json — hand/editor-edited
    verdict: Option<Approve | RequestChanges | Comment>,
    actions: [ Summary | Comment | Reply | Resolve | Drop ],   // append-style, id-addressed
}
// Summary { id, body }
// Comment { id, file?, line?, side?, start_line?, start_side?, body, commit? }
// Reply { id, thread, body }
// Resolve { id, thread, resolved }
// Drop { id }   // local-only removal of the live action with this id
//   side, start_side: New (default) or Old; start_side defaults to side
//   commit: the snapshot head SHA this comment's line anchors were written against
```

The draft is the **only** thing you write, and you write it *as a file*, not
through commands (§2). Every stored action has an `id`: clients may supply one,
or `draft <mr> -` generates a `wits:<uuid>` id before appending. Later actions
with the same id replace earlier ones, and `Drop` removes the current live action
with that id. `Summary` is the review body; `Comment` infers its placement from
the fields present (`file`+`line` → line, `file` → file, neither → mr), so the
file stays pleasant to hand-edit; `Reply` and `Resolve` name a remote thread. One
draft per MR (reviewing a stack means several); the verdict is one per MR. At
`submit` the append-only stream is compacted by id, posted, and cleared —
failures stay in the file to retry. Editing or deleting an *already-published*
comment is out of v1 scope (§17); the draft is only your unsubmitted intent.

Each `Comment` carries its own `commit` — the snapshot head SHA its line anchors
were written against. `draft <mr> -` stamps it at ingest; a hand-editor may set
it explicitly. At `submit`, `build_submission` resolves that SHA against the
snapshot history (`info.json`'s `snapshots[]`) to a full `DiffVersion`
(`{base, start, head}`), so a per-comment version — not just a head SHA — rides
with the comment into the forge boundary.

**Cross-snapshot anchoring is per-comment on GitLab, review-level on GitHub** —
an honest asymmetry of the two APIs, surfaced in the capability matrix (§10):

- **GitLab** anchors each diff note to its *own* `position{base_sha, start_sha,
  head_sha}`, so different comments in one draft can target different snapshots:
true **cross-snapshot drafting**, each marked outdated honestly when the branch
has moved on (§6).
- **GitHub** `addPullRequestReview` takes *one* top-level `commitOID` for the
  whole review; the per-comment `commit` cannot drive a per-comment anchor there.
  So the whole review batch anchors to the review's snapshot head (the current one
  unless a single comment dominates), and GitHub marks it outdated when that head
  is behind the branch tip. GitHub's standalone per-comment endpoint *does* carry
  a per-comment commit, but it fires one notification per comment — handing
  the §10 "one notification" guarantee to get per-comment anchoring was not worth
  it. The asymmetry is documented, not papered over.

Comments without a `commit` (hand-edited, pre-existing) are stamped with the
current snapshot at normalize time, so they anchor to the review's current head
on both backends.

A comment body may carry a `[[path:line]]` reference (repo-relative path,
optional `:line`/`:start-end`, optional `@ref` to pin another commit; default is
the reviewed head). `submit` expands it to a forge permalink so it renders as a
link there, while the stored body stays plain and portable — the expansion is a
forge concern (`Forge::permalink`), never baked into the draft.

## 6. Outdating — anchor to what you reviewed, let the forge mark it

The governing rule: **submit each comment against the SHA it was written on, and
let the forge display it as outdated.** We do not auto-re-anchor a comment onto
the new line — that risks pinning it to the wrong code. GitLab anchors each diff
note to its own version's SHAs (§5.4), per-comment; GitHub anchors the whole
review to one `commitOID` (§5.4, the documented API limit). Both accept a comment
written on an older snapshot and display it as outdated when the branch has moved
on. This is exactly the "outdated comments can still be pushed" requirement, and
it is why §4 pins the snapshot's objects and §5.1 keeps the version SHAs.

**Reading back — computed locally, uniformly.** `outdated` is *not* read off a
per-forge field (GitLab exposes none; GitHub only via GraphQL). It is a **local
inference**, the natural payoff of owning coordinates: `show` marks a line thread
outdated when its anchored line falls inside a region the file changed between the
commit the comment was written on (the thread's `commit`) and the current snapshot
head — `git diff <commit>..<head> -- <path>`, intersecting the line on its side,
computed from the objects `fetch` already pins (`recompute_outdated` in `show.rs`).

- **Uniform** across GitHub and GitLab, so the behaviour and its tests are one.
- **Offline** — a pure function over two commits, an anchor, and a diff; no
  network, easy to test.
- **Fallback only** — the forge's own signal (`isOutdated` on GitHub via GraphQL;
  nothing on GitLab) is used *only* when the anchor commit's objects aren't local
  (a thread on a commit we never fetched). `File`/`Mr` threads are never outdated.

The GC edge — reviewing `A`, sitting on the draft for a very long time, the author
force-pushing `A` away, and the forge eventually GC-ing it — is real but bounded:
we hold `A` locally regardless, forges retain pushed commits for a long time, and
the moment we first submit, our own comment references `A` and pins it on the
forge forever. If a submit against a vanished commit ever does fail, it is handled
per-action (§11): that comment stays in the draft with a warning, the rest go.

Moving a stale local draft onto the new state is offered only as an explicit,
opt-in convenience — never the default, never a correctness dependency. The
default is honesty: the comment stays anchored to the code it was written about.

## 7. Local store — three files per MR

Each MR is described by three JSON files in its own directory (`<id>/`), split
by lifetime and by who writes them, so no two concerns share a representation:

- **`info.json`** — the MR's necessary metadata and its snapshot history
  (summary, `snapshots[]`, current commits/files). Drives the inbox; a pure
  cache, regenerated by `fetch` — not for hand-editing.
- **`comments.json`** — the forge's discussion as last observed. A **cache**:
  refetchable at will, safe to overwrite whole. Everyone else's comments live
  here.
- **`local.json`** — your unsubmitted verdict + actions (§5.4). The **precious**
  part — the only thing that would be lost, and the only thing "carry my review
  to another machine" (portability, not collaboration) moves. It exists only
  while you have a draft.

All three are JSON because they are API-shaped data; only the *config* layer
(feeds, §8) is TOML. The read layout is otherwise an implementation detail, but
`local.json` is a public contract because it is the write interface (§2, §12).

**Submit clears `local.json`.** Once flushed, everything in it is public on the
forge, so it has nothing left to hold — landed actions are removed, and once the
file empties we re-`fetch` so the just-posted comments return as ordinary remote
threads. Local action ids are for editing the pending append-only draft; they are
not stitched to the remote thread ids after submit.

The store root follows the env → XDG_STATE → common-git-dir ladder, with **state**
kept distinct from **config** (§8):

```
WITS_REVIEW_DIR  >  $XDG_STATE_HOME/wits/review  >  <common-git-dir>/wits/review
```

The default is `<common-git-dir>/wits/review`, per-clone like `.git/machete`;
env/XDG lift it out when you want it centralized or shared across clones. It is
the **common** git dir (not the per-worktree one), so a `checkout` worktree and
the main clone share one store — you can review from either.

## 8. Configuration — git config for secrets, TOML for feeds

Two axes, split by what each is good at:

- **Identity and secrets** — token, service override, api-url — stay in **git
  config** under `wits.forge.*`, reused verbatim from `stack`. Per-host,
  per-repo, and appropriate for a secret. `review` needs nothing new here.
- **Review behaviour** — feeds and their filters (§9), cache policy — goes in
  **TOML**, because it is structured and will grow, and git config is a poor home
  for nested lists.

The TOML is a **single global file** with per-repo sections, borrowing `prr`'s
"one global config, target on the command line" shape but keyed by the parsed
remote identity so it can hold many repos:

```toml
# $XDG_CONFIG_HOME/wits/review.toml   (overridable via WITS_REVIEW_CONFIG)

[repo."github.com/mesa/mesa"]
feed.mine   = { reviewer = "@me", state = "open+draft" }
feed.vulkan = { labels = ["vulkan"], state = "open+draft" }
```

Two deliberate points:

- **Config (`XDG_CONFIG`) and state (`XDG_STATE`, §7) live in different trees** —
  the correct XDG split, and it keeps a backup of one from dragging in the other.
- **Graceful degradation, as in `stack`.** A token alone is enough to
  `fetch <number>` and review any single MR anywhere; the TOML only adds the
  *batch/subscription* layer. "A repo without config isn't supported" means "has
  no feeds", not "can't be reviewed". Personal review preferences are therefore
  never committed into someone else's repo.

## 9. Feeds — an RSS-shaped subscription, filtered server-side

A repo like mesa or llvm has more open MRs than we could ever fetch, so batch
acquisition must be a *subscription*, not "fetch everything then filter". A feed
is a named set of faceted filters:

- **Fields:** `state` (defaults to `open+draft`; `merged`/`closed` are not
  fetched), `label`, `author`, `assignee`, `reviewer`.
- **Semantics:** different fields are **AND** (`reviewer=@me` *and*
  `label∈{…}` *and* `state∈{open,draft}`). This is the faceted model
  `gh pr list` / `glab mr list` and the forges' own search use; a full expression
  language would be over-built for the need. (In v1 multiple *labels* are AND-ed
  on both GitHub and GitLab — that is the platforms' behaviour for one list/search
  query; the earlier hope of within-field OR for labels isn't natively available
  and client-side union was rejected for scale.)
- **Exclusion:** an `exclude-labels` list drops MRs carrying any of them, the one
  extension that pays for itself (bot/WIP noise).
- **Escape hatch:** a `search = "..."` string is passed straight to the
  platform's search endpoint for the rare full-text case, so we never invent a
  query syntax.

The load-bearing decision: **filters are pushed down to the forge's list/search
query and paginated server-side** — never applied client-side after a full fetch,
with a hard `limit` cap (default 30). A truly incremental `updated_at` cursor
(only MRs touched since the last sync) is plumbed through the query but unused in
v1 (§17); the cap alone keeps a large repo's inbox bounded.

`prr` has no filtering (it names specific PRs), so it is no guide here; the real
analogues are the forge `list` CLIs and an RSS reader's filter rules.

## 10. Forge extension and the honest capability matrix

The review half is **added to the existing `Forge` trait**, not split into a
parallel one, so adding a platform stays a single self-contained mapping. The
write side is **one** primitive — the forge owns the mapping of a whole review
onto its native batch, rather than the orchestration layer assuming a fixed
shape:

```rust
// Added to Forge, alongside find/create/set_base/set_body/apply_attributes:
fn list_mrs(&self, q: &FeedQuery) -> Result<Vec<MrSummary>>;   // feed
fn mr_details(&self, mr: &str) -> Result<MrDetails>;
fn list_threads(&self, mr: &str) -> Result<Vec<RemoteThread>>;   // + resolved/outdated
fn submit(&self, mr: &str, batch: &ReviewBatch) -> Result<BatchOutcome>;   // the whole review
```

`ReviewBatch` carries the verdict, summary, and every action (comment
line/file/MR-level, reply, resolve), each tagged with a stable `ActionKey`.
`submit` folds as many actions as the platform's native batch allows into one
notification, does the rest as separate calls, and returns a **granular**
`BatchOutcome` — `landed[key]` per action, plus `summary_ok`, `verdict_ok`, and an
honest `notifications` count. An `Err` means *nothing* landed (an atomic backend's
hard failure, or an all-or-nothing batch rolled back); any partial success is
`Ok` with the per-action map filled in, so the orchestration layer reconciles each
action independently *by key* (§11). This replaced the earlier split of
`submit_review` + separate `comment_mr`/`reply`/`resolve` verbs, which forced a
lowest-common-denominator "batch = line/file comments only" shape that fit neither
platform.

As with the MR half, no platform JSON shape escapes a host module. But review
touches corners of the platforms that do **not** normalize cleanly, and pretending
otherwise would be the mistake. These are documented as a matrix rather than
hidden, because a reviewer needs to know them:

| Concern | GitHub (GraphQL) | GitLab (REST) |
|---|---|---|
| Batch mechanism | `addPullRequestReview` (one review → one notification) | draft notes → one `bulk_publish` (publishes all of the user's pending drafts) |
| Verdict + summary + line comments | one review call | line comments **and the summary** ride one `bulk_publish` (the summary as a position-less draft note — the released `bulk_publish` takes **no body**); the verdict is a **separate** call (A3) |
| **File-level** comment in the batch | yes — pending review + `addPullRequestReviewThread(subjectType: FILE)` + submit (still one review) | yes — `position_type: file` draft note |
| **MR-level** (conversation) comment | **separate** `addComment` (its own notification) | position-less draft note (rides the batch) |
| Reply to an existing thread | **rides the review** — `addPullRequestReviewThreadReply` with the *pending* review's id, published on submit (one notification, exactly as the web UI does) | draft note with `in_reply_to_discussion_id` (rides the batch) |
| Resolve / unresolve | **supported** — `resolveReviewThread`/`unresolveReviewThread` | separate `PUT …/discussions/:id` (a draft note needs a body, so a bare resolve can't ride the batch) |
| `request-changes` verdict | native (`REQUEST_CHANGES`) | **no released API** — mapped to `POST …/unapprove`, its concrete released effect (A3) |
| `comment` verdict | native (`COMMENT`) | **no-op** — leaving notes no longer sets `reviewed`, and no released API sets it; the comments/summary are the review (A3) |
| `approve` verdict | part of the review | **separate `POST …/approve`** (A3) |
| Partial failure handling | the review call is atomic — failure leaves nothing | **all-or-nothing per attempt**: a draft failure aborts the publish and rolls back (DELETE) what posted (§11) |
| Diff-line comment anchor | one review-level `commitOID`; the batch anchors to one snapshot | **per-comment** `position{base/start/head_sha}`; each anchors to its own version (A1) |
| Cross-snapshot drafting | not in one review (one `commitOID`) | **delivered** — each comment carries its own version |
| Multi-line span | `line`/`side` + `startLine`/`startSide` (two-sided) | `position.line_range{start, end}` (two-sided) |
| Feed returns a real MR (base/head) | yes — GraphQL `search(type: ISSUE, "is:pr")` returns `PullRequest` nodes | yes — the MR list |
| `resolved` read-back | `isResolved` | notes' `resolved`/`resolvable` |
| `outdated` | **computed locally** (§6); forge `isOutdated` only as fallback | **computed locally** (§6); no forge fallback |

The named caveats behind the matrix:

- **A1 — GitLab diff anchors need forge SHAs.** A line/file comment on GitLab can
  only target a commit the forge knows as an MR *version*. Normal review (of
  pushed MR commits) always satisfies this; commenting on an un-pushed local
  intermediate commit does not, and degrades to an `mr` comment. GitHub is more
  lenient. Captured by keeping the version SHAs on the snapshot (§5.1).
- **A3 — GitLab verdicts map onto the *released* API, not the unmerged
  `bulk_publish` extension.** A `reviewer_state`/`note` body on `bulk_publish`
  would let the verdict and summary ride the one publish, but that is the
  **unmerged** proposal gitlab-org/gitlab!237813 — it is in *no* shipped release,
  and GitLab's Grape API silently ignores the undeclared params, so relying on it
  would drop the summary and verdict while reporting success. So the released
  surface is used instead: the **summary** rides as a position-less draft note
  (published by the bodyless `bulk_publish`); **`approve`** is `POST …/approve`;
  **`request-changes`** is `POST …/unapprove` (there is no released API for the
  formal `requested_changes` reviewer state, and unapproving is its concrete
  released effect — a reviewer requesting changes has their approval removed);
  **`comment`** is a no-op (leaving notes no longer auto-sets `reviewed`, and no
  released API sets it). The `draft` boolean list filter *is* released (GitLab
  ≥ 16), so the feed path is unaffected.
- **A5 — batching is best-effort per platform, and `notifications` is honest.**
  On GitLab the comments, replies **and the summary** fold into one
  `bulk_publish`; a bare resolve is a separate quiet PUT, and the verdict is a
  separate `approve`/`unapprove` call (A3). On GitHub the review (verdict +
  summary + line/file comments **and replies**) is one notification — replies
  join the *pending* review by id and publish with it, the same fold the web UI
  performs. The one unfoldable case is an MR-level conversation comment: it is a
  separate `addComment` (a real API limit) with its own notification. Resolves
  are separate mutations but do not notify. `BatchOutcome.notifications` reports
  the true count rather than promising "one".

Transport is **GraphQL over `ureq` on GitHub** (the whole forge — threads,
resolution, PR search, and the stack half all live there; REST is used only for
the object-fetch refspec, a git operation) and **REST over `ureq` on GitLab**.

## 11. Submit — batched, concurrent, reconciled per action

`submit` flushes drafts, and its correctness rests on getting the granularity
right:

- **Concurrency is per-MR and bounded.** Unlike `stack submit` (which
  serializes sibling MR *creation* to dodge duplicate-detection races), review
  submissions to different MRs are wholly independent, so they fan out over
  scoped threads (up to 8 at a time, matching `stack`'s `map_parallel`) with no
  ordering constraint.
- **Reconciliation is per action, by key.** `build_batch` turns the normalized
  draft into a `ReviewBatch`, tagging each action with its persistent local
  action id. `forge.submit` returns `BatchOutcome.landed[key]`; `submit` then
  keeps exactly the actions whose key did *not* land, clears summary actions when
  `summary_ok` is true, and clears `verdict` when its flag lands. This replaced
  the earlier positional walk (line/
  file comments matched to a `Vec<bool>` by order, everything else by side calls),
  which was correct-but-fragile. An `Err` from `submit` means nothing landed — the
  whole (normalized) draft stays.
- **Cleanup is deferred and id-keyed, not this-attempt rollback.** Both backends
  can create forge-side state (GitLab draft notes, a GitHub *pending* review) that
  they then fail to publish. Rather than `DELETE`-ing it in the same attempt —
  where the delete can *itself* fail, leaving an orphan a later run republishes —
  the failing attempt **records the ids it left behind** in `BatchOutcome.inflight`
  and returns "nothing landed". `submit` persists them (`<id>/inflight.json`), and
  the *next* attempt hands them back as `ReviewBatch.stale`; the backend deletes
  them **first**, before doing anything else. Cleanup is thus idempotent and
  eventually-consistent (retried every attempt until it succeeds), and it only
  ever deletes ids **we** recorded — a draft or review the user created by hand is
  never touched (which is why blanket pre-flight deletion, §16, was rejected but
  this keyed form is safe). `submit --all` includes MRs that have only a pending
  cleanup, so an orphan is chased down even after its draft is gone.
- **GitLab: nothing publishes until the slate is clean.** Pre-flight deletes the
  recorded `stale` draft ids (a `404` counts as already-gone); a *real* delete
  failure aborts before any POST — so an undeleted orphan can never be swept into
  this run's `bulk_publish` and duplicated. A draft-note POST failure, or a
  `bulk_publish` failure, records the posted-but-unpublished ids as `inflight` and
  aborts (no this-attempt delete). The summary rides as a position-less draft
  note; the verdict is a separate call on the released API
  (`approve`→`/approve`, `request-changes`→`/unapprove`, `comment`→no-op; A3);
  resolves are separate PUTs, reconciled by their own key.
- **GitHub: the single-pending-review invariant makes cleanup self-healing.** A
  PR allows one pending review per user, so a leftover orphan is unambiguous.
  Pre-flight best-effort `deletePullRequestReview`s the recorded `stale` id; if
  that delete transiently fails, the create below fails ("already pending"), and
  we *re-discover* the orphan's id (`reviews(states: PENDING)`, `viewerDidAuthor`)
  and record it again — so the next attempt retries the delete. On submit failure
  the orphaned pending review's id is recorded as `inflight`. The atomic path (no
  file comments or replies) creates no pending review, so it can't orphan. MR-level
  conversation comments and resolves remain independent calls, reconciled by key.
- **The local write happens per-MR inside the fan-out.** Each MR writes to a
  distinct store path (`<id>/local.json`, `<id>/inflight.json`), so the per-MR
  tasks are independent and there is never a race or a half-cleared draft; the
  safety comes from path
  isolation, not from a single post-join write pass.

## 12. Editor interface — read via `--json`, write by editing `local.json`

The editor is a client of the tool, and the boundary is two contracts:

- **Read through `--json`, never the other store files.** The layout of
  `info.json`/`comments.json` is a private implementation detail; the stable read
  contract is the JSON emitted by `show`/`diff`/`draft`, versioned by a `schema`
  integer. `draft --json` echoes `local.json`.
- **Write via `local.json`.** The one write interface is that file (§5.4, §7).
  The tool owns the write: an editor pipes a batch of actions to
  `draft <mr> -` (which appends + validates) so it needn't know the store path or
  format; a human can edit the file directly. There is no per-action command
  surface — a single well-specified file, plus one ingest verb, is smaller and
  easier to fuzz than a family of verbs, and matches the plain-text preference.
- **`show` returns the whole MR in one payload, and filtering is the knob.**
  Even a large MR is a small JSON document and cheap I/O; pagination is not worth
  the complexity. What is worth it is *filtering* — `--outdated`, `--resolved`,
  `--unread`, `--file` — so the editor (or a terminal user) can pull just the
  threads that matter.

The exact, field-by-field payloads (`show` detail and inbox, `diff`, and the
`local.json` write contract) are specified in `docs/review/json.md`; this section
records only the boundary, not the shapes.

A long-running `serve` daemon speaking JSON-RPC is a **possible future**
optimization — it would keep parsed diffs, computed anchors, and a warm forge
session in memory, push outdate/CI changes to the editor, and serialize writes
natively. It is explicitly *not* v1: the whole codebase is currently zero-IPC, and
a daemon is a cache layer over the *same* `--json` contract, not a redesign, so it
can be added later without breaking anything.

## 13. Stack integration — navigate freely, but a comment belongs to one MR

Reviewing a stack is jumping between its MRs, and the tool should make that fluid
without inventing anything the forge can't store.

- **Stack shape is reconstructed from the MR list, not required from machete.** A
  chain of MRs whose base branches link head-to-tail *is* a stack; we read it off
  the fetched MRs, so `review` is stack-aware for *anyone's* stack. The linking
  is by **branch** (`base` ↔ `source`), never by MR number, so non-contiguous or
  non-increasing numbers are fine (a stack's MRs are opened in parallel, so their
  numbers need not increase with position) — the topology is correct regardless.
- **Every `fetch` completes the stack, so there is no gap to fix by hand.** A
  label/limit feed can match only part of a stack, which would leave the
  reconstruction stopping at the first unfetched node. So `fetch` closes that
  itself: from each fetched MR it walks the same `base`↔`source` links *on the
  forge* — `find_any(base)` climbs to the parent, `find_children(source)`
  descends to the children — out to the whole connected stack, and pulls in any
  member the filter missed (a feed does this **lightly**, summary-only; `fetch
  <mr>` does it **fully**). The walk is bounded to the real stack — it stops at a
  trunk and never enumerates a trunk's MRs — so it never drags in unrelated work.
  How eagerly it completes is one `--stack` setting (also a per-feed default):
  `auto` (the default) only seeds from an MR that sits on another — so a lone or
  bottom MR costs no probing — which leaves one blind spot, a stack whose only
  fetched member is its bottom; `all` completes even from the bottom (a children
  probe per bottom MR) for the zero-miss guarantee; `none` fetches just the named
  MR. Re-deriving happens on every `fetch`, so a rebase that reshapes the stack
  is picked up on the next fetch, not guessed at.
- **Navigation, not cross-MR comments.** `checkout --next/--prev` walks the chain
  (relative to a small per-repo pointer recording the last checkout, §14), and
  `show` hands the editor a `neighbors` block so it can do the same. But a
  **comment always belongs to exactly one MR** — the forge has no object for a
  comment spanning two MRs, and faking one would be an abstraction with nowhere to
  push. A "stack review" is therefore a connected *session* — per-MR drafts you
  build while hopping nodes and then `submit --stack` in one go — not a shared
  comment surface. Each MR keeps its own verdict, which is usually what you want
  (approve the base, request changes up top).

## 14. Materialization — worktree or in-place

To debug/build/fuzz/test an MR's code, the tool must put it somewhere runnable
without clobbering your current work. `checkout` reuses the unified git module's
worktree/checkout operations (`wits_util::git::Repository`) and supports both modes,
chosen per invocation so neither is forced on the user:

- **Worktree mode (default):** a **single** review worktree, a sibling of the
  **main** worktree at `../<main-worktree-name>.review`, reused for every MR —
  `checkout <mr>` and `--next`/`--prev` just switch its HEAD to the target
  snapshot. This is the load-bearing simplification: a stack of N MRs is
  inherently reviewed in sequence, so it costs **one** worktree and **one**
  submodule tree, not N — decisive for a repo with heavy submodules. The
  location is anchored to the main worktree (not whichever worktree the command
  runs from), so it resolves to the same place from the main clone or any linked
  worktree. Switching HEAD hard-guards a dirty tree (tracked changes; untracked
  build output is preserved), and the worktree is **not owned by any one MR** —
  so pruning a merged member never disturbs the review of its open siblings; the
  worktree is reclaimed only once the store is empty (§15). `--worktree DIR`
  overrides the location for a one-off separate slot.
- **In-place mode (`--in-place`):** check the snapshot out in the current working
  tree. *Supported on purpose* (not everyone uses worktrees), but it moves HEAD
  and hosts one review at a time, so it **hard-guards a dirty tree** — reviewing
  someone else's MR must never silently bury your work.

Submodules follow the same one-worktree logic: `--submodules` **borrows** objects
from the primary store on a submodule's first materialization (so even large
submodules cost no re-download), and on a later HEAD switch merely **updates** the
already-present submodules to the new snapshot's pins — the borrow is a one-time
concern, not redone per switch.

A small per-repo `current` pointer records the last checkout so `--next/--prev`
has an origin (§13). Pruning the checked-out MR clears it (the worktree stays,
still showing that snapshot until the next checkout — a cheap HEAD switch away).

## 15. Lifecycle and `prune`

An MR ends its life two ways, and only one is unambiguous:

- **Explicit (merged/closed).** `fetch` observes the terminal state and marks it;
  such MRs are the clear targets of `prune`.
- **Implicit (long dormant).** Never auto-deleted — we can't be sure it's dead —
  but reachable by an optional `--older-than`.

The cost of *not* pruning is deliberately bounded so that doing nothing is fine:
the JSON files are kilobytes, and the only real weight is git objects held alive
by the `refs/wits/review/*` pins (§4). Pins are created by a **full** `fetch <mr>`
only; a merely feed-listed MR pins nothing. `prune` then mirrors
`stack tree prune`: idempotent, automatable, a no-op when nothing dangles — it
drops the pins and the store directory of terminal MRs (and, with `--older-than`,
dormant ones) and lets git GC the objects.

## 16. Rejected alternatives

- **Depend on the forge's rendered diff.** Rejected: computing diffs from fetched
  objects locally keeps us offline-capable, consistent with local content, and
  consistent with `stack`'s fidelity argument for the `git` CLI. We own
  coordinates; the editor renders.
- **A parallel `ReviewForge` trait.** Rejected: it would make "add a forge" two
  mappings in two places. Extending the one `Forge` trait keeps it cohesive.
- **A daemon/RPC protocol in v1.** Rejected as premature (§12); it is a future
  cache layer over the same JSON contract, not a foundation.
- **A mutable, in-place JSON blob per MR.** Rejected in favour of the
  cache/draft split (§7): conflating the disposable remote view with the precious
  local intent is how the store rots.
- **Semantic duplicate detection for the local draft.** Rejected in favour of
  explicit action ids: clients that know they are modifying or removing an
  earlier pending action say so by reusing its id or appending `drop`. The tool
  should not infer intent from similar bodies or nearby anchors.
- **Auto-re-anchoring outdated comments onto the new line.** Rejected as a default
  (§6): it can pin a comment to the wrong code. Honesty (anchor to the reviewed
  SHA) is the default; re-anchoring is an explicit opt-in.
- **Splitting a GitHub review into per-comment `commit_id`s.** Rejected as a
  default for the cross-snapshot case (§5.4): GitHub's batched-review API takes
  one top-level `commit_id`, so per-comment anchoring there means splitting the
  draft into multiple review POSTs (one per commit) — which fires one notification
  per review, breaching the §10 "one notification" matrix guarantee. Purposely not
  bought; cross-snapshot anchoring stays GitLab-delivered and the asymmetry is
  documented.
- **`GitLab bulk_publish` with an explicit `draft_ids` body / pre-flight cleanup
  of pre-existing drafts.** Rejected: `bulk_publish` publishes *all* of the
  user's pending drafts on the MR and ignores a `draft_ids` body. Pre-flight
  deleting existing drafts would risk discarding drafts the user built by hand in
  the GitLab UI. Instead the backend owns atomicity per attempt — any failure
  rolls back *this* attempt's drafts by id; double-failure is bounded (§11).
- **A flat `start_line` + single `side` anchor.** Rejected in favour of the
  nested two-sided `LineRef` (§5.2): a span that crosses the delete/add boundary
  (an old-side start, a new-side end) could not be expressed, and GitLab's
  `line_range` is inherently two-sided. The nested shape is the honest common
  representation; the flat, hand-friendly `start_line`/`start_side` lives in the
  `local.json` write contract.
- **Cross-MR comments on a stack.** Rejected (§13): no forge object backs it.
- **Folding `review` into `wits stack`.** Rejected: it is its own subcommand with
  its own verbs; it only *reuses* `stack`'s resolution.

## 17. What is delivered, and future work

Delivered: forge-first acquisition with object pinning and **snapshot history**,
the snapshot/anchor/thread model with a hand-edited `local.json` draft (plus a
`draft <mr> -` ingest verb so the tool owns the write), per-comment snapshot
anchoring with **cross-snapshot drafting on GitLab**, the three-file store on the
env→XDG_STATE→**git-common-dir** ladder (worktree-safe), config-driven feeds with
server-side filtering that return **real MR objects on both forges** (GitHub via a
GraphQL `search`, so feed-fetched PRs carry base/head), the `--json` read contract
and the `local.json` write contract (`schema` 1), `[[path:line]]` reference
expansion, two-sided multi-line spans, worktree/in-place materialization with
stack navigation, `prune` (day count or ISO date), and parallel submit with
**per-action, key-based `BatchOutcome` reconciliation**.

Delivered by the API-native revision (§10/§11; rationale in Appendix A): the
**whole GitHub forge on GraphQL**; **thread resolve/unresolve on both forges**
(GitHub `resolveReviewThread`, GitLab discussion PUT); GitLab's **single
`bulk_publish`** carrying comments + replies + **summary** (as a position-less
draft note — the released `bulk_publish` has no body), with the **verdict mapped
onto the released API** (`approve`→`/approve`, `request-changes`→`/unapprove`,
`comment`→no-op; A3), since the `bulk_publish` `reviewer_state`/`note` extension
is unmerged and ships in no release; and **locally-computed, uniform, side-aware
outdating** (§6).

Deliberately deferred, and honest about it:

- **future** editing or deleting an *already-published* comment; the draft is
  only your *unsubmitted* intent (§5.4).
- **future** GitHub per-comment cross-snapshot anchoring. Per-comment
  **anchoring** is delivered on GitLab (each comment carries its `commit` and
  resolves it to a `DiffVersion` that anchors the diff note — §5.4); GitHub's
  batched-review API accepts one top-level `commitOID` per review, so the batch
  anchors to the review's snapshot there. GitHub's standalone-comment endpoint
  does take a per-comment `commit_id` but fires a notification per comment — not
  worth the §10 "one notification" guarantee.
- **future** per-comment snapshot *pinning* for commits outside the fetched
  snapshot history. The deferred part is holding git objects alive for arbitrary
  commits that aren't already pinned by a fetch; anchoring (resolving
  `commit` → `DiffVersion` from the snapshot history) is delivered.
- **future** the incremental-sync cursor for feeds (v1 pulls the most-recently
  updated MRs up to `limit`; the `updated_after` plumbing exists but is unused —
  §9), and a feed cache-expiry policy.
- **future** Gitea/Forgejo/Codeberg review backends (the trait leaves the seam,
  §10).
- **future** a `serve` daemon over the `--json` contract for large-MR latency and
  live outdate/CI push (§12).
- **future** CI status surfaced into `show` (shared with `stack`'s own deferred CI
  read-back).

## Appendix A — why the forge boundary is API-native

The first cut of the forge layer picked one intermediate shape — *"a batch is a
verdict + summary + line/file comments; everything else is a separate call"* —
and bent each backend to fit. Held against the real APIs, that shape fit
**neither** platform, so §10/§11 were revised to let each `Forge` own the mapping
of one rich review batch onto its native primitive and report honestly what it
could and couldn't do. The three findings that forced the revision:

- **GitLab can batch more than the old shape allowed.** Its draft-notes +
  `bulk_publish` primitive carries line comments, file comments, MR-level notes,
  replies, and the summary as one publish — one notification. The first cut
  instead posted the summary as a lone draft, published with an empty body, then
  fired separate calls for replies and resolves.
- **GitHub REST could batch *less* than the old shape assumed.** The REST review
  endpoint takes only line comments (no `subject_type`), silently dropping the
  file-level comments the old code sent inside the batch, and it cannot read or
  set thread resolution. So the whole GitHub forge moved to **GraphQL**, where
  reviews are actually modelled (threads with `isResolved`/`isOutdated`,
  resolution mutations, and a `search` that returns real `PullRequest` objects).
- **Outdating cannot be read uniformly off either forge** (GitLab exposes no
  cheap per-thread flag; GitHub only via GraphQL), so it became a **local**
  inference over the pinned objects (§6) — uniform, offline, and testable.

One correction landed late, when the mapping was checked against *release status*
rather than just the docs: the `reviewer_state`/`note` body on GitLab's
`bulk_publish` (which would fold the verdict and summary into the one publish) is
an **unmerged proposal** (gitlab-org/gitlab!237813), present in no shipped
release and silently ignored by the API if sent. So the summary rides as a
position-less draft note and the verdict is a separate released
`approve`/`unapprove`/no-op call (§10 A3). If that proposal ships, folding both
back into the one publish is a version-gated optimisation, never a correctness
dependency.

## Appendix B — verified API ground truth

The boundary rests on these concrete facts, recorded so a future change can be
checked against them rather than re-derived. (Sources: the official GitHub
GraphQL and GitLab REST references, re-checked against *release status* in
2026-07.)

### B.1 GitHub GraphQL (the whole forge)

| Need | Mutation / query | Key fields |
|---|---|---|
| Submit a review (verdict + summary + line threads) | `addPullRequestReview` | `pullRequestId`, `commitOID`, `event` (`COMMENT`/`APPROVE`/`REQUEST_CHANGES`; omit ⇒ **PENDING**), `body`, `threads: [DraftPullRequestReviewThread]` |
| A line thread in that batch | `DraftPullRequestReviewThread` | `body!`, `path`, `line!`, `side`, `startLine`, `startSide` — **no `subjectType`** (so no file-level here) |
| A **file-level** thread | `addPullRequestReviewThread` on the pending review | `pullRequestReviewId`, `path`, `subjectType: FILE`, `body!` |
| Publish a pending review | `submitPullRequestReview` | `pullRequestReviewId`, `event`, `body` |
| Reply into a thread (rides the review) | `addPullRequestReviewThreadReply` | `pullRequestReviewId` (the *pending* review), `pullRequestReviewThreadId!`, `body!` |
| Resolve / unresolve | `resolveReviewThread` / `unresolveReviewThread` | `threadId` (`PRRT_…`) |
| MR-level (conversation) comment | `addComment` | `subjectId` (PR node id), `body` — an issue comment, **not** part of the review (its own notification) |
| Read threads | `pullRequest.reviewThreads.nodes` | `id`, `isResolved`, `isOutdated`, `path`, `line`/`originalLine`, `startLine`/`originalStartLine`, `diffSide`/`startDiffSide`, `subjectType`, `comments{ nodes{ databaseId, author, body, createdAt, originalCommit{oid} } }` |
| Feed / details | `search(type: ISSUE, "repo:o/r is:pr …")` → `... on PullRequest` | `number`, `title`, `author{login}`, `baseRefName`, `headRefName`, `headRefOid`, `state`, `isDraft`, `labels`, `updatedAt`, `url` |

A review of only line comments/summary/verdict is one atomic
`addPullRequestReview`. Add file comments or replies and it becomes the pending
flow (create pending → attach FILE threads and replies by review id → submit) —
still one review, one notification. An empty `COMMENT`/`REQUEST_CHANGES` review
(no body, no comments) is rejected; only `APPROVE` may be empty.

### B.2 GitLab REST (the review backend)

| Need | Endpoint | Key fields |
|---|---|---|
| Draft a diff/file/reply/summary note | `POST …/merge_requests/:iid/draft_notes` | `note!`; `position{base_sha,start_sha,head_sha,new_path,old_path,new_line/old_line,line_range,position_type}` (`file` since 16.4; a `line_range` endpoint **requires** `line_code` = `SHA1(path)_old_new`); `in_reply_to_discussion_id`; `resolve_discussion` — a note with no `position` is an MR-level/summary note |
| Publish the whole batch | `POST …/draft_notes/bulk_publish` | **no body** in any shipped release (the `reviewer_state`/`note` body is the *unmerged* !237813); publishes *all* of the user's pending drafts as one review |
| Set the verdict | `POST …/approve` / `POST …/unapprove` | the only released reviewer actions; there is **no** released API to set the formal `reviewed`/`requested_changes` reviewer state |
| Resolve a discussion | `PUT …/discussions/:id` | `resolved` (a bare resolve can't ride the batch — a draft note needs a body) |
| Delete a draft (deferred cleanup) | `DELETE …/draft_notes/:id` | 404 ⇒ already gone |
| Read discussions | `GET …/merge_requests/:iid/discussions` | notes with `position`, `resolvable`, `resolved`, `system` |
| Feed | `GET …/merge_requests?state=opened&…` | server-side `labels`/`author`/`assignee`/`reviewer`/`draft` filters; returns `iid`, `target_branch`, `source_branch`, `sha`, `draft`, `labels`, … |

One bodyless `bulk_publish` publishes the comments, replies and summary together
(one notification); the verdict is a separate call afterwards
(`approve`→`/approve`, `request-changes`→`/unapprove`, `comment`→no-op).
