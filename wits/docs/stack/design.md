# `wits stack` — Design

> Status: **implemented**. This records the agreed shape of the `stack` tool and
> the *why* behind it; the code lives in `src/cmd/stack/` and
> `src/util/{forge,remote}`. Where a detail has since evolved in code, the code
> is authoritative — the trait sketch in §7.3 has been kept in sync.
>
> This file explains *why the tool is shaped the way it is*. The companion usage
> document (`docs/stack.md`) explains *how to drive it* and carries the full,
> reader-facing reference (every flag, every config key). Neither restates the
> other; when in doubt, behaviour-for-users goes there, rationale goes here.

---

## 1. What the tool is, and what it deliberately is not

A stacked-diff workflow means cutting one line of work into a chain of small
branches, each built on the previous one, and turning each into its own merge
request so reviewers see a sequence of digestible changes instead of one wall of
diff. The hard part was never the local commit surgery — `git rebase`,
`git-branchless`, `git-machete` already do that well. The hard part is the
*remote* bookkeeping: pushing the right branches, opening each MR against the
right base, keeping those bases correct as the stack is reordered, and keeping
every MR's description pointing at its neighbours so a reviewer can navigate.
That remote bookkeeping is the entire job of this tool.

(On naming: the user-facing label is per-host — GitHub calls it a PR, GitLab an
MR. Internally, and throughout this document, we call it an **MR**. The label is
just a presentation detail a forge supplies, §7.)

The division of labour:

- **Local topology is given to us, not computed by us.** `.git/machete` records
  which branch sits on which (a forest, not just a chain). We read it; we never
  reimplement restack/rebase. `slice` (§9) is the one place we *write* it, and
  even there git does the commit movement — we only assign names and record the
  resulting shape.
- **Local refs are the source of truth for content.** If `feature-b` points at a
  commit locally, that is what gets pushed. We assume the user (or
  `git-branchless`) has kept the pointers sane.
- **We own the remote.** Pushing, opening MRs, fixing MR bases, rewriting MR
  descriptions — that is what lives here.

Non-goals, stated once: no rebase/restack engine and no conflict resolution —
those have good tools already. We *do* edit the topology metadata (`slice` writes
it, `tree` edits it), but we never move commits; restacking after a `tree mv` is
the user's job.

## 2. CLI surface

Four verbs, each doing exactly one thing. The split is the point: when a push
succeeds but an MR update fails you want to know *which* step you were in and to
re-run only that step. The three remote verbs map cleanly onto three distinct
intents.

```
wits stack sync    [scope]   # push branches to origin (git only; no forge)
wits stack submit  [scope]   # reconcile MRs: create missing, fix drifted bases
wits stack anno    [scope]   # rewrite MR descriptions with stack navigation
wits stack decorate [branch] # add labels/assignees/reviewers to an MR (additive)
wits stack slice  [--base B] # interactively cut HEAD's commits into a stack
wits stack tree   {prune|rm|mv}  # direct edits to the stack's structure
```

`decorate` is single-MR by default (attributes differ per MR; `--all` applies one
set across the stack) and additive-only, so it never fights a project's own
label/reviewer automation — see [`behavior.md`](behavior.md) §7.

`tree` is a separate group on purpose: `prune`/`rm`/`mv` change *what the stack
is* (structure edits to `.git/machete`), as opposed to the four verbs that *act
on* it. Their behaviour — and the splice-up rule that keeps a removal from
destroying the line above it — is specified in [`behavior.md`](behavior.md) §9.

- **`sync` — push.** Force-with-lease push of every in-scope branch to `origin`
  (§6), and nothing else. It touches git only and never the forge; that is what
  keeps it cleanly orthogonal to `submit`.
- **`submit` — reconcile MRs.** Everything MR-shaped lives here: for an in-scope
  branch with no open MR, create one against the correct base; for one whose MR
  base drifted as the stack was reordered, correct it. It does **not** push —
  that is `sync`'s job, and keeping the two orthogonal is the whole point. A
  branch must already exist on `origin` (i.e. you `sync`ed first) for its MR to
  make sense, so `submit` assumes that and says so plainly if the branch is
  missing remotely.
- **`anno` — descriptions only.** Read each in-scope MR, rewrite its generated
  navigation block, write it back. Never pushes, never creates.
- **`slice`** is the local authoring step (§9).

The clean consequence: the three remote verbs are orthogonal facets of remote
state — branch content (`sync`), MR existence and base (`submit`), MR description
(`anno`) — and each is an idempotent reconcile you can re-run on its own.

### Scope: which branches a verb touches

This is the subtle part of the behaviour; the table below is the rationale, while
[`behavior.md`](behavior.md) is the authoritative spec (with worked fork and
dynamic-edit examples). Given the current branch **N**:

