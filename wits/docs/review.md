# `wits review`

Review merge requests locally, across forges, from your editor or the terminal.
`wits review` is the mirror image of [`wits stack`](stack.md): where `stack` owns
the *existence and structure* of a set of MRs, `review` owns their *review
content* — the diff you read, the threads you leave, the verdict you render. It
never touches the code or the branches; it fetches an MR's objects, lets you
build a review in a local file, and submits it as one batch.

> Terminology: GitHub says *pull request*, GitLab *merge request*. This tool says
> **MR** everywhere; on a GitHub repo the output just says "PR".

Two ideas shape everything:

1. **Any MR, not just yours.** You address an MR by number; the tool asks the
   forge what it is and fetches its objects. No local branch, no authorship
   required.
2. **You author by editing a file, not by running commands.** There are no
   `comment`/`verdict`/`resolve` verbs. You edit a plain `local.json` draft (by
   hand or through an editor), and two verbs move data over the network: `fetch`
   reads, `submit` writes.

The rationale is in [`review/design.md`](review/design.md); the exact JSON shapes
are in [`review/json.md`](review/json.md); the on-disk store and how to move it
between machines are in [`review/store.md`](review/store.md). This guide is the
getting-things-done level.

## The mental model

Each MR is described by three local files (details in [store.md](review/store.md)):

| File | Holds | Written by |
|---|---|---|
| `info.json` | the MR's metadata and diff state (the inbox row) | `fetch` |
| `comments.json` | the forge's discussion (a refetchable cache) | `fetch` |
| `local.json` | **your** unsubmitted verdict + append-only review actions | **you** (edit it) |

```
fetch  ─────────►  info.json + comments.json        ◄── you edit ──  local.json
(network read)     (+ pinned objects)                                    │
                                                                         ▼
                                                                      submit
                                                                  (network write)
```

- `fetch` pulls an MR's metadata, objects (pinned so a later force-push can't
  lose them), and discussion — and, by default, the rest of the MR's stack too.
- You edit `local.json` to record your review — nothing reaches the forge.
- `submit` merges and posts the draft as one batched review, clears it, and
  re-fetches, so your just-posted comments come back as ordinary remote threads.

## One-time setup

### A token for your forge

`fetch` and `submit` need a forge API token, configured exactly as for `stack`:

| Where | Example |
|---|---|
| git config, per host | `git config wits.forge.github.com.token ghp_xxx` |
| git config, blanket | `git config wits.forge.token ghp_xxx` |
| environment | `export GITHUB_TOKEN=ghp_xxx` (or `GITLAB_TOKEN`) |

The forge (GitHub or GitLab in this version) is detected from the **upstream**
remote's URL, or `origin` when there is no upstream. A self-hosted instance:

```sh
git config wits.forge.git.acme.com.service gitlab
git config wits.forge.git.acme.com.api-url https://git.acme.com/api/v4
```

Only `fetch` and `submit` need a token; the read verbs (`show`, `diff`, `draft`)
and `checkout`/`prune` need just a remote to identify the repo.

## The commands

Seven verbs; only `fetch` and `submit` touch the network.

| Verb | Network | What it does |
|---|---|---|
| `fetch [mr] [--stack MODE] [--feed name]` | read | Pull an MR and its stack (full), a feed and its stacks (light), or every feed (bare). |
| `show [mr] [--details] [filters] [--json]` | — | The inbox, or one MR's merged review view. |
| `diff <mr> [--range SPEC] [--against SPEC] [--patch[=MODE]\|--json]` | — | Describe one range's commits/files/coordinates, or compare two. |
| `draft <mr> [FILE\|-] [--json] [--dedup]` | — | Show the pending draft, append a batch of actions to it, or compact it. |
| `submit [mr] [--stack\|--all]` | write | Flush the draft(s) as batched reviews. |
| `checkout [mr] [--next\|--prev] [--in-place\|--worktree DIR]` | — | Materialize the code to build/test. |
| `prune [mr] [--older-than DAYS\|DATE]` | — | Drop one MR, or sweep terminal/dormant ones. |

### Fetching

```sh
wits review fetch 123                       # MR 123 and its stack — a full pull
wits review fetch https://github.com/o/r/pull/123   # …or by URL
wits review fetch 123 --stack none          # only MR 123, not the rest of its stack
wits review fetch 123 --stack all           # 123 and its whole stack, even from the bottom
wits review fetch --feed mine               # a feed's MRs and their stacks (light)
wits review fetch                           # every configured feed (light)
```

**Fetch completes stacks.** A stack is the review unit, so the tool discovers
the other MRs in a stack by walking each MR's base/source links on the forge — a
*parent* is the MR whose source branch is your base; a *child* is an MR whose
base is your source — out to the whole connected stack. This is why a
label/limit feed can no longer leave a stack half-fetched.

