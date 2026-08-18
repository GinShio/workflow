# `wits stack` — Behaviour reference

The authoritative description of *how the verbs decide what to do*, including the
awkward cases — forks, multi-round edits, deleted branches. The usage guide
(`docs/stack.md`) is for getting things done; the design doc
(`docs/stack/design.md`) is for why the tool is shaped this way; this is the
precise contract, aimed at whoever changes or debugs the logic.

Throughout, an MR is the merge request (GitHub's "PR", GitLab's "MR"); the noun
shown to users is per-host, but everything here calls it an MR.

---

## 1. The machete forest

`.git/machete` records a dependency **forest**: each line is a branch, and
indentation means "sits on top of". It is a forest, not just a chain — a branch
may fork into several.

```
main
    A            single child: linear
        B        fork-point (children C, D)
            C    leaf
            D    single child: linear
                F  leaf
```

Two properties of the parser matter:

- **Reading is indentation-agnostic.** Only *relative* nesting is read: a line's
  parent is the nearest preceding line with strictly smaller indent. Two spaces,
  four, or a tab all parse identically, and deleting a middle line leaves its
  child correctly attached to the grandparent even if the child is not
  re-indented.
- **Writing normalizes to four spaces.** A rewrite (by `anno` caching numbers, or
  `slice`) emits four spaces per level regardless of the input width.

A trailing annotation per line caches the MR identity (e.g. `PR #123`). It is a
cache only — the live forge is the source of truth — refreshed by `anno`.

### Definitions

| Term | Meaning |
|---|---|
| **fork-point** | a node with ≥ 2 children |
| **linear node** | a node with ≤ 1 child (a leaf or a single-child node) |
| **ancestors(N)** | root → … → parent, excluding N |
| **subtree(N)** | N and all descendants, DFS pre-order |
| **linear stack(N)** | ancestors(N) + N + the first-child chain down to a leaf |

---

## 2. Scope — which branches a verb touches

`sync`, `submit`, and `anno` share **one** scope computation, so they can never
disagree. Given the checked-out branch N:

| Situation | Operable set |
|---|---|
| N is a **fork-point** (≥ 2 children) | ancestors(N) + entire subtree(N) — "I manage this whole tree" |
| N is **linear** (≤ 1 child) | linear stack(N) — this one line of work; sibling forks are left alone |
| N is **not in the file** | just N, as a synthetic one-node stack on the base branch (§3) |
| `--all` | every node in every tree, file order, no filtering |

The base branch is always removed from the operable set (it is never pushed and
never gets its own MR), but it still appears inside `anno` chains so reviewers see
the full lineage. A detached HEAD with no `--all` is an error (there is no branch
to scope from).

**Anchoring on a named branch.** N above is normally the checked-out branch, but
`sync`/`submit`/`anno` accept an optional positional branch that replaces it as
the anchor — the stack is then computed around *that* branch without checking it
out. It is a scope anchor, not a single target: an anchor mid-line still selects
its ancestors and downstream chain (the same set standing on it would). The
anchor and `--all` are mutually exclusive, and an explicitly named anchor must be
a real branch — a live local ref, or a name recorded in the file — so a typo
fails loudly instead of quietly resolving to an empty synthetic stack. (An anchor
that *is* a valid branch but absent from the file still becomes the synthetic
one-node stack of §3, just as the checked-out branch would.) A named anchor also
lifts the detached-HEAD restriction, since scope no longer depends on HEAD.

**Worked examples** on the sample forest above:

- Standing on **B** (fork): operable = `A, B, C, D, F` (ancestors `A` + subtree of
  `B`); `main` dropped as base.
- Standing on **D** (linear): operable = `A, B, D, F` (the linear stack); `C` is
  *not* touched — it is a sibling line.

---

## 3. Base resolution and per-branch base

The **base branch** is resolved once: the merge target's remote HEAD
(`upstream`, else `origin`), then the first of `main`/`master`/`trunk` that
exists. (A future `project` subcommand will supply it from project identity;
there is deliberately no config key.) If nothing resolves, that is a hard error.

A branch's **MR base** is its parent in the forest, or the base branch when the
branch is a root. This is the only place the origin/upstream distinction reaches
resolution: a root branch's MR targets the base branch on the *merge-target*
repo.

A branch absent from the file becomes a synthetic `base → branch` stack:
`sync`/`submit` act on it; `anno` skips it (a lone MR has no neighbours to list).

---

## 4. `sync`