| Situation | Branches in scope |
|---|---|
| N is a **fork-point** (≥2 children) | `ancestors(N)` + the **entire subtree** under N — "I'm managing this whole tree." |
| N is **linear** (≤1 child) | the **linear stack**: ancestors + N + the first-child chain down to the next fork/leaf. Sibling branches are someone else's line of work and are left alone. |
| N is **not in `.git/machete`** | N alone, as a one-node stack on the base branch (§5.3). This is what makes single-branch MRs work with zero machete setup. |
| `--all` | every branch in every tree in the file. No filtering. |

The base branch (usually `main`) is never itself pushed or given an MR, but it
*does* appear in annotation chains so reviewers see the full lineage.

`sync`, `submit`, and `anno` must agree on this selection — if the fork-point
threshold ever changes for one, it changes for all. The selection therefore
lives in exactly one place (§5), not copied per verb.

The `[scope]` positional on those three is that single seam surfaced to the CLI:
it is a *scope anchor*, the branch the selection is computed from, defaulting to
the checked-out branch. It replaces `HEAD`, nothing more — so it flows through
the same `plan()` and cannot fork the fork-point rule (it strengthens invariant
1 rather than threatening it), and it lets a stack be driven without a checkout
(worktrees, a dirty tree). It is deliberately *not* like `decorate`'s `[branch]`:
that one names the single MR to touch (per-MR), this one names where a whole
stack is read from (per-stack). It is branch-only, never a commit — the topology
is keyed by branch name and a commit can carry several branches (§9), so a commit
anchor would be ambiguous. Anchor and `--all` are mutually exclusive; a named
anchor must resolve to a real branch (local ref or a file entry) so a typo fails
loudly instead of resolving to an empty synthetic stack.

Global `-v/--verbose` and `-n/--dry-run` come from the `wits` process layer for
free; every mutating git/forge call respects dry-run, every read still runs.

## 3. Code organization (brief)

Not worth a diagram, but the one decision worth recording is the `core` / `util`
line. `core` holds foundational, near-zero-domain primitives — "how do we talk to
the OS, to git, to config" — and stays minimal on principle: `process`, `log`,
`resolver`, and an **expanded `git`** (§6). `util` holds larger, self-contained
subsystems with real logic of their own that a command *composes*: `forge` (the
git-hosting REST layer, §7) and `remote` (URL parsing + origin/upstream role
resolution), which lives there because it is the same git-hosting concern and is
reusable beyond stack. The command's own tree logic (`topology`, `resolution`)
and its verbs live under `cmd/stack/`. `forge` keeps its name — it is the precise
term for a git hosting platform and dodges the worse options (`remote` collides
with git's noun, `platform`/`provider` are vague).

## 4. Topology — the machete forest

The topology layer is pure data and pure functions; it never touches git or the
network, which is what makes the tree rules trivially testable.

The file format stays **git-machete-compatible**: one branch per line,
indentation encodes parentage, an optional trailing annotation per line. We keep
the annotation slot and use it to cache MR identity (e.g. `!123`) so a later run
need not re-discover numbers — but the annotation is a cache, never the source of
truth; the live forge is.

The tree algebra is small and total:

- `ancestors(n)` — root→…→parent, excluding n.
- `subtree(n)` — n and all descendants, DFS pre-order.
- `linear_stack(n)` — ancestors + n + first-child chain.
- `anno_blocks(n)` — the set of navigation chains to render for n's MR (§8).

One invariant a future change will be tempted to break: `anno_blocks` stops a
chain at the next fork-point rather than expanding it, because that nested
fork-point renders its own multi-chain description; expanding it here would grow
descriptions combinatorially.

## 5. Stack resolution

Resolution is the seam between "the file on disk" and "the work to do". It takes
the topology, the current branch, the live local refs, and the resolved base
branch, and produces a single ordered selection of *operable* nodes plus, for
each, the **base it should target** (its parent, or the base branch when its
parent is the root). Everything downstream — sync, submit, anno — consumes this
one structure, which is how the three verbs are guaranteed to agree on scope.

### 5.1 Base branch resolution

In order: the `project` subcommand → the upstream/origin remote's default branch
(its remote HEAD) → first existing of `main`/`master`/`trunk`. Resolved once per
run.

The right source of truth is the future `project` subcommand: given a checkout's
source path it will answer "what project is this, and what is its main branch?".
Until it exists we skip straight to the two mechanical fallbacks above. We
deliberately do **not** add a `wits.stack.base-branch` config key — the answer
should come from project identity, not a hand-maintained per-repo setting, and an
override now would only be a thing to migrate away from later. If nothing
resolves, that is a hard error, not a guess.

### 5.2 MR base mapping

A node's MR base is its machete parent. When the parent *is* the base branch (the
root of the tree), the MR targets the base branch on the **merge-target repo**
(§7.1) — the only place the origin/upstream distinction reaches into resolution.