How much to pull is one setting, `--stack MODE`, shared by `fetch <mr>` and
`fetch --feed`:

| Mode | Completes the stack for… | Cost |
|---|---|---|
| `auto` *(default)* | an MR that **sits on another** (its base is a feature branch, so it is not at the bottom) | none for a lone/bottom MR — no probing |
| `all` | **any** MR, even the bottom-most one | one children probe per bottom/lone MR |
| `none` | nothing — just the named MR(s) | — |

`auto` is the sweet spot: it completes a stack you are clearly inside without
spending a forge call on unrelated MRs. Its one blind spot is a stack whose
**only** fetched member is its bottom (a `base`-is-trunk MR with no fetched
child): locally that is indistinguishable from a lone MR, so `auto` leaves it,
and you use `all` when you want the guarantee. A feed can set its own default in
`review.toml` (`stack = "all"`), which `--stack` on the command line overrides.

Two depths, by intent:

- **`fetch <mr>`** is a **full** pull — objects, discussion, derived commit/file
  lists — for the MR and every completed member of its stack.
- **A feed** is **light**: the matched MRs and any completed stack members get
  only their inbox metadata (`info.json`), leaving the per-MR object pull to a
  later `fetch <mr>`. So a feed still scales to a repo with thousands of open MRs.

The walk is **bounded to the real stack** — it climbs to a trunk branch and
stops (and never even probes one), and only ever asks for the children *of a
source branch* — so completing a stack never drags in unrelated MRs. Progress is
logged as members come in, so a multi-MR stack is not a silent wait.

A bare `wits review fetch` (every feed) does one more thing: after the feeds, it
re-checks any **still-open MR already in the store that no feed reported** this
run. A feed only lists live work, so an MR that merged or closed since the last
fetch just drops out of every feed and would otherwise sit `open` in your inbox
forever; this second pass catches that transition. It is bounded to the
non-terminal known MRs (a merged/closed one is already final) and light (one
metadata call each), and `prune` keeps that set from growing without bound.

### Feeds — an RSS-style subscription

A feed is a named, server-side filter. Feeds live in one global TOML file,
`$XDG_CONFIG_HOME/wits/review.toml` (or `$WITS_REVIEW_CONFIG`), with a section
per repo keyed by its `host/owner/repo` identity:

```toml
[repo."github.com/mesa/mesa"]
feed.mine   = { reviewer = "@me", state = "open+draft" }
feed.vulkan = { labels = ["vulkan"], exclude-labels = ["wip"], limit = 40 }
```