Push every operable branch that exists locally to `origin`, with
`--force-with-lease`, in parallel. A name in the file with no local ref is a
stale entry and is skipped rather than pushed. Any push failure makes the whole
command exit non-zero (after attempting the rest). `sync` never contacts the
forge.

`--force-with-lease` (not a plain force) is the deliberate choice: history
rewriting makes non-fast-forward pushes routine, but the lease still refuses to
overwrite a remote that someone else advanced.

---

## 5. `submit`

Reconcile the MRs to the forest. Two phases: read every branch's MR state in
parallel, then apply — base corrections fan out, creations run serially (some
forges race on duplicate detection when siblings are opened at once). `submit`
never pushes; a branch must already be on `origin`, or the forge refuses to open
its MR (reported per-branch, not fatal).

Per branch, with desired base B = its parent (§3):

1. **Open MR exists** → if its base ≠ B, retarget it to B; otherwise nothing to
   do. *Finding the MR is by branch only, never filtered by base* — that is what
   lets a drifted base be detected at all.
2. **No open MR, a closed/merged one exists** → recreate only if our local tip
   differs from that MR's head commit, or `--force` is given; otherwise leave it
   (a merged branch being reused should not silently spawn a duplicate).
3. **No MR at all** → create it.

**Draft.** A created MR whose base is *not* the base branch starts as a draft
(a mid-stack change should not be reviewed or merged before what it sits on);
the MR that targets the base branch starts ready. `--no-draft` opens everything
ready. (Draft is expressed per host: GitHub a field, GitLab a `Draft:` title
prefix, Gitea a `WIP:` prefix.)

**Title/body** for a new MR come from one of the branch's commits — the latest by
default, `--title-source first` for the oldest. Existing MRs are never re-titled.

---

## 6. `anno`

Rewrite each operable MR's description with a generated navigation block,
preserving the human-written remainder. The block is one delimited region
(`<!-- wits stack: generated navigation … -->` … `<!-- wits stack: end navigation -->`)
containing one or more `### Stack List` sections. Discovered MR numbers are
cached back into the machete annotations.

### Block generation

For node N, let `prefix = ancestors(N) + [N]`, and let
`path_to_next_fork_or_leaf(start)` walk linearly from `start` (following the lone
child each step) stopping inclusively at the first leaf or fork-point.

```
N is a fork-point (≥2 children):  one block per child Ci → prefix + path(Ci)
N has exactly one child:          one block             → prefix + path(child)
N is a leaf:                      one block             → prefix
```

A downstream walk stops at the next fork-point because that fork renders its own
multi-section description; expanding it here would grow descriptions
combinatorially.

### Rendering rules

- Within a section, **only nodes that currently have an open MR are numbered.**
  An MR-less node (the base branch, or a merged middle branch) is not given a
  line, but still appears as the *parent* in its child's flow line, so the chain
  reads correctly. A root branch with no parent shows the base branch as its
  parent rather than a placeholder.
- The current MR's own line is marked `⬅️ **current**`.
- **Idempotent:** regenerating identical content replaces the old block byte for
  byte, so a second `anno` run reports "already up to date" and writes nothing.
  (A forge that rewrites a description's whitespace/line-endings on its side can
  defeat this and cause a harmless re-write each run.)

### Block table for the sample forest

```
main -> A -> B(fork) -> C(fork) -> E
                                -> G
                     -> D -> F
```

| Node | Sections (branch names per section) |
|---|---|
| `A` | `[main, A, B]` — linear into fork B, stops at B |
| `B` | `[main, A, B, C]` · `[main, A, B, D, F]` — one per child |
| `C` | `[main, A, B, C, E]` · `[main, A, B, C, G]` |
| `D` | `[main, A, B, D, F]` |
| `E` / `F` / `G` | their own lineage to the leaf |

(`main` is the base: not annotated, shown only as a parent.)

### Rendered output (B's description, B current)

```markdown
<!-- wits stack: generated navigation, do not edit below -->

### Stack List

  * [1/3] PR #10
    `main` ← `A`
  * [2/3] PR #11  ⬅️ **current**
    `A` ← `B`
  * [3/3] PR #12
    `B` ← `C`

### Stack List

  * [1/4] PR #10
    `main` ← `A`
  * [2/4] PR #11  ⬅️ **current**
    `A` ← `B`
  * [3/4] PR #13
    `B` ← `D`
  * [4/4] PR #14
    `D` ← `F`

<!-- wits stack: end navigation -->
```

---

## 7. `decorate`