### 5.3 Branches not in the file → synthetic one-node stack

When the current branch is absent from `.git/machete`, resolution synthesizes a
trivial tree: `base → branch`. `sync` and `submit` then operate on exactly that
branch. `anno` **skips** it: a lone MR has no neighbours to navigate to, so a
navigation block would be pure noise. This single-node path requires zero machete
setup and is the common case for an ordinary one-off MR.

## 6. Git access (`wits_util::git`)

Driven through the `git` CLI, deliberately — the same fidelity argument this
codebase already makes for config reads. A user's real git behaviour is the sum
of their includes, conditional includes, credential helpers, and SSH setup;
libgit2 reimplements a drifting subset of exactly that. Spawning the same `git`
the shell would means we behave identically, with no second implementation to
keep honest. A process spawn per call is noise next to a network round-trip. We
do not introduce libgit2.

The surface grows to what stack needs and no further:

- read: current branch, branch→tip ref map, `log` over a `base..branch` range
  (for MR title/body), the URL of a named remote, the `.git` dir.
- write: `push --force-with-lease` to a remote, `fetch`.

Force-*with-lease* rather than plain force: a stack is rewritten constantly, so
non-fast-forward pushes are normal, but `--force-with-lease` still refuses to
clobber a remote that someone else advanced — the one safety we actually want.

## 7. Remotes, roles, and forges (`wits_util::forge`)

### 7.1 Two roles, made explicit

Two remotes carry distinct meaning and we make both first-class:

- **`origin`** — where we have push rights and where branches go. Also the *head*
  side of an MR.
- **`upstream`** — the fork source; the MR's **merge target**. When absent, it
  collapses to `origin` (you are working directly on the repo you'll merge into).

The forge to talk to is determined by the **upstream** URL (that is where the MR
lives). When origin and upstream differ, the MR crosses a fork: GitHub/Gitea
express that with an `origin_owner:branch` head, while GitLab needs its
cross-project dance (create on the source project with a numeric
`target_project_id`; the MR then lives in the target, where reads and edits go).
The forge layer hides this — the verbs never know whether a fork is involved.

### 7.2 URL parsing and detection (`wits_util::forge::remote`)

Parse the messy reality of remote URLs into `host / owner / repo` plus a detected
service: scp-syntax (`git@host:owner/repo`), full URIs, SSH alias resolution via
`ssh -G`, and a small domain-normalization table (e.g. `ssh.github.com` →
`github.com`). Detection is host-based with a config override
(`wits.forge.<host>.service`) so a self-hosted GitLab/Gitea behind a
custom domain can be told what it is.

### 7.3 The forge abstraction

The design boundary that keeps this from rotting: a **normalized** MR type, a
**tiny primitive trait** per host, and the verbs composing those primitives. No
provider JSON shape (`number` vs `iid`, `base.ref` vs `target_branch`) is ever
allowed to escape a host module.

```rust
struct MergeRequest {
    id: String,            // opaque handle for later updates (number/iid/id)
    display: String,       // "!123" — presentation only
    state: MrState,        // Open | Merged | Closed
    base: String,          // current merge target
    head_sha: Option<String>,
    body: String,
    web_url: String,
}

trait Forge: Send + Sync {
    fn noun(&self) -> &str;                                    // "PR" | "MR"
    fn find(&self, branch: &str, state: StateFilter) -> Result<Option<MergeRequest>>;
    fn create(&self, req: &NewMr) -> Result<MergeRequest>;
    fn set_base(&self, id: &str, base: &str) -> Result<()>;
    fn set_body(&self, id: &str, body: &str) -> Result<()>;
    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> Result<()>;
}
```

`find` matches on the **branch alone**, not the base: the base a branch *should*
target is the topology's business, so the verb compares `MergeRequest.base`
against the plan itself rather than asking the forge to filter by it (an early
version filtered by base and missed drifted MRs — the regression that motivated
this signature). `apply_attributes` is the additive labels/assignees/reviewers
primitive `decorate` composes.

A host impl (`github`/`gitlab`/`gitea`) is then *only* a mapping: base API URL
from host, auth header style, endpoint paths, and the JSON↔`MergeRequest`
translation. The Gitea impl is GitHub-shaped but kept as its own thing
(composition over a fragile shared base class), and serves the whole Gitea /
Forgejo / Codeberg family — one API surface, three identities.

There is no monolithic "reconcile" — the split verbs compose the primitives, and
that is cleaner than a mode flag:

- **`sync`** uses no forge primitives at all — it is a git push (§6). Mentioned
  here only to make the boundary explicit: nothing MR-shaped happens in `sync`.
- **`submit`** → `find(open)`; if found and its base drifted, `set_base`; if
  none, consult the closed-MR guard below, else `create` with title/body derived
  from the branch's commits (default: the latest commit's subject/body;
  `--title-source first|last`). The MR's draft state is decided here: an MR
  whose base is *not* the stack base starts as **draft** by default (a mid-stack
  change should not be reviewed/merged before what it sits on), overridable
  per-invocation with a `--no-draft` flag — a CLI option, not a config key,
  because it is a per-run intent.