Every key is optional; see the [configuration reference](#configuration-reference)
for the full table. Filters are pushed down to the forge and paginated
server-side — never "fetch everything then filter". A repo with no section simply
has no feeds; a token alone still lets you `fetch <number>` any single MR.

### Reading

```sh
wits review show                 # the inbox: every fetched MR, newest first
wits review show 123             # one MR: metadata, snapshot, commits, files, threads
wits review show 123 --details   # …with the metadata in full, for a human
wits review show --details       # …the inbox, with a detail line per row
wits review show 123 --json      # …as the stable editor payload
```

`--details` is the human counterpart to `--json`: it answers *what am I actually
looking at, and how did it get here* — branches, labels, when the MR last moved
and when we last synced it, the snapshot's fork/head/base, the whole snapshot
history with its rebases marked, the MR's place in its stack, and the discussion
counted by category. Metadata only; the diff is `wits review diff` and the
threads follow underneath as usual.

```
#123 [open] Fix the lock ordering
  author     alice
  branches   fix-locks -> main
  labels     vulkan, needs-rebase
  url        https://github.com/me/proj/pull/123
  updated    2026-08-18T09:00:00Z
  fetched    2 hours ago
  stack      121 -> 122 -> *123* -> 124   (3 of 4)

  snapshot   the current of 3 fetched
    fork     3e585a562f9
    head     be3e64fbd01
    change   4 commit(s), 7 file(s)

  history
    1   a1a375c8fd6  fork cb2dd430805
    2   5f6a7b8c9d0  fork cb2dd430805
    3   be3e64fbd01  fork 3e585a562f9   (rebased, current)

  threads    12 total · 3 unresolved · 2 outdated · 1 awaiting you · 0 pending
```

The counts are over *all* the MR's threads, before any filter below narrows the
list — so a `--file`-scoped view still tells you how much you are not looking at.

The detail view folds your pending draft into the remote threads: your new
comments appear as local threads, your replies attach to their threads, your
resolutions flip the flag. For a large MR, **filter** instead of paginate:

| Filter | Keeps threads that… |
|---|---|
| `--outdated` | are anchored to a line no longer in the current diff |
| `--resolved` | are resolved |
| `--unresolved` | are not yet resolved |
| `--unread` | have someone else's comment last (likely awaiting your reply) |
| `--file PATH` | are anchored in `PATH` |

Diff coordinates (the tool does not render diffs — your editor and `git` do):

```sh
wits review diff 123                 # commits + changed files of the current review point
wits review diff 123 --range a1b2..c3d4
wits review diff 123 --patch         # the textual patch (terminal/debug)
wits review diff 123 --json          # coordinates for an editor
```

**The unit of input is a range, not a commit.** One range answers *what does this
change?*; two answer *what changed between these two versions of it?* That is the
whole shape of the command: `--range` names one, `--against` names a second, and
both take the same grammar.

### The range grammar

A range spec has exactly two forms. There are no keywords and nothing that
reinterprets something which looks like a git revision:

| Spec | Means |
|---|---|
| *(flag omitted)* | the current review point's `fork..head` |
| `<review-point-head>` | that review point's `fork..head`, using the fork **pinned at fetch**. A prefix is fine. |
| `A..B` | `merge-base(A,B)..B` |

Anything else — a bare branch name, a tag, `A...B`, a half-written `..B` — is
refused with a message pointing at the two forms. A bare revision would force the
tool to *guess* a base for it, and that guess is exactly the kind of hidden
assumption this grammar exists to remove.

The review-point form is the one shorthand, and it is only a shorthand:
`<base>..<head>` resolves to the same place. `wits review show 123 --details`
lists the head SHAs.

**Why `A..B` always computes the merge base.** In the overwhelmingly common case
where `A` is already an ancestor of `B`, `merge-base(A,B)` *is* `A`, so nothing
changes. The two only differ when `A` has diverged, and there a two-endpoint
`git diff A..B` is precisely the misleading answer — it replays `A`'s side as
inverted hunks. `git log` agrees either way: `A..B` excludes `A`'s divergent
commits, which are not reachable from `B` anyway, so it lists the same commits as
`fork..B`. Commits and diff therefore describe the same series, which is why a
separate `A...B` spelling would buy nothing.

### The fork point — why the diff is not `base..head`

A review diff is taken from where the MR **forked** from its target, never from
the target's current tip. The distinction is not academic, because the two forges
mean different things by "base":

| Forge | What its `base` SHA is |
|---|---|
| GitLab | `diff_refs.base_sha` — already the merge base. |
| GitHub | `baseRefOid` — the target branch's **current tip**. |

Diffing against a tip that has moved since the fork replays the target's own
progress as inverted hunks: unrelated files appear in your review, and the more
active the target branch, the worse it gets. So `fetch` computes
`merge-base(target, head)` once and stores it as the review point's `fork_sha`.
Every local diff — `diff`, `--patch`, the stored commit and file lists, `show` —
runs `fork..head`, uniformly on both forges.

**The forge's own base is not kept.** A review point is `{fork, start, head}`,
and that is enough for everything: the fork is the diff endpoint *and* what
GitLab's comment `position` wants for its `base_sha`, those being the same commit
there; `start` is GitLab's separate diff-version start (a copy of the fork on
GitHub); `head` is the tip. Storing the forge's base as well invited one specific
bug — a value that is not an ancestor of `head`, quietly used as a diff endpoint.

That is now an enforced invariant. A fork must be an ancestor of its head, so
`fetch` acquires the target's objects properly — the bare commit first, then the
target branch by name — and **fails** rather than recording a review point whose
fork it could not resolve. Old data that predates the rule is reported by the read
path and fixed by re-fetching.

`show --details` prints both, labelling `base` as what it is.

### Snapshots vs. ranges

These are two different things, kept apart on purpose:

- A **snapshot** (review point) is something you fetched: the forge's
  base/start/head SHAs plus the fork point derived from them, pinned so the
  objects survive a later force-push. Every `fetch` that sees a new head records
  one, so `info.json` accumulates a history. `show --details` and `show --json`
  list them.
- A **range** is a throwaway diff query — never stored. A review point's head SHA
  is *accepted as* a range spec, but the range itself is not remembered.

To review the code as of an older review point (browse "outdated context"):

```sh
wits review show 123 --details         # the history, oldest first, rebases marked
wits review diff 123 --range 1a2b3c    # that review point's fork..head (prefix ok)
```

Because every snapshot's objects are pinned, this works even after the author
has force-pushed past them.

### What changed since I last looked: `--against`

In a stack-based workflow the author force-pushes constantly, and the question
that matters is not "what does this MR do" but "what did they change since my
last read". A plain diff between the two heads cannot answer it: if they rebased,
it is mostly the target branch's own commits.

`--against` names a second range. Each side is then measured from **its own**
base, which is what cancels the base's movement out from between them:

```sh
wits review diff 123 --against 1a2b3c                  # an earlier review point vs. the current
wits review diff 123 --against 1a2b3c --range 9f8e7d6   # between any two review points
wits review diff 123 --against 1a2b3c --patch           # …as text
```