Add labels, assignees, and reviewers to MRs — additively, and per MR.

Attributes differ from one MR to the next, so this verb is **single-MR by
default**: it acts on the named branch (or the current one). `--all` applies the
*same* set across the whole in-scope stack, for a uniform label like `stacked`.
Per-branch differences are expressed by running it once per branch with that
branch's flags — typically from a per-repo script; the tool keeps no defaults and
reads no config. Flags `--label` / `--assignee` / `--reviewer` are each
repeatable, and `@me` resolves to the authenticated user; at least one is
required.

Semantics:

- **Additive and idempotent.** It only adds what you list and never removes
  anything, so a project's own label/reviewer automation is never clobbered and
  re-running is safe.
- **Best-effort.** A sub-item that fails — an unknown label, a self-review the
  platform forbids — is logged and skipped; the rest still apply.
- Unlike `anno`, it does **not** skip a standalone branch: a lone MR still wants
  labels.

Per platform, hidden behind `apply_attributes`:

- **GitHub** uses add-only endpoints (issue labels/assignees, requested
  reviewers) — naturally additive, no read-merge.
- **GitLab** uses `add_labels` for labels; assignees/reviewers are id lists with
  no add verb, so it reads the current ids and unions ours in. Usernames (and
  `@me`) resolve to numeric ids.
- **Gitea** resolves label names to ids and uses add-only label/reviewer
  endpoints; assignees are unioned through an issue edit.

## 8. `slice`

Cut the commits on top of a base into named branches, by driving `git rebase -i`
with a sequence editor that seeds the todo with each commit and an `update-ref`
line. The refs are set at the *end* of the rebase (safe for the current branch
and for worktrees). The branch list is read back from the saved todo, not from a
post-rebase `base..HEAD` scan (which misleads when the current branch is an
intermediate update-ref target).

### What each `update-ref` line is, per commit

The suggestion is chosen from what we know about the commit, so re-slicing an
existing stack needs no retyping:

1. **A branch already in the stack** points here → the line is **active**
   (uncommented) under that real name, so the branch is preserved in place.
2. **No stack branch, but some branch** points here → a **commented** line with
   that branch name (a suggestion to adopt it).
3. **No branch at all** → a **commented** `<prefix><slug>` suggestion — the name
   to mint for fresh work.

At most **one** line per commit is ever active. Several branches on one commit are
not a fork (a fork diverges later); activating two would make the linear record
collapse them into a bogus parent→child chain (an empty MR), so the extras are
demoted to commented suggestions. The names you uncomment are de-duplicated and
the base is dropped before writing.

- **A single slice is linear by nature** — a rebase range is one line of commits,
  so `update-ref` can only mark points along it. Forks are not expressible in one
  slice.
- **Before writing**, the assignment list is de-duplicated and the base itself is
  dropped, so a slug collision (two similar commit subjects suggesting the same
  name) or a stray `update-ref` to the base cannot create a self-loop.
- **Writing** lays the branches as a chain `base → b1 → … → bn` via `reparent`,
  which **refuses any link that would form a cycle** and leaves unrelated stacks
  in the file untouched.

### Growing or rebuilding an existing stack

`slice` records the branches whose `update-ref` line is active and chains them
from `base`. Because branches already in the stack come pre-filled active (tier 1
above), re-slicing preserves them automatically — you only touch the lines you
want to change.

- **Append to the tip.** Run `slice --base <tip-branch>` so the todo holds only
  the new commits; uncomment a name for each and they attach under the tip, the
  existing chain untouched (slicing a sub-range never disturbs the rest of the
  forest). With the default base instead, the existing branches are already
  active, so you'd just uncomment the new tail.
- **Insert or rebuild within a line.** Slice from the line's base: the existing
  branches are already active in commit order, so you only uncomment the new
  middle one (and reorder commits as needed). `reparent` *moves* the downstream
  branch under the new node (detaching it from its old parent), so the result is a
  clean chain with no orphan — unlike removal, insertion leaves nothing behind.

---

## 9. Editing the structure (`wits stack tree`)

`slice` builds the forest from a rebase; `tree` is the set of direct edits for
everything else. The rule shared by all of them: **removing a branch never
discards the work above it** — `remove` splices a node's children up into its
slot, so a mid-stack deletion keeps the downstream line (and `submit` then
retargets its base). The base branch is protected from removal.

