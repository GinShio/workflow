# `wits worktree`

Make, inspect, and reclaim git worktrees — in **any** repository.

`wits worktree` reads nothing from the project registry and keeps no state of its
own. It works on whatever repo you are standing in, asking git for everything it
needs, so it serves a registered project and a repo you cloned five minutes ago
equally well.

It is deliberately *not* a wrapper around `git worktree`. Each verb does **one**
thing, and each exists because git leaves a gap:

| Verb | The gap it fills |
|---|---|
| `create` | `git worktree add` never materialises submodules — and a linked worktree shares no submodule object store with the primary, so the obvious `submodule update --init` re-clones everything. It also *invents branches*; this does not. |
| `switch` | Moving an existing worktree to another branch, without the create/checkout/branch-creation overloading `git` spreads across three commands. |
| `info` | `git worktree list` says *where* a worktree is, not whether you still need it. |
| `prune` | `git worktree prune` only forgets records of directories that are **already** deleted. It will not reclaim a worktree whose branch merged last week. |

`move`, `lock`, `unlock`, and `repair` are absent on purpose: they would be pure
pass-throughs, and `git worktree` already spells them.

> `wits worktree prune` and `git worktree prune` are **not** the same thing. git's
> forgets stale bookkeeping; this one removes worktrees whose work is finished —
> and folds git's record cleanup in, so it is a superset.

## The commands

```sh
wits worktree create [--detach] <rev> [dir] [--submodules]
wits worktree switch [--detach] <rev> [target] [--submodules]
wits worktree info   [target] [--merged] [--gone] [--older-than SPEC] [--long] [--path]
wits worktree prune  [target] [--merged] [--gone] [--older-than SPEC] [--force]
```

Nothing here touches the network.

## One act per verb

`create` makes a **new** worktree. `switch` moves an **existing** one. Neither ever
creates a branch. That separation is the point, and it is not what bare `git`
does: left to itself, `git worktree add <path> <name>` will invent a local branch
— from a same-named remote branch if it finds one, otherwise from the directory
name — so "make me a worktree" quietly becomes "make me a worktree *and* a
branch". Creating a branch is `git branch`'s job, and it stays there.

So `<rev>` must **already exist**:

| You write | What happens |
|---|---|
| `create feature/x` | `feature/x` must be a local branch; the worktree checks it out |
| `create --detach v1.2.3` | any revision — tag, commit, `origin/theirs` — checked out with a detached `HEAD` and no branch added |
| `create feature/x` where it only exists as `origin/feature/x` | **refused**, naming both remedies |

That last error reads:

```
branch 'feature/x' does not exist locally, only as 'origin/feature/x';
create it with `git branch feature/x origin/feature/x`,
or use --detach to check out 'origin/feature/x' without adding a branch
```

`--detach` is the answer to "I just want to look at someone's branch": it gets you
a worktree at that commit without polluting your local branch namespace.

## Creating

```sh
wits worktree create feature/x              # -> ../<repo>.feature_x
wits worktree create feature/x /srv/wt/x    # …or wherever you say
wits worktree create --detach v1.2.3        # a tag, at a detached HEAD
wits worktree create feature/x --submodules # …and materialise its submodules
```

The default location is a **sibling of the main worktree**, named
`<main-worktree-name>.<slug>`, where the slug is the revision with anything
outside `[A-Za-z0-9._-]` folded to `_`. That keeps every worktree of one repo
together, and means a worktree is never created *inside* another one. Give an
explicit directory as the second argument when you want it elsewhere; missing
parent directories are created.

A **bare** repository has no main worktree, so one of its live checkouts stands in
— the one holding its symbolic-HEAD branch (the project bootstrap, in the layout
`wits update` builds), else any other. The bare directory itself is the anchor only
when the repository has no checkout at all. That is what keeps the *bare-style*
layout working, where the git-dir is deliberately parked away from the checkouts:
anchoring on the git-dir would drop every default worktree into the `.bare` tree
instead of beside the checkouts it belongs with.

Bare-style goes one step further and drops the `<repo>.` prefix as well, because
there is nothing to disambiguate against:

| Layout | Default location |
|---|---|
| A working tree of its own (`/src/proj`) | `/src/proj.feature_x` |
| Bare, git-dir beside the checkouts (`/src/proj.git`, `/src/proj.main`) | `/src/proj.main.feature_x` |
| Bare-style: git-dir apart from them (`<root>/.bare/<org>/<repo>`, `<root>/<org>/<repo>/main`) | `<root>/<org>/<repo>/feature_x` |

The discriminator is whether the git-dir sits in the same directory as the
checkouts. Where it does, that directory is shared with everything else in it and a
worktree has to stay identifiable by name. Where it does not, the checkouts have a
directory of their own holding one entry per branch — which is exactly what a
`worktree_dir` template renders to — so the slug names one outright.

`create` is **idempotent**: if the directory already exists it says so and
succeeds, which makes it safe in a script or a git hook. It deliberately does
*not* move an existing worktree onto `rev` — that would pull someone's `HEAD` out
from under them on what reads like a create, and it is what `switch` is for.

A branch that is **already checked out in another worktree** is refused by git,
which names the worktree holding it.

## Switching

```sh
wits worktree switch other-branch                 # the worktree you are standing in
wits worktree switch other-branch repo.feature_x  # …or one you name
wits worktree switch --detach v1.2.3 repo.review  # …at a detached HEAD
wits worktree switch other-branch --submodules    # …and realign its submodules
```

Omit the target and it moves the worktree you are in; there has to be one, so
standing in a bare repository's own directory is an error rather than a guess.

Switching **refuses a worktree holding uncommitted changes**, since moving `HEAD`
would bury them. Untracked build output is left alone, which is what makes moving
one worktree cheaper than making another.

One thing worth knowing about naming a target: a worktree can be named by its
path, its branch, or its directory name — but its *branch* is the thing `switch`
changes, so the path and directory name are the handles that stay stable across
switches.

Both repository shapes are supported:

- A **bare** repository — "one bare clone, a worktree per branch" is a normal
  setup. Worktrees land beside one of its live checkouts (see the table
  [above](#creating)), falling back to the bare directory only while it has none,
  and the bare repository itself is never reclaimable. What such a repository can
  tell you depends on how it was made; see
  [below](#a-bare-repository-made-by-git-clone---bare).
- A repository that is itself a **submodule**. Its worktrees land beside its
  working tree (`<super>/sub.<slug>`), never inside `<super>/.git/modules/`.
  This needs saying because `git worktree list` reports a submodule's main
  worktree as its *git-dir*, so the raw git answer points into `.git`.

### A bare repository made by `git clone --bare`

`git clone --bare` is a poor host for worktrees, and it is worth knowing why
because the symptoms look like bugs elsewhere. It maps the remote's
`refs/heads/*` onto the **local** `refs/heads/*`, so the repository starts with
one local branch per branch anyone ever pushed — thousands in a shared tree — and
nothing distinguishes the one you are working on. It writes **no fetch refspec**,
so `git fetch` afterwards updates no ref at all. And it publishes no
`origin/HEAD`, so no trunk is found and nothing ever reads as `merged` — use
`--gone` or `--older-than` in such a repository.

`wits update` builds a bare-backed project's repository with `git init --bare` +
`git remote add` + `git fetch` instead, which inverts all three: the remote's
branches live in `refs/remotes/origin/*`, a plain `git fetch` keeps them current,
`origin/HEAD` is published, and `refs/heads` holds only the branches you chose to
work on. The one thing you notice day to day is that `create` refuses a branch you
have not asked for yet, and names the remedy:

```sh
git branch feature/x origin/feature/x   # …then create the worktree
```

`update` repairs the missing refspec in a repository cloned before this, since
adding one is additive. It cannot un-invent the local branches, so clearing those
is a one-time manual step. A branch is provably safe to drop when it points at
exactly what its remote-tracking ref points at, no worktree holds it, and `HEAD`
is not on it — the reflog aside, there is nothing in it the remote does not have:

```sh
cd <bare>
git fetch origin                        # after `wits update` added the refspec
{ git worktree list --porcelain | sed -n 's|^branch ||p'
  git symbolic-ref -q HEAD; } | sort -u > /tmp/wits-keep
git for-each-ref --format='%(refname) %(objectname)' refs/heads |
  while read -r ref oid; do
    grep -qxF "$ref" /tmp/wits-keep && continue
    tracked=${ref#refs/heads/}
    [ "$(git rev-parse -q --verify "refs/remotes/origin/$tracked")" = "$oid" ] &&
      printf 'delete %s %s\n' "$ref" "$oid"
  done | git update-ref --stdin
```

`git update-ref --stdin` names each old value, so a ref that moved under you is
refused rather than dropped, and one invocation handles a few thousand branches.

### Sparse checkout: nothing to configure

If your checkout is sparse, a new worktree inherits the same cone automatically.
That is git's own behaviour since **2.36** — `git worktree add` copies the
sparse-checkout pattern file and `config.worktree` into the new worktree, in cone
and non-cone mode alike. `wits worktree` therefore does nothing about sparse
patterns: git's copy is the file verbatim, where anything replayed through
`sparse-checkout list`/`set` could only lose fidelity.

One consequence worth knowing: git copies the patterns of the worktree the `add`
ran **from**. That is the same anchor the default location is derived from — the
main worktree, or for a bare repository one of its live checkouts, preferring the
one on the symbolic-HEAD branch — so the cone never depends on which worktree you
happened to be standing in.

Its one gap is the **first** add into a freshly created bare host, which has no
checkout to copy from and so starts out full. That is why `wits update` writes a
bare-backed repo's `skip` mask onto the bootstrap before materialising anything in
it: otherwise a skipped submodule would be cloned in full and only then
deinitialised.

### Submodules: borrowed, not re-downloaded

`--submodules` materialises the worktree's submodules. The **first** time, it
borrows objects through git alternates, so even a large submodule costs no
download of its own; every level of a nested tree borrows from its own store. On a
later HEAD move it just **updates** the already-present submodules to the new
pins — the borrow is a one-time materialisation concern.

It is idempotent, so it also works as a cheap second pass over a worktree you
first made lightweight.

#### What it borrows from, and why that is safe

Always a store the **repository** owns, at `<git-dir>/modules/<name>` — never one
belonging to a worktree. That distinction is the whole design:

- a **conventional** clone already has such a store. It is git's own, the main
  worktree fills it, and the main worktree cannot be removed;
- a **bare** repository has no main worktree, so git never fills that slot. Left
  alone it files each linked worktree's submodule git-dir under *that worktree's*
  administrative directory (`<bare>/worktrees/<id>/modules/<name>`), which `git
  worktree remove` deletes. Borrow from there and removing one worktree leaves
  every other one with an unreadable alternate. So `wits` gives the repository a
  store of its own, and every worktree — including the bootstrap, including the
  replacement you make after deleting one — borrows from there.

The store is created **before** anything is materialised, which is what makes the
first worktree an ordinary borrower rather than a special case. Given nothing on
disk, the store is downloaded straight into the repository (`git clone --bare` of
the submodule's own URL, resolved by `git submodule init` so a relative URL works)
and the checkout that asked then borrows it. Given objects already on disk in some
live worktree — a repository set up before this, most often — the store is copied
from there with `git clone --local`, which hardlinks the object files: an 8 GiB
tree is published for the cost of its inodes.

The order matters more than it sounds. Downloading into the worktree first and
copying afterwards leaves **two** full stores for one submodule: the copy the
repository keeps, and the one the first checkout owns and does not borrow from.
They are hardlinked at birth and diverge from the next fetch on.

The store is keyed by the submodule's **name**, not its path, and nested exactly
the way git nests it. This is not a preference: git derives a nested store's
location as `<parent-store>/modules/<name>`. The two coincide in most
repositories, because `git submodule add` defaults the name to the path.

Nesting is walked one level at a time rather than with `--recursive`, because each
level borrows from a different store and a level git materialises before its store
exists downloads a copy nothing can reclaim afterwards.

Stores are marked `extensions.preciousObjects`, so `git prune` and `git repack -d`
refuse to run inside them. Worktrees hold only a pointer into these objects, so
deleting one would corrupt them; `git fetch` still works — a store carries the
fetch refspec `clone --bare` omits — which is all a reference store needs. Undo
with `git config --unset extensions.preciousObjects` in the store.

A repository set up before this has the duplicate: a store under `<bare>/modules/`
*and* a full copy under `<bare>/worktrees/<bootstrap>/modules/`, borrowing from
nothing. One round trip through the bootstrap reclaims it, because the store is
already there for the new checkout to borrow:

```sh
git -C <bare> worktree remove --force <bootstrap>
wits worktree create <main-branch> <bootstrap> --submodules
```

Check `<bare>/modules/<name>/objects` exists first. If it does not — a repository
whose submodules were only ever initialised by hand — removing the bootstrap
throws away the one copy on disk, and the store is downloaded again. Materialise
another worktree with `--submodules` before removing anything and the store is
published from the bootstrap's copy for the price of hardlinks.

## Inspecting

```sh
wits worktree info                  # a table of every worktree
wits worktree info feature/x        # one worktree, as a panel
wits worktree info --long           # every worktree, as panels
```

A `target` is whatever you naturally reach for — the worktree's **path**, its
**branch**, or its **directory name**. An ambiguous target is an error rather
than a guess.

### The table

```
repository  /home/russell/cyber/workflow-rs/.git
trunk       origin/main

BRANCH      HEAD     STATE                  PATH
main        11658dd  main, current          /home/russell/cyber/workflow-rs
busy        11658dd  dirty, merged          /home/russell/cyber/workflow-rs.busy
diverged    df1e429  -                      /home/russell/cyber/workflow-rs.diverged
goneup      11658dd  merged, upstream gone  /home/russell/cyber/workflow-rs.goneup
(detached)  11658dd  locked                 /home/russell/cyber/workflow-rs.review
tmp         11658dd  records only           /home/russell/cyber/workflow-rs.tmp
```

The first block describes **the repository**, not any worktree: where its git dir
is, and what `merged` is measured against. For a bare repo it reads
`/path/b.git (bare)`; for a repository that is itself a submodule it reads
`…/super/.git/modules/sub`, which is how you can tell at a glance what shape of
repository you are in.

`HEAD` is abbreviated the way **git** would in this repository — it honours
`core.abbrev`, so a hash here is the same one `git log --oneline` prints and stays
copy-pasteable.

`STATE` is the whole point of the table; it answers "can I delete this?":

| Tag | Means |
|---|---|
| `main` | The repository's own working tree. Never reclaimable. |
| `current` | The worktree you are standing in. Never reclaimable. |
| `locked` | Pinned with `git worktree lock`. Never reclaimable. |
| `records only` | The directory is gone; only git's record is left. Exclusive — no other tag bears on what happens to it. |
| `dirty` | Uncommitted changes or new untracked files live here. |
| `merged` | Nothing on this branch that the trunk lacks — the work landed. |
| `upstream gone` | The branch had an upstream that no longer exists on the remote. |
| `dormant` | Shown when `--older-than` selected it. |
| `-` | Nothing worth flagging. |

`--path` prints just the path, one per line — the form a shell wants:

```sh
cd "$(wits worktree info feature/x --path)"
```

### The panel

Naming one worktree — or passing `--long` for all of them — gives the full record
instead of a row. **A row appears only when it has something to say**, so every
line you see carries signal:

```
/home/russell/cyber/workflow-rs.feature_x
  branch      feature-x
  head        9f8e7d6   3 weeks ago
  trunk       3 ahead of origin/main
  changes     2 modified, 1 untracked
  tracking    origin/feature-x — 1 behind
  locked      keeping for bisect
  sparse      crates, docs
  submodules  2 of 3 initialised
  prune       kept — uncommitted changes (--force to remove)
```

`tracking`, `locked`, `sparse` and `submodules` are the conditional ones. A bare
repository has no HEAD and no working tree, so it shows neither `head` nor
`trunk`, and `changes` says why it is empty rather than claiming to be clean.

Commit counts are spelled out and drop their zero component — `3 ahead`,
`1 behind`, `3 ahead, 1 behind`, or `up to date`. Deliberately **no `+`/`-`**,
which would read as a count of changed lines rather than of commits.

The `prune` row is the answer the command exists for. It states what `prune` would
do and why — `would remove — merged`, `kept — 3 commits not on origin/main`,
`kept — uncommitted changes (--force to remove)`, `never — you are in it`. It is
computed from the very same predicate the sweep uses, so it cannot disagree with
what actually happens.

### Filters, which are also `prune`'s predicates

`--merged`, `--gone`, and `--older-than` mean exactly the same thing to `info` as
to `prune`, so **`info --merged` is an exact preview of what `prune --merged`
would remove**. With no filter, `info` shows everything, and the `prune` row
answers for the default sweep.

## Reclaiming

```sh
wits worktree prune                          # the default sweep
wits worktree prune --merged                 # only what landed on the trunk
wits worktree prune --older-than 30d         # …plus what has sat still 30 days
wits worktree prune --older-than 2026-06-01  # …or since a date
wits worktree prune feature/x                # just that one, whatever its state
```

A bare `prune` sweeps the two signals that work **demonstrably landed**:
`merged` and `upstream gone`. Then, whatever the filter selected, it forgets git's
records for directories that are already gone.

**Dormancy is never implied.** "Nobody has touched this lately" is not evidence
that work is finished, so it applies only when you ask with `--older-than`, which
takes a day count (`30`, `30d`, `4w`) or an ISO-8601 date. A bare four-digit
number is refused as ambiguous — write `2026d` or `2026-01-01`.

Four kinds of worktree are **never** swept, however the filters are set: the
**main** worktree (that is the repository itself), the **current** one (removing it
would delete the ground under your shell), a **locked** one (the lock is the
instruction not to), and one whose **directory is already gone** (there is nothing
to remove, only a record to forget).

Naming the worktree you are standing in is refused as well — `cd` out and try
again.

### What protects your work

A worktree holding **uncommitted changes or untracked files** is not removed
without `--force`. The two paths differ on purpose:

- in a **sweep** it is kept with a note, so one work-in-progress worktree cannot
  stop the other five from being reclaimed;
- when you **name it**, refusing is an error — you asked for that one specifically,
  so silence would be the wrong answer.

Ignored files (a `.gitignore`d `_build/`, say) do **not** count as uncommitted
work, so build output never blocks a sweep.

**`prune` never deletes a branch.** That is a deliberate limit, not an omission.
Removing a worktree discards a *checkout*; the branch and every commit on it stay
exactly where they were, so the only thing `--force` can ever destroy is
uncommitted changes and untracked files. That one sentence is what makes a sweep
safe to run without reading the output first, and deleting branches would cost it.

The consequence to know is that cleanup is deliberately half of the job: sweep
enough worktrees and you will accumulate merged local branches with nothing
checked out. Clearing those is `git branch -d`'s business — it refuses an unmerged
branch, which is the same conservatism by a different route.

Preview any sweep with `-n`, which prints the plan on stdout and changes nothing:

```sh
wits worktree prune -n
```

## Who else uses this

[`wits review checkout`](review.md#reviewing-the-code-itself-checkout) materialises
an MR into a worktree through the same code — same default-location rule, same
submodule borrowing, same dirty guard. Review keeps only its own policy on top:
*one* worktree for the whole store (`../<repo>.review`), re-pointed at whichever
snapshot you are reading.

Because this command knows nothing about the review store, a review worktree
sitting on a snapshot that has since **merged** looks exactly like any other
landed work, so a sweep will reclaim it. Nothing is lost — one `wits review
checkout <mr>` puts it back — but that is why it can disappear from under a
finished review.

`wits build` meets this command at the path and nowhere else: create a worktree,
then point a build at it.

```sh
wits worktree create feature/x
wits build --work-dir "$(wits worktree info feature/x --path)"
```

## Invocation forms

Like every `wits` tool, `worktree` has a direct form via symlink —
`wits-worktree` — created by `meson install` (see the top-level
[README](../README.md)).