The usual loop, and the one precondition — both ends have to exist locally,
which for a review point means some `fetch` saw that head:

```sh
wits review fetch 123                  # records review point 1; you review it
wits review show 123 --details         # copy its head SHA from the history
#   … the author force-pushes …
wits review fetch 123                  # records review point 2
wits review diff 123 --against <that SHA> --patch    # what they changed in between
```

There is deliberately no shorthand for "the previous one" — `--against` takes the
same grammar as `--range`, so a bare flag would be ambiguous rather than
convenient, and a `prev` keyword would collide with a branch of that name. You
copy the SHA from `show --details`, which is why that view prints the history.

If you only ever fetched *after* the force-push, there is nothing to compare
against. This is also why `fetch` pins every snapshot's objects — the older side
of the comparison has to survive the author's force-push.

**Neither end has to be a review point.** Comparing modulo the base is a general
operation on two ranges, so two hand-written ranges work just as well — and need
no review history at all:

```sh
# the cherry-pick case: is the cherry-pick faithful to the original?
wits review diff 123 --range 'pick^..pick' --against 'orig^..orig' --patch
```

Without `--patch` you get coordinates: both ends (with their fork points), a
`rebased` flag when the fork moved, the files that really differ, and the commits
**paired across the rebase** — `unchanged` when the patch carried through
untouched, `reworked` when it is the same commit amended, `added` and `dropped`
for the ones that only exist on one side.

```
7 a1a375c8fd6..be3e64fbd01
  from  a1a375c8fd6  fork cb2dd430805
  to    be3e64fbd01  fork 3e585a562f9  (rebased)
  commits:
    reworked   c25d445     168d92a     MR: edit the shared header
    unchanged  a1a375c     be3e64f     MR: add a file
  M src/shared.h
```

The file list is the same comparison as the patch, so the two can never disagree.

### How the base is excluded

The reduction is Nicolai Hähnle's
[diff-modulo-base](https://github.com/nhaehnle/vctools), reimplemented in-tree —
no third-party binary. The idea:

> A hunk of the target diff (`A's head .. B's head`) is worth showing only if
> **one of the two series is responsible for something in it**. Otherwise both
> heads hold unmodified base content there, and any difference can only be the
> base having moved.

That is two tests, cheap and in that order. First a range test: the coordinates
already line up, so it is an interval intersection over hunk headers — no
similarity scoring, no diff-of-diffs.

```
target hunk's old side  == A's head lines  == A's own diff, new side
target hunk's new side  == B's head lines  == B's own diff, new side
```

Then, for hunks that survive, an attribution test on the changed lines
themselves. A region can sit squarely inside a series' edit and still contain
nothing but base movement: both review points contribute the *same* line there,
and the heads differ only because the base changed a neighbour. So an added line
counts as the series' doing when B added it (or A had removed it), a removed
line symmetrically, and a hunk with no such line goes. Coincidental text matches
make this err towards keeping — showing a little too much is a nuisance, hiding
a real change is a bug.

On a real MR with a busy trunk, the two tests took a 24,767-line target diff
across 353 files down to **1,444 lines across 2 files**. The range test alone got
to 3 files; the attribution test removed the third, whose only difference was a
line the base had added beside the author's unchanged one.

Two properties matter in practice:

- **It always terminates and always produces a valid patch,** bounded by the
  target diff. It has no notion of a conflict, so a base change that overlaps
  the series is simply *kept* — that region is inside the series' footprint, and
  it is the conflict-resolution zone worth reading anyway.
- **Its one blind spot is honest.** If the two heads have identical content there
  is no target diff to reduce, so a rebase that silently reverted one of the
  base's changes produces no output. That is checked against the commit pairing
  and reported rather than left to read as "nothing changed"; `--patch=3way`,
  which works from each series' own diff, shows it.

### Printing it: `--patch[=MODE]`

`--patch` switches from coordinates to text. Two modes, and they are about *how
many things are shown*:

| Mode | One range | Two ranges |
|---|---|---|
| `2way` *(default)* | the range's patch | one diff, older to newer, base excluded as above |
| `3way` | *(an error — needs a second range)* | the three diffs the comparison is made of, labelled |

Bare `--patch` is `2way`. The mode must be attached with `=` (`--patch=3way`), so
that `wits review diff --patch 123` can't read `123` as a mode.

Empty `2way` output means nothing changed — with one nuance `wits` reports when
it happens: if the two ranges produce identical content but their commits differ
against their bases, either the series was restructured with the same result, or
a rebase undid something the base did. Drop `--patch` for the pairing, or use
`3way`.

#### Reading a `3way` comparison

`3way` is the raw material rather than the conclusion: what A does to its base,
what B does to its base, and what the base did in between. Reach for it to see a
base change in full, or to read the case above that `2way` cannot show.

The base section is **restricted to the files the two series touch**. Unrestricted
it is the trunk's entire progress — 23,000 lines on the MR quoted above, none of
it about the code under review, which buries rather than shows. Within those
files it is complete. An empty base section means the fork point never moved, so
there was no rebase.

```
=== A     e961d64e6ff  (its own change, from fork 82db8d826fa) ===
…
=== B     97be61afe8c  (its own change, from fork 5308d01074a) ===
…
=== base  82db8d826fa..5308d01074a  (what the base did in between) ===
…
```

## Authoring a review — edit `local.json`

There are **no authoring commands**. You produce the content; the tool writes it
into `local.json`. Two equivalent ways:

- **Pipe a batch to the tool** (the path an editor extension uses):
  ```sh
  wits review draft 123 -   # read a JSON batch of actions from stdin; a file path also works
  ```
  This appends the batch's actions to the draft (setting the verdict if the batch
  carries one), fills in missing action ids, and validates as it writes.