| Command | Effect |
|---|---|
| `tree prune` | Drop every node whose branch no longer exists locally; each removed node's children splice up. Needs no names, idempotent, network-free — the automation cleanup. A live fork sibling keeps its node (its ref still exists), so it is never collateral. |
| `tree rm <branch>… [--delete]` | Remove named branches from the stack (children splice up). `--delete` also runs `git branch -d` (`-D` with `--force`). Refuses to remove the base branch. |
| `tree mv <branch> --onto <parent>` | Re-parent a branch; **its whole substack moves with it** (children come along). Refused if it would place the branch beneath its own descendant (the cycle guard). Creates the node if it wasn't recorded yet, so `mv` also serves as "add this branch onto X". |

`tree mv` changes the *declared* shape only — it does not move commits. After a
move, rebase the branch onto its new parent for the code to match; `submit` then
retargets the MR base.

### Adding and removing mid-stack

The verbs are stateless re-readers of the file, so dynamic correctness is just a
matter of keeping the file in step with reality; `submit`'s base correction (§5)
does the rest.

| Operation | How | Result |
|---|---|---|
| **Insert** B between A and C | `slice` (re-run), `tree mv B --onto A` then `tree mv C --onto B`, or edit the file | C ends up under B; `submit` creates B's MR and retargets C's base to B |
| **Remove** B (between A and C) | `tree rm B`, `tree prune` (after deleting B's branch), or delete B's line in the file | C reattaches to A; `submit` retargets C's base to A |
| **Remove** B via re-`slice` | re-run `slice`, assign A, C | ⚠️ `slice` does not prune, so B lingers as a dead sibling. Follow with `tree prune` (if B's branch is gone) or `tree rm B` |

Deleting a branch from the file by hand is still valid — the parser is
indentation-agnostic, so the orphaned child reattaches to the grandparent without
re-indenting.

---

## 10. Known limitations

- **Re-`slice` does not prune** a branch dropped from a line; it lingers as a
  dead node (auto-pruning inside `slice` is unsafe — it cannot be told apart from
  a fork sibling that should survive). Clean it with `tree prune` (once the
  branch is deleted) or `tree rm`.
- **An MR orphaned by removal** (its branch no longer in the file) keeps its old
  navigation block; `anno` no longer touches it.
- **`tree mv` is metadata only** — it does not rebase commits; you must restack
  the branch yourself for the MR to be meaningful.
- **Cross-fork MRs** are supported on all three: GitHub/Gitea via the
  `owner:branch` head; GitLab via its cross-project mechanism (create on the
  source project with a numeric `target_project_id`, read/edit on the target),
  which costs two extra project-id lookups when a fork is detected.
- **Gitea/Forgejo base changes** depend on the server version honouring the
  `base` field; a server that doesn't degrades to a per-branch warning, never a
  corruption.

---

## 11. Where the logic lives

| Concept | Location |
|---|---|
| forest parse/serialize, tree algebra (`ancestors`/`subtree`/`linear_stack`/`anno_blocks`/`reparent`) | `src/cmd/stack/topology.rs` |
| scope selection, base resolution, per-branch base | `src/cmd/stack/resolution.rs` |
| push | `src/cmd/stack/sync.rs` |
| MR reconcile decision | `src/cmd/stack/submit.rs` (`decide`) |
| navigation rendering + splice | `src/cmd/stack/anno.rs` |
| attribute application (labels/assignees/reviewers) | `src/cmd/stack/decorate.rs`, `src/util/forge/*` (`apply_attributes`) |
| structure edits (prune/rm/mv), `remove` splice | `src/cmd/stack/tree.rs`, `topology.rs` (`remove`) |
| forge primitives + normalized MR + detection | `src/util/forge/` |
| remote URL parsing + origin/upstream roles | `src/util/forge/remote.rs` |

---

## 12. Invariants

1. `sync`, `submit`, `anno` must share one scope computation — never fork the
   fork-point rule across verbs.
2. `anno_blocks` must stop at the next fork-point; expanding it grows
   descriptions combinatorially.
3. Exactly one navigation marker pair per description; stripping relies on it.
4. The base branch is excluded from the operable set but included in `anno`
   chains.
5. `find` matches an MR by its branch only, never by base — otherwise a drifted
   base becomes invisible and `submit` would create a duplicate instead of
   retargeting.
6. `reparent` must refuse cycles; it is the only operation that can introduce one
   (parsing yields a forest by construction).
7. `remove` must splice children up, never drop the subtree; removing a branch
   cannot destroy the work stacked above it.
8. `decorate` is additive only — it adds attributes and never removes them, so it
   cannot clobber a project's own label/reviewer automation.