- **`anno`** → `set_body`.

**Closed-MR guard** (submit only): if no open MR exists but a closed/merged one
does, do not silently recreate it — recreate only when its head SHA differs from
our local tip, or when the user passes `--force`. (The branch was likely merged
and is being reused; recreating blindly spams the forge.)

**Cross-fork on GitLab** is handled inside the GitLab module: because it cannot
use the `owner:branch` head trick, it resolves the numeric source/target project
ids once, creates the MR on the source project with `target_project_id`, and does
every read/edit against the target project (where the MR resides). Same-project
stacks skip all of that and pay no extra request.

### 7.4 Transport and credentials

Transport is **direct REST** over `ureq` + `serde_json` — no dependency on an
installed `gh`/`glab`, and identical behaviour on every host. A small private
helper inside `forge` does "method + path + json → json", applying the host's
auth header; it does not become a public `wits_util::http` because nothing else needs
it yet.

Token resolution, most specific first:
`wits.forge.<host>.token` → `wits.forge.<service>.token` →
`wits.forge.token` → env (`GITHUB_TOKEN` / `GITLAB_TOKEN` /
`GITEA_TOKEN` / `FORGEJO_TOKEN` / `CODEBERG_TOKEN`). Environment is the most
deliberate, most ephemeral override, consistent with the rest of the codebase.

Gitea, Forgejo and Codeberg are one API family and share a single impl, but stay
three separate *identities* — each with its own token env and `.service` name —
because they are parallel platforms users think of by name. Their env search
follows the fork lineage rather than jumping to the root: a Forgejo host falls
back to `GITEA_TOKEN`.

## 8. Annotation rendering (`anno`)

For each in-scope MR, `anno` rebuilds a single generated block delimited by a
fixed HEADER/FOOTER comment pair, replacing any previous one (the single-pair
invariant is what makes stripping reliable). Inside, one navigation section per
chain from `anno_blocks` (§4); each line names the MR and its `parent ← child`
flow, with the current MR marked. A fork-point MR therefore shows one section per
downstream branch, so a reviewer sees every path the stack takes from here.

Identity: `anno` discovers MR numbers from the forge, caches them back into the
machete annotations, and reuses them within the run.

## 9. `slice` — authoring a stack

`slice` is the local authoring step. We drive `git rebase -i` with a custom
`GIT_SEQUENCE_EDITOR` that (a) seeds the todo with the range's commits and
commented `update-ref refs/heads/<suggested>` lines, (b) opens the user's real
editor, (c) captures the final todo. Branch pointers are set by `update-ref` at
the end of the rebase — safe for the current branch and for worktrees, unlike
`branch -f`. The captured todo, not a post-rebase `base..HEAD` scan, is the
authoritative list of assigned branches, because git leaves HEAD in a misleading
position when the checked-out branch is itself an intermediate update-ref target.
From that list we (re)write `.git/machete`.

Suggested branch names use a configurable prefix (`wits.stack.prefix`, else a
slug of `user.name`, else `stack/`).

## 10. Concurrency

Network and push latency dominate, and the work is embarrassingly parallel
(branches are independent), so we parallelize from the start with scoped OS
threads (`std::thread::scope`) over a bounded pool — `ureq` is blocking, threads
are the natural fit, and the global verbose/dry-run flags are already atomics.

Two ordering constraints survive parallelism: MR **creation** for siblings is
serialized where a forge races on duplicate detection, while base/body **updates**
fan out freely; and a branch whose push failed is excluded from any later step
that depends on it. The selection (§5) is computed once, single-threaded, before
any fan-out.

## 11. Configuration (pointer, not reference)

Only the shape, kept light here on purpose — the complete, reader-facing table
lives in `docs/stack.md`. Config is read from git config under the `wits.*`
namespace: `wits.stack.prefix` for `slice`, and the shared `wits.forge.*`
per-host service/api-url overrides plus the token chain of §7.4. There is intentionally no
base-branch config key (§5.1). Per-run intents (draft, title source, force) are
**CLI options**, not config — they describe one invocation, not a standing
preference.

## 12. Open questions / future

- **future** `project`-subcommand integration: derive base/main branch (and more)
  from a source path once that tool exists (§5.1).
- **future** CI status read-back into the annotation block.