- **Edit `local.json` directly** (the plain-text path for a human). It is the
  same file; both are equivalent.

To edit a queued action, append another action with the same `id`. To remove one,
append a `drop` action naming that id. `wits review draft <mr> --dedup` writes the
compacted form back before submit; `submit` applies the same compaction
automatically. Its full schema is in
[json.md](review/json.md#localjson---the-write-contract); the shape:

```json
{
  "schema": 1,
  "verdict": "request-changes",
  "actions": [
    { "action": "summary", "id": "wits:550e8400-e29b-41d4-a716-446655440000", "body": "A few blockers below." },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440001", "file": "src/x.c", "line": 42, "body": "Off-by-one.", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440002", "file": "src/x.c", "line": 40, "start_line": 38, "side": "old", "start_side": "old", "body": "Was this intended?", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440003", "file": "src/x.c", "body": "This file wants a header.", "commit": "a1b2c3d4" },
    { "action": "comment", "id": "wits:550e8400-e29b-41d4-a716-446655440004", "body": "Overall close." },
    { "action": "reply", "id": "wits:550e8400-e29b-41d4-a716-446655440005", "thread": "9987", "body": "Done." },
    { "action": "resolve", "id": "wits:550e8400-e29b-41d4-a716-446655440006", "thread": "9987", "resolved": true },
    { "action": "drop", "id": "wits:550e8400-e29b-41d4-a716-446655440004" }
  ]
}
```

Rules, all inferred so the file is pleasant to hand-write:

- **`verdict`** (optional): `approve`, `request-changes`, or `comment`.
- **`id`** on every action once stored: the action's logical identity. If a
  client omits it while piping a batch to `draft <mr> -`, the tool generates a
  `wits:<uuid>` id before appending. Reuse an id to overwrite that action in the
  append-only stream.
- **`summary`** action: the review's overall body. If multiple summary actions
  survive compaction, the last one is submitted as the review summary.
- **`drop`** action: local-only removal of the live action with that id.
- **`commit`** on a comment (optional): the snapshot head SHA the comment's line
  anchors were written against. `draft <mr> -` stamps it automatically at ingest;
  a hand-editor may set it. `submit` resolves it to the snapshot's full version
  (`{base, start, head}`) and anchors the comment to it — the forge may mark it
  outdated if the head has moved. Different comments in one draft can target
  different snapshots (cross-snapshot drafting — fully per-comment on GitLab; on
  GitHub the whole review anchors to one review-level commit). When unset,
  `submit` falls back to the current snapshot's head.
- **A `comment` action's placement** is inferred: `file`+`line` → a line comment;
  `file` alone → a file-level comment; neither → an MR-level conversation comment.
  `side` (`new`/`old`, default `new`) and `start_line` (a multi-line start) are
  optional; `start_side` (defaults to `side`) marks a span that starts on one side
  and ends on the other (e.g. a deleted line through to an added one).
- **`reply`** targets a thread id (the bare forge id, or the `remote:` form
  `show` prints).
- **`resolve`** sets a thread's resolved state (supported on both forges).

Preview what's recorded any time, without touching the forge:

```sh
wits review draft 123           # human
wits review draft 123 --json    # machine (echoes local.json)
wits review draft 123 --dedup   # compact append-only edits in local.json
```

### Referencing another line or file

A comment body may reference another location with a `[[…]]` token, which
`submit` expands into a forge permalink (so it renders as a link, while your
local body stays plain and portable):

| Token | Refers to |
|---|---|
| `[[src/y.c:20]]` | line 20 of `src/y.c` (path is repo-relative) |
| `[[src/y.c:20-30]]` | lines 20–30 |
| `[[src/y.c]]` | the whole file, no line |
| `[[src/y.c:20@main]]` | line 20 as of another commit/branch/tag (`@ref`) |

The reference resolves against the **reviewed snapshot's head** by default; the
optional `@ref` pins any other commit, branch, or tag. Example:

```json
{ "action": "comment", "file": "src/x.c", "line": 42,
  "body": "Same bug as [[src/y.c:20]] — factor them together." }
```

## Submitting

```sh
wits review submit 123          # one MR
wits review submit 123 --stack  # every drafted MR in 123's stack
wits review submit --all        # every MR that has a pending draft
```

On submit, the draft is compacted by action id (`drop` removes local actions;
later actions with the same id replace earlier ones), then handed to the forge as
one review. Each platform folds as much as its native batch allows into **one
notification**:

- **GitLab** — comments (line/file/conversation), replies, and the summary (as a
  position-less draft note) all ride a single bodyless `bulk_publish`. The verdict
  is a separate released call — `approve`→`POST …/approve`,
  `request-changes`→`POST …/unapprove` (there is no released API for the formal
  `requested_changes` state; unapprove is its effect), `comment`→nothing — and a
  bare thread resolve is a separate (quiet) PUT.
- **GitHub** — the verdict, summary, line/file comments, **and replies** are one
  review (replies join the pending review by id, exactly as the web UI batches
  them), so they share one notification. Only a conversation (MR-level) comment
  is a separate notification — that one *is* a GitHub API limit. Resolves are
  separate calls but don't notify.

`submit` reports how many notifications it actually produced, so there are no
surprises. Reconciliation is **per action**: whatever lands is cleared from
`local.json`, whatever fails stays for a retry, and only a fully-flushed draft
triggers a re-fetch. Preview exactly what would be posted with `-n`:

```sh
wits review submit 123 -n
```

## Reviewing the code itself: `checkout`

To build, run, or fuzz an MR, materialize its code:

```sh
wits review checkout 123               # into a worktree (leaves your tree alone)
wits review checkout 123 --worktree /tmp/mr123
wits review checkout 123 --in-place    # in the current tree (moves HEAD)
wits review checkout --next            # the MR one step up the stack
wits review checkout --prev            # one step down
```

The default is **one** worktree beside the repository's own checkouts and named for
the review (`../<repo>.review`, or a plain `review` under a bare-style repo's
checkout directory — [`wits worktree`](worktree.md#creating) settles the shape),
reused for every MR: `checkout` and `--next`/`--prev` just switch its `HEAD` to the
target snapshot. So a whole stack costs one worktree and one submodule tree, and pruning
a merged member never disturbs the review of its still-open siblings. Switching it
**refuses a dirty worktree**, since moving `HEAD` would bury your work; untracked
build output is left alone.

The snapshot has a detached `HEAD` by design. A registered project can build it
without creating a local branch:

```sh
cd ../<repo>.review
wits build --detach
```

`build` otherwise continues to require an attached branch. Detached builds do
not expose `branch.raw` / `branch.slug`; a project whose output templates need
those variables must use checkout-keyed templates or explicit `--build-dir` /
`--install-dir` overrides.

`--in-place` checks the snapshot out in your current tree instead; because that
also moves `HEAD`, it refuses a dirty tree too. `--next`/`--prev` walk the stack
from the last checkout — the shape is reconstructed from the fetched MRs'
base/source branches, so it works for anyone's stack.

The worktree mechanics themselves — where it goes, sparse-checkout inheritance,
and the submodule object borrowing behind `--submodules` — are shared with
[`wits worktree`](worktree.md), which is the standalone command for worktrees that
have nothing to do with a review.

## Housekeeping: `prune`

```sh
wits review prune                    # merged/closed MRs
wits review prune --older-than 30    # …and any not refreshed in 30 days
wits review prune --older-than 2026-06-01   # …or last refreshed before a date
wits review prune 123                # just MR 123, whatever its state
```

`prune` drops the store directory and snapshot pins (`refs/wits/review/*`) of
terminal MRs — and their **review worktree** if it sits at the default sibling
path — letting git collect the objects. `--older-than` also catches dormant
MRs, given a **day count** or an **ISO-8601 date**. It is idempotent and a no-op
when nothing is stale.

Naming an MR prunes **just that one, whatever its state** — the way to reclaim a
review worktree and store for an MR you're finished reviewing before it merges.
(A `--worktree <custom>` checkout isn't tracked, so only the default worktree
path is removed automatically.)

## Outdating

A review is pinned to the snapshot you fetched. Comments submit against it, so
when the branch has moved they are shown as *outdated* rather than re-based onto
code they were never about. **`wits review` computes outdating itself**, locally
and identically for every forge: a thread is outdated when its anchored line
falls inside a region the file changed between the commit the comment was written
on and the current head — read from the objects `fetch` pinned, no network, no
reliance on a forge flag. `show --outdated` surfaces exactly those threads. The
reviewed objects are held alive by `refs/wits/review/*` even after the author
force-pushes, so an outdated comment can still be submitted.

## Configuration reference

Three surfaces: **environment variables** (locations and tokens),
**git config** (forge identity and tokens, shared with `stack`), and the feed
**`review.toml`**. Every key is listed below with what it does.

### Environment variables

- **`WITS_REVIEW_DIR`** — Absolute path to the store root, overriding the default
  location. Point it at synced storage to share your drafts across machines. See
  *Store location* below for how it fits the ladder.
- **`WITS_REVIEW_CONFIG`** — Absolute path to the feed config file, overriding the
  default `review.toml` location. Handy to keep one config outside `$HOME`.
- **`XDG_STATE_HOME`** — When set (and `WITS_REVIEW_DIR` isn't), the store lives at
  `$XDG_STATE_HOME/wits/review`. This is *state*, not config.
- **`XDG_CONFIG_HOME`** — When set (and `WITS_REVIEW_CONFIG` isn't), the feed file
  is `$XDG_CONFIG_HOME/wits/review.toml`. This is *config*, not state.
- **`GITHUB_TOKEN` / `GITLAB_TOKEN`** — The forge API token, used by `fetch`/
  `submit` when no git-config token key matches. The one that applies is chosen
  by the detected service.
- **`HOME`** — Falls back to `$HOME/.config/wits/review.toml` for the feed file
  when neither override nor `XDG_CONFIG_HOME` is set.

### Git config (under `wits.forge.*`, shared with `stack`)

Token resolution tries these in order, most specific first, then the env var:

- **`wits.forge.<host>.token`** — Token for one host (e.g.
  `github.com`). The most precise, and what you usually set.
- **`wits.forge.<service>.token`** — Token for a whole service
  (`<service>` ∈ `github`, `gitlab`), when several hosts share a service.
- **`wits.forge.token`** — A blanket token, the last config fallback
  before the environment.
- **`wits.forge.<host>.service`** — Declares a self-hosted host's type
  (`github` / `gitlab`) when the hostname doesn't reveal it.
- **`wits.forge.<host>.api-url`** — The API base for a self-hosted or
  enterprise instance (e.g. `https://git.acme.com/api/v4`).

### Feeds — `review.toml`

The file is a single global TOML, found at `$WITS_REVIEW_CONFIG`, else
`$XDG_CONFIG_HOME/wits/review.toml`, else `$HOME/.config/wits/review.toml`. Each
repo is a table `[repo."<host>/<owner>/<repo>"]`; inside it, each feed is an
inline table `feed.<name> = { … }`. The feed keys, each optional:

- **`state`** *(string, default `"open+draft"`)* — Which lifecycle states to
  pull: `"open+draft"`, `"open"`, or `"draft"`. Merged and closed MRs are never
  fetched — a review inbox is about live work.
- **`labels`** *(list, default `[]`)* — Only MRs carrying **all** of these labels.
  Multiple labels are AND-ed on both GitHub and GitLab (the platforms' own
  behaviour for a single list query); use separate feeds when you want either-or.
- **`exclude-labels`** *(list, default `[]`)* — Drop MRs carrying **any** of these
  labels — the way to filter out `wip`/bot noise.
- **`author`** *(string)* — Only MRs opened by this user. `@me` resolves to the
  authenticated user.
- **`assignee`** *(string)* — Only MRs assigned to this user. `@me` is you.
- **`reviewer`** *(string)* — Only MRs with this user requested as a reviewer.
  `@me` is you — this is the "assigned to me to review" feed.
- **`search`** *(string)* — A raw platform search string, passed straight through
  for the full-text case the faceted keys can't express.
- **`limit`** *(integer, default `30`)* — A cap on how many MRs the feed pulls,
  most-recently-updated first, so a large repo can't flood the inbox.
- **`stack`** *(string, default `"auto"`)* — How much of each matched MR's stack
  to complete: `"auto"`, `"all"`, or `"none"` (see [Fetching](#fetching)). This
  is the feed's own default; a `--stack` on the command line overrides it.

Different keys are combined with **AND**: a feed pulls only MRs matching all of
them. `@me` works in `author`/`assignee`/`reviewer`.

### Store location (state, distinct from config)

The store root is resolved on this ladder, first hit wins:

- **`$WITS_REVIEW_DIR`** — an explicit override, when set.
- **`$XDG_STATE_HOME/wits/review`** — when `XDG_STATE_HOME` is set.
- **`<common-git-dir>/wits/review`** — the default, per-clone (beside the machete
  file, and in the common dir for the same reason: one store per repository, shared
  by every worktree).

Per-run choices (`--range`, `--against`, `--patch`, `--details`, `--stack`,
`--all`, `-n`) are flags, not config — they describe one invocation.

## Version scope and limitations

Bounded on purpose, and honest about it:

| Area | behaviour |
|---|---|
| Forges | GitHub (GraphQL) and GitLab (REST). Gitea/Forgejo/Codeberg have the trait seam but no review backend. |
| Diff base | Always the **fork point**, `merge-base(base, head)`, computed locally at fetch and stored on the snapshot — so a moving target branch never leaks into a review diff, and both forges behave identically. The forge's own base/start/head ride untouched into `submit`. |
| Range grammar | Two forms only: a fetched review point's head SHA, or `A..B` (always `merge-base(A,B)..B`). A bare revision is refused rather than given a guessed base, and there is no `prev` keyword. |
| Comparison | `diff --against` takes **any two ranges** — neither has to be a review point. The base is excluded by the diff-modulo-base reduction (reimplemented in-tree): a target hunk survives only if one of the two series touched that region. Commits are paired with `git range-diff`, biased toward pairing so a small amend reads as *reworked* rather than drop + add. Both ends must be present locally; the snapshot pins keep an older review point alive. |
| Reduction blind spot | Two heads with identical content leave no target diff to reduce, so a rebase that reverted a base change shows nothing. Detected against the commit pairing and reported; `--patch=3way` shows it. |
| External tools | None. Everything is git plumbing (`diff`, `range-diff`, `log`) plus the reduction, so there is nothing to install. |
| Fork-point integrity | A fork must be an ancestor of its own head. A fetch that ran before the base's objects arrived can record one that isn't, which widens every later comparison; `fetch` re-derives the whole history and `diff` re-checks at the point of use, so a stale record heals itself rather than quietly costing you accuracy. |
| Thread resolve | Supported on **both** — GitHub via `resolveReviewThread`, GitLab via the discussion API. |
| Verdicts on GitLab | Mapped onto the *released* API: `approve`→`POST …/approve`, `request-changes`→`POST …/unapprove` (no released API sets the formal `requested_changes` reviewer state; unapprove is its concrete effect), `comment`→no-op. The `bulk_publish` `reviewer_state`/`note` body that would fold the verdict + summary into the publish is the unmerged gitlab-org/gitlab!237813 — absent from every release — so the summary rides as a draft note instead. |
| Editing/deleting a **published** comment | Not supported; you edit only your pending `local.json`. |
| Cross-snapshot anchoring | Per-comment on GitLab (each comment anchors to its own snapshot version); review-level on GitHub (its API takes one commit per review, so the batch anchors to one snapshot). Comments without a `commit` use the current snapshot. |
| Outdating | Computed **locally** and identically for both forges — a thread is outdated when its anchored line changed between the commit it was written on and the current head. Falls back to the forge's own flag only when that commit's objects aren't local. |
| Feeds | Return real MRs (base/head) up to a hard `limit`, most-recently-updated first, then **complete each match's stack** (light) by walking base/source links; a bare `fetch` also re-checks still-open known MRs no feed reported, so a merge/close is reflected even after the MR drops out of the feed. |
| Notifications | Minimised, not promised: `submit` reports the true count. GitLab folds comments + replies + summary into one `bulk_publish` (the verdict is a separate quiet `approve`/`unapprove`). GitHub folds the verdict, summary, line/file comments, and replies into one review; only an MR-level conversation comment is a separate notification (resolves are separate but quiet). |

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `no 'origin' or 'upstream' remote…` | `review` keys off the target remote; add one. |
| `no API token for …` | Set `wits.forge.<host>.token` or `*_TOKEN` (fetch/submit only). |
| `MR N isn't in the store yet` | Run `wits review fetch N` first — read verbs use the local files. |
| `not a range and not a fetched review point's head` | `--range`/`--against` take `A..B` or a review-point head SHA; a bare branch name isn't a range. `show --details` lists the heads. |
| `is git's three-dot form` | Write `A..B`; the merge base is computed either way, so the two would mean the same thing. |
| `--patch=3way … needs a second range` | `3way` shows two versions against their base; add `--against <SPEC>`. |
| `a value is required for '--against'` | It shares `--range`'s grammar, so a bare flag would be ambiguous; name the range or SHA. |
| `no feed 'x'` | The repo has no `feed.x` under its `[repo."…"]` section in `review.toml`. |
| `no feeds configured for …` | Bare `fetch` needs at least one feed; add one, or name an MR. |
| `working tree has uncommitted changes` (checkout) | In-place checkout moves HEAD; commit/stash first, or use a worktree. |
| `some actions did not submit` | A per-action failure; the failed ones stayed in `local.json` — fix and re-`submit`. |

## Invocation forms

Like every `wits` tool, `review` has a direct form via symlink — `wits-review` —
created by `meson install` (see the top-level [README](../README.md)).
