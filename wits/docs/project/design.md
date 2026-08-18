# `wits project` — Design

> Status: **implemented (v1)**. This is the agreed shape of the `project` tool
> and the reasoning behind it. Items marked **[open]** still need a decision;
> everything marked **future**/**TODO** is deliberately out of v1; everything else
> is settled and reflected in the code.
>
> This file explains *why the tool is shaped the way it is*. The reader-facing
> documents explain *how to drive it*: [`project.md`](../project.md) is the usage
> guide and [`project/reference.md`](reference.md) is the exhaustive key/flag
> reference. Neither restates the other — rationale lives here, behaviour-for-users
> lives there.

This is the reference design for the `project` sub-tool of `wits`. It records the
shape we agreed on and the reasoning behind each decision, so later work has one
place to consult rather than rediscovering the trade-offs.

---

## 1. Motivation and boundaries

Two genuinely different concerns must never be fused: *what a project is* (where
it lives, which branch, resolved build/install dirs, git state) and *how to build
it* (toolchain/preset layering, command generation). Fuse them and every
read-only operation — listing, validating, reporting — gets dragged through build
*planning* just to introspect, which is complexity no one asked for.

A subtler trap lies in the layering itself. If each configuration layer's
templates can reference *and overwrite* the previous layer's resolved values, the
resolver has to keep looping back on itself — rebuilding its context and
re-asserting authoritative values after every layer. The design forecloses that
by making the layering strictly one-directional (§5).

The design draws two hard lines.

### 1.1 The read/act split

A small, pure, **read-only core** describes and resolves; heavier **action**
modules build, update, and manage per-branch build contexts on top of it. The
core has no side effects beyond reading config and git state, so scripts and
other tools (notably `wits stack`, §9) can consume it freely; the actions'
complexity never leaks back into it.

### 1.2 We are a mechanism, not a policy engine

The single most clarifying principle, relied on everywhere below:

> **We apply what the user declared; we do not transform it, and we do not
> guess.** Overrides passed on the command line or via the environment are
> layered on verbatim — the tool never reinterprets what a `-DFOO=BAR` *means*.
> The one and only place we translate is a *declared toolchain's* canonical
> fields into a build system's native spelling (§5.4), because that is the whole
> point of declaring a compiler once. Everything else is pass-through.

This principle is why there are no built-in toolchains, no runtime linker
probing, no path-inferred organisations, and no clever "fix the drifted remote
URL for you" behaviour. When the tool does less, it surprises less.

### 1.3 CLI surface

The read/act split (§1.1) is carried onto the command line itself: `project` is
the **read** command, while the mutating actions `build` and `update` are their
own top-level commands. `project` itself therefore mutates nothing at all.

```
wits project [<name|path>] [--check]                # describe / list / validate (read-only; the default)
wits build   [<name|path>]                          # configure + build + (un)install
wits update  [<name|path>]                          # refresh git for a project's repos
```

Worktrees are `wits worktree`'s, not `project`'s (§8.3): the act is not
project-shaped, so scoping it to the registry only made it less useful.

Each earns the busybox applet forms automatically (`wits-build`, `build`,
`wits-project`, …), like every other `wits` sub-tool. Global `-v/--verbose` and
`-n/--dry-run` are inherited from the `wits` process layer: every mutating action
respects dry-run, every read still runs.

Splitting the commands this way is not a departure from the "one core, many
consumers" shape — `build`, `update`, and `project` all sit on the *same*
read-only core (§1.4); only the CLI grouping changed. Any future project-related
verb makes the same choice independently: nest under `project` if it is rare or
tightly coupled to the read surface, promote to a top-level command if it is
frequent enough to deserve a terse form (as `build`/`update` were). Because the
core neither knows nor cares which side of that line a consumer falls on
(§1.4), the choice is cheap to revisit later.

### 1.4 Library shape — core plus actions

The CLI grouping is *not* the code grouping, and it is *also* not the same as
the crate-module grouping. `project`'s read-only **core** (`model`, `workspace`,
`resolve`, `git`) is a self-contained subsystem, so it lives under `util` as
`wits_util::project`, not inside a command; the build systems live beside it as
`wits_util::build_system`. The `cmd` layer is then thin CLI shells: `cmd::project`
(describe / `--check` / path queries), `cmd::build`, and `cmd::update` all consume
the core's public API — `resolve_target`, `resolve::plan`, `git` — the same way an
external tool would:

```
                        wits_util::project
          (read-only core: model / workspace / resolve / git)
            ▲             ▲             ▲                ▲
            │             │             │                │ implements
      cmd::project   cmd::build    cmd::update    wits_util::build_system
      (info/check,       │                        (cmake/meson/cargo)
       queries)          └──────────── also uses ─────────┘
```

The build systems (`cmake`/`meson`/`cargo`) live in `wits_util::build_system`,
beside the core rather than inside any command, because emitting build steps is
a purely build-time concern the core never touches. The core has exactly one
tie to them: translating a *selected toolchain* into a backend's native
env/definitions at L0 (§5.4). That tie is a one-method seam,
`resolve::ToolchainInjector`, **defined by the core (`wits_util::project`) but
implemented by each backend (`wits_util::build_system`)**; `cmd::build` resolves the
chosen backend and hands it to `resolve::plan` as the injector, while path-only
callers (the `*-dir` queries, `info`) inject nothing. So `wits_util::project`
exposes no `Backend`, `Step`, `EmitContext`, or build-system registry at all — only
the abstract seam — and the dependency still points one way: `build_system` →
`project`, and each `cmd` shell → the core, never back into a command.

The rule the layout follows: an action with its own top-level verb has no business
reaching into `project`'s private internals, only its public API. `build` and
`update` moved out for exactly that reason, and it is why `wits worktree` — which
took over the one mutating action `project` used to nest (§8.3) — shares no code
with `project` at all, only a path.

### 1.5 Out of scope

Git hooks, the `bin/git` safety proxy, and `ssh-key.sh` stay as shell. `project`
is a *programmatic* tool only. **Cross-project dependencies are out of scope**:
building one project never triggers building another. Dependency compilation is
rare, and supporting it would drag in a whole graph / topological-order /
profile-propagation subsystem for little benefit. A `build` builds exactly one
project.

One thing does cross the project boundary, and it is worth being precise about
why it is not the above: a repo may be **borrowed** from another project with
`from` (§3.6). What crosses is a *repo's identity*, resolved once at load into a
plain path — not a build. Nothing is compiled in dependency order, no profile
propagates, and a plan still spans exactly one project.

---

## 2. Core concepts

| Concept | One-line definition |
|---|---|
| **Repo** | One git checkout — a `path`, remotes, a main branch, lifecycle hooks, a branch strategy, and an `anchor`. Whether it is standalone / submodule / subtree is *inferred* from its path and `main_branch` (§3.5), never declared. |
| **Borrow** | A repo declared with `from` instead of a `path`: it *is* another project's repo, so a component several projects consume has one home (§3.6). |
| **Skip** | The paths a checkout never materialises (§3.7) — what lets a borrower keep its own copy of a shared component out of the way. |
| **Project** | A buildable unit: references one or more Repos as `[repos.NAME]` (a `repos.main` is always required), names which one is the `focus`, plus build configuration. |
| **Profile** | The axes that affect *resolution*: `build_type`, `toolchain`, `generator`, `branch`, and the active `presets`. |
| **Preset** | A reusable, named, inheritable bundle of build config across three levels (org → project → repo). |
| **Toolchain** | A named, user-declared set of compilers/tools/flags, *selected* per build and *translated* to a backend's native form. |
| **Backend** | A build system (cmake / meson / cargo …). The tool's only extension axis. |
| **Build context** | A branch's physical build space: a worktree (+ build dir) or just a build dir, depending on strategy (§8.3). |

The unifying principle, stated once and relied on everywhere:

> **A submodule is just a Repo whose working path happens to be nested.** It is a
> normal `[repos.NAME]` entry, managed exactly like a top-level Repo (same
> remotes, branch, hooks, update logic). The only difference is *where it sits*,
> never *how git is handled*.

---

## 3. Repo model

A Repo describes one git checkout. A Project may have several (§4); the fields
below apply to each `[repos.NAME]`.

### 3.1 Remotes and roles — additive only

Remotes are declared with **roles**, so commands never guess which remote does
what:

- **`origin`** — the repo we push to (often our fork). It is only the *clone /
  fetch* source when no `upstream` is declared; when an `upstream` is present,
  nothing ever clones or fetches `origin`, so a fork that does not exist on the
  server yet is fine — it is merely added as a push target.
- **`upstream`** — the **sync source**: what `clone` and the default `update`
  fetch from and fast-forward `main` against. When absent, the sync source falls
  back to `origin` (we do not create a git remote for `upstream` in that case).
- **`mirrors`** — extra *push* URLs attached to `origin`, so one push fans out.

The **sync source is a single concept** used consistently — `upstream` if
declared, else `origin` — for the clone source, the on-`main` fetch+merge, and
the off-`main` ref-only fast-forward. This is what makes tracking an upstream
while owning a not-yet-created fork work: the fork (`origin`) is never fetched.

```toml
[repos.main.remotes]
origin   = "git@github.com:me/mesa.git"
upstream = "https://gitlab.freedesktop.org/mesa/mesa.git"
mirrors  = ["git@codeberg.org:me/mesa.git"]
```

The reconciliation `update` performs is deliberately **additive only, never
modifying**: a declared remote that is missing is added; a declared remote that
already exists is **left exactly as-is** — its fetch URL is never "corrected",
and no warning is emitted, because the URL is the user's to own (§1.2). Missing
mirror push-URLs are added; existing push-URLs are never removed. Remotes the
config does not mention are never touched. The one non-obvious mechanic worth
recording: git stops defaulting `push` to the fetch URL once *any* push URL is
added, so a mirror setup's push-URL list must include the origin URL itself
(`{origin} ∪ mirrors`) — `update` only ever *adds* toward that set.

### 3.2 Main branch

A Repo with its **own git** (standalone / submodule) **must** declare its main
branch explicitly; we do not auto-guess. A `subtree` has no own git, so it has no
main branch — it follows its `anchor`.

### 3.3 Lifecycle and hooks

`clone` and `update` are phased lifecycles. Each phase has a **default action
that can be replaced wholesale**, plus pre/post hooks. Hooks are **inline
templated command strings**, run with `sh -c`:

```
clone:   action (in-place clone, or bare clone + bootstrap worktree) →
         submodule/skip setup → post → skip verification
update:  ensure-remotes → pre → action (default: §7) → post
```

The hook contract (settled, and the reason it is simple):

- **The clone phase has no `pre` hook.** Before the repo exists there is nothing
  useful a pre-hook could act on, so the phase is just `action → post_clone`.
- **cwd is phase-specific**: a `clone` override runs in the **current working
  directory**. For in-place repos, later hooks run in `path`. For bare-backed
  repos, `post_clone` and update hooks run in the bootstrap/current main-branch
  worktree; if no main worktree remains, update hooks fall back to the bare
  `path`. A bare clone override therefore owns creating the configured bootstrap
  worktree as well as the repository.
- **cwd during update is the checkout being refreshed**. An in-place feature
  checkout remains untouched; a bare-backed repo uses the linked worktree
  holding `main_branch`. No default action switches branches (§7).
- **Overriding the `action` hands the user full control.** We run their string
  verbatim in the appropriate cwd — including any branch switching they choose to
  do — and do not switch back for them. This is §1.2 applied to lifecycles: the
  smart no-switch behaviour is the *default action's*, not a wrapper we impose on
  overrides.
- **Any hook exiting non-zero fails fast** (§7): the operation stops, the RAII
  restore guard returns the repo to its original branch, a log line is written,
  and the program exits non-zero. State is never left half-switched.

### 3.4 Branch strategy — in-place, worktree, and hybrid

Each Repo chooses how multi-branch work is physically realized:

```toml
[repos.main]
branch_strategy        = "hybrid"   # in-place | worktree | hybrid
worktree_dir           = "{{repo.path}}.worktrees/{{branch.slug}}"
bootstrap_worktree_dir = "{{repo.path}}.primary"
```

All three are first-class. They are unified behind one resolved per-repo variable
**`{{repos.<name>.workdir}}`** = "the directory where source is checked out for
*this* build's build repo" (§6):

- **in-place**: `{{repos.<name>.workdir}}` = the repo's `path`. Building a non-current branch
  runs the classic dance — stash → switch → build → switch back → pop — driven by
  an **RAII restore guard** that returns to the original branch and pops the stash
  on *any* exit (success, error, or panic). A dirty tree is always auto-stashed;
  because the guard always restores, this is safe and needs no config knob.
- **worktree**: the repo is cloned bare and
  `{{repos.<name>.workdir}}` is the deterministic `worktree_dir` for the target
  branch. `bootstrap_worktree_dir` is optional; without it, the initial
  main-branch checkout uses `worktree_dir(main_branch)`.
- **hybrid**: also bare-backed, but first searches Git's live worktree inventory
  for the target branch and uses its actual path. If absent, `worktree_dir` is
  only the suggested path. Its branch-independent `bootstrap_worktree_dir` is
  required. A relative bootstrap name is anchored beside
  `worktree_dir(main_branch)`, so project resolution never leaks process cwd
  into checkout identity.

Both worktree strategies require `worktree_dir`. `build` requires the worktree
to exist and errors otherwise — it never implicitly creates one. Worktrees are
created explicitly with `wits worktree create` (§8.3).

Because everything downstream references the named build repo's `workdir`,
switching strategy is transparent to the rest of the config. In a *build*,
`{{repos.<build_repo>.workdir}}` is the **build repo** (the focus's `anchor`,
§4.1); when the focus builds itself the two are the same repo, which is the case
this strategy is defined against. Configuration names the repo explicitly, so a
focus/anchor change may require changing those references.

### 3.5 Topology — `path`, `anchor`, and inferred kind

Every project has a **required `[repos.main]`** — the reserved root and entry
point. Other repos hang off it.

```toml
[repos.main]                       # REQUIRED reserved root; standalone
path        = "~/src/mesa"
main_branch = "main"

[repos.inner]                      # inferred submodule: nested path + own main_branch
path        = "subprojects/inner"  # subpath, relative to repos.main
main_branch = "develop"
anchor      = "main"               # build via the root (else builds itself)
```

- **kind is inferred, never declared** — a repo whose `path` is non-nested (an
  absolute location) is `standalone`; a repo whose `path` is nested (relative to
  `repos.main`) is a `submodule` when it declares its own `main_branch` and a
  `subtree` when it does not. `standalone`/`submodule` have their own git; a
  `subtree` shares its anchor's. `repos.main` is always standalone. `info --check`
  validates the inference against actual git state.
- **`path`** — for `repos.main` and standalone siblings, the on-disk repository
  location / clone destination (the bare common dir for worktree/hybrid); for
  nested repos, a subpath relative to main's primary/bootstrap checkout.
- **`anchor`** — names the Repo whose `path` is this Repo's build/config **base**
  (§4.2). It may point at *any* repo, not only `main`. Unset / self → build at
  this repo's own `path`.

### 3.6 One component, many consumers — `from`

A component can be a submodule of several projects at once, at a different path
in each. Declared the obvious way, that means N copies of its git identity in N
files, N checkouts of the same code on disk, and the same feature branch created
N times. `from` collapses all three:

```toml
# engine.toml — the component, declared once, where it lives
[repos.main]
path        = "~/src/engine"
main_branch = "stable"

# viewer.toml — a project that consumes it
[repos.engine]
from   = "engine"            # [<org>/]<project>[:<repo>], default repo `main`
anchor = "main"              # build it through this project's root (§4.2)
```

The split of what travels is one sentence: **a repo's git identity travels, how
*this* build uses it does not.** So `path`, `main_branch`, `remotes`, `hooks`,
`branch_strategy`, `worktree_dir`, `bootstrap_worktree_dir`, and `skip` come
from the source; `anchor`, `source_dir`, and `presets` stay with the borrower —
which is exactly right,
because the per-consumer build knobs (which feature flags this consumer compiles
the component with) genuinely differ per consumer while the repo does not.
Declaring a travelling field *and* `from` is a hard error rather than a silent
precedence rule: a local override would quietly reintroduce the duplication the
borrow exists to remove.

Four decisions make this cheap rather than a dependency subsystem (§1.5 still
holds — nothing here ever builds a second project):

- **It resolves at load, into a plain repo.** The borrower's entry is filled in
  with the source repo's fields and the source's *absolute* path (absolute
  because a nested source resolves against its own project's root, which the
  borrower knows nothing of). Everything downstream — `repo_abs_path`,
  `repo_value`, `infer_kind`, `update`, the resolver — sees an ordinary repo and
  needs no notion of borrowing at all.
- **A borrow may not be borrowed.** That is what keeps resolution a single pass
  over an immutable snapshot instead of a graph walk needing cycle detection.
- **A borrowed repo is `standalone` from the borrower's side**, whatever it is in
  the project that owns it. That is the honest answer *and* the useful one: an
  external checkout, with its own git, that this project's submodule refresh must
  keep its hands off.
- **`from` may name a non-`main` repo** (`from = "viewer:engine"`). This is not
  generality for its own sake: it expresses "the component lives *inside* one of
  the projects that consumes it", which is the layout you already have before
  splitting it out, and therefore the one a migration passes through.

Two consequences follow, and both are rules rather than heuristics:

- **Path ownership.** One checkout claimed by N projects would tie at equal depth
  in `project_for_path`, decided by nothing better than map order — a silently
  wrong answer to "which project am I standing in?". So **a borrow is never a
  candidate** for the lookup. Since a borrow always points at a project that
  declares that checkout as its own, exactly one owner remains, and `cd` into a
  shared component lands on the project it *is*.
- **Update ownership.** The same rule, applied to work: `update` skips borrowed
  repos, so a component five projects consume is fetched once by its owner rather
  than five times. `--with-borrowed` opts in when you do want the sweep. (A
  borrowed hook then runs against the *borrower's* `org.*` namespace; same-org
  borrowing is the case that motivated this, and a cross-org reference simply
  fails loudly the way any unresolvable template does.)

### 3.7 `skip` — the paths a checkout never materialises

A borrow is only half the story: the borrower still has its own copy of the
component at the nested path, which wastes the clone, splits editor indexing
across two trees, and invites editing the wrong one. `skip` is the other half —
and it stands alone just as well (a monorepo you only ever build one component
of).

```toml
[repos.main]
path = "~/src/viewer"
skip = ["/third_party/engine", "/vendor", "!/vendor/keep.c"]
```

The syntax is **gitignore-style, ordered, `!` re-includes** — it says what to
leave *out*. Not templated: these are git patterns, and mixing `{{ }}` into `*`
and `!` would only make them harder to read.

**One declaration, two mechanisms, settled per path at build time.**
`sparse-checkout` alone cannot mask a *materialised submodule*: git tries to
remove the directory, fails because the submodule's content and `.git` are in it,
and leaves the gitlink un-skipped with only a `warning: unable to rmdir`. It has
to be `submodule deinit` **first**, then sparse — after which the gitlink is `S`,
the directory is gone, and the tree is clean. Reversing the order silently does
nothing. So which mechanism a path needs is *inferred* when a checkout is built,
exactly as a repo's kind is inferred rather than declared (§3.5).

**Translation to sparse is a local, order-preserving toggle.** sparse-checkout
speaks the opposite language — its patterns say what to *keep* — so the patterns
are `/*` followed by every `skip` entry with its leading `!` flipped. That the
translation is this simple is not obvious: gitignore refuses to re-include
anything under an excluded directory, so one might expect `["/vendor",
"!/vendor/keep.c"]` to need rewriting into "exclude the contents, then
re-include". It does not — sparse-checkout matches each *index entry* against the
list rather than walking directories, so a nested `!` takes effect as written.
Non-cone is not a choice: cone mode cannot express an exclusion at all. It costs
nothing here (git emits no deprecation warning for `--no-cone`).

**Applied by `clone`, only verified afterwards.** Realising the mask on a tree
that already holds the content means deleting checked-out work, and `update`
never touches a working tree (§7). So `clone` **applies** it — legitimate,
because the tree is ours and still being built, so removing what config says not
to keep is finishing construction rather than repairing reality — while `update`
and `project --check` only **verify**, and fail. Converting an existing checkout
stays your `git` call; `-v` prints exactly which commands, in the order that
works. This is §1.2 at the lifecycle level: we build what you declared and refuse
to fight what you have.

Two mechanical notes worth recording, because both remove code that looks
necessary:

- An in-place clone writes patterns **before** checkout (`--no-checkout` →
  sparse → checkout), so skipped paths never materialise.
- A bare-backed clone must first create a real bootstrap worktree. It initialises
  submodules there, deinitialises the skipped ones, then writes the sparse mask.
- `git worktree add` copies the pattern file of the worktree it runs from. Bare
  repositories therefore prefer the worktree holding their symbolic-HEAD branch
  (the bootstrap) as the source for later adds, preserving the mask in cone and
  non-cone mode.

**What it refuses: overwriting somebody else's patterns.** `sparse-checkout set`
replaces the *whole* list, so applying `skip` to a checkout a `clone` hook had
narrowed to a sparse cone would silently **widen** it to the entire tree while
masking one path out of it — on a large monorepo, a disaster dressed as a
configuration step. Cone mode cannot represent an exclusion either (git rejects a
`!` pattern outright), so there is no safe write to make there at all. Applying is
therefore conditional on the pattern file being empty or already exactly ours, and
anything else is a hard error. This is the one place a *textual* comparison of
patterns is right, and it asks a different question from verification below: not
"is the mask in force" but "may we write this file".

**Verification is behavioural, not textual.** It asks "is anything the list
excludes still materialised?", never "does the sparse file look like what I would
have written" — so your own extra sparse patterns are legal and invisible, and so
is reordering or hand-editing them. Two facts are checked, because each catches a
state the other misses: a wholly excluded path must not exist on disk (catching a
`deinit` with no sparse write, which leaves an empty directory that a build
system probing `if(EXISTS …)` will then find), and every index entry under a
skipped path must be tagged `S` (catching a directory removed by hand without the
sparse write, which git reports as a deletion). A glob entry, and anything under
one, is deliberately **not** checked: deciding those needs gitignore's matcher,
and a second implementation of it would disagree with git's in exactly the corners
that matter. Under-reporting beats guessing.

### 3.8 `anchor` and `source_dir` are orthogonal

`anchor` and `source_dir` are two orthogonal axes. `anchor` names *which repo's*
checkout is the build base (`repos.<name>.workdir`); `source_dir` names *where within that
base* the build system's top-level file (`CMakeLists.txt`/`meson.build`/…) lives,
for the common case where it is not the checkout root. `source_dir` is a templated
path on the build repo (`[repos.<name>]`), defaulting to that repo's `workdir`;
set it to e.g. `"{{repos.main.workdir}}/src"` to configure from a subdirectory.
It changes only the backend's configure source — the named `workdir` stays the checkout root that carries
branch identity and anchors `build_dir`/`install_dir` templates, so the two never
conflate. A subtree cannot express this: a subtree with `anchor = self` has no
own git (no branch identity), and with `anchor = "main"` its `workdir` becomes
the anchor's root, not the subdirectory.

---

## 4. Projects, repos, and the `focus`

### 4.1 One project, one or more repos

A Project lists its git checkouts as named `[repos.NAME]` tables (with a required
`repos.main`, §3.5) and names which one is the build focus:

```toml
[project]
focus = "main"     # focus repo; defaults to "main"
```

- `focus` selects the **focus** repo — the one you are working on. **Branch
  identity, and the repo switched to your target branch, come from the focus**
  (its own git, or the git it shares when it is a subtree); the focus's own
  submodules are aligned to the target's gitlink on a switch. The build repo's
  **`repos.<name>.workdir` (build source) comes from the focus's `anchor`**
  (§4.2). In the common case the
  focus builds itself (`anchor` = self) and the two coincide. `focus` defaults to
  `main` and can be overridden per-invocation with `--focus <repo>` — invaluable
  in a large monorepo where you switch which component you work on without editing
  config.
- `update` touches **all** of the project's repos; `info` reports all of them;
  `build` builds the `focus` (switching *it* to the target branch), sourcing from
  its `anchor`.
- The repos are **local to this project file** — this is *not* repo sharing
  across projects (one project : N repos, never N projects : one repo).

### 4.2 Build source dir: the `anchor`

"Where the build sources from" is decided by each repo's **`anchor`**:

- `anchor` unset / self → the build sources from the repo's **own** `path` (an
  independent build of a submodule, a subtree subdir, or a standalone repo).
- `anchor = "main"` (or any other repo) → the build sources from **that repo's**
  path ("cannot build detached from the root").

This one rule covers every case uniformly — there is no separate "component kind"
or "build at root / source at root" machinery. A subtree that must build via the
root and a submodule that must build via the root are spelled identically:
`anchor = "main"`. `anchor` picks the base checkout; `source_dir` (§3.8) then
picks the subdirectory within it that holds the build system's entry file, when
that is not the root.

A consequence worth stating: when a subtree sets `anchor = "main"`, its own
subpath carries **no git meaning and no build-source meaning** — the configure
source is the anchor's path. The subtree entry then contributes only its *name*
(as the possible `focus`) and its *repo-level presets*. That is intended.

---

## 5. Build configuration layering

Configuration is assembled in one strictly single-directional pass, then handed
to a backend emit step. Nothing is *accumulated and then re-asserted* — the fixed
order guarantees no layer ever needs revisiting.

### 5.1 The pipeline

```
Selection (always) — produces names + paths, no build side effects
  1. resolve config-root → load Workspace → locate the Project (by name/org, or by path)
  2. resolve the Profile axes: build_type / toolchain(name) / generator / branch / presets
  3. resolve path templates that need only names: repo workdir, build_dir, install_dir

Accumulation (single-directional; each layer resolved as it merges)
  L0  toolchain injection        → environment / definitions      [skipped when trusting config, §5.3]
        the injector (a backend, via the ToolchainInjector seam) translates the
        toolchain's canonical fields (§5.4); the toolchain's own environment/
        definitions are passed through verbatim. A path-only resolve injects nothing.
  L0.5 org config                → merge the org's [environment]/[definitions] (§5.6)
  L1  project config             → merge [environment]/[definitions]/extra_*
  L2  presets                    → default_presets, then applies_when matches, then --preset (§5.5)
  L3  CLI extra args             → --extra-*-args / -Xscope,arg  (verbatim, highest priority)
  = final { environment, definitions, extra_config_args, extra_build_args, extra_install_args }

Emit (backend)
  backend.steps(mode)            → ordered [Step{ argv, cwd, env }]; definition→argv spelling is private
```

Because the order is single-directional and no later layer can rewrite a
toolchain's compiler identity (§5.2), **no layer is ever revisited** — there is no
context rebuild between layers and no re-assertion of the toolchain after presets.

Template resolution is not a literal single pass at the very end; each layer's
newly-added keys are resolved *as the layer merges*, against the immutable
context accumulated so far. Intra-layer self-reference (one `environment` entry
referencing another) is handled by the resolver's lazy recursion, not a separate
topological-sort pass (§6.1).

### 5.2 The toolchain hard constraint

**A toolchain's compiler identity (cc/cxx/rustc/linker/launcher) enters the
context as an immutable input; presets cannot overwrite it.** Changing the
compiler is done only by moving along the selection chain (§5.5), never by
patching from a preset. This is the constraint that makes the pipeline
single-directional: since a preset can never clobber the toolchain, there is
never a need to re-assert the toolchain after presets. (A user who *explicitly*
sets, say, `CMAKE_C_COMPILER` via `-Xconfig` is exercising §1.2 pass-through, not
re-asserting a toolchain — and their explicit value wins, as the highest layer.)

### 5.3 Selection vs injection, and trusting an existing config

Two things that are easy to conflate but must stay separate:

- **Selection** — the toolchain's *name* is always resolved, because path
  templates (`build_dir = ".../{{toolchain}}"`) depend on it.
- **Injection** — merging the toolchain's env/definitions into the build.

Injection (L0) is **skipped** in `auto`/`build-only` mode when the build
directory is already configured and no toolchain was explicitly requested — so
re-running `build` does not trigger a needless reconfigure. Selection always
happens; injection is conditional. In `config-only`/`reconfig` the toolchain is
always injected, and it can be swapped for that run with `--toolchain` or the
environment (§5.5) *without editing config*.

### 5.4 Single source of truth for compilers, realised by the backend

A compiler is declared **once**, in a toolchain, using a backend-agnostic
vocabulary aligned with meson's native file:

```
cc  cxx  rustc  ar  nm  ranlib  strip  linker  launcher      # binaries
c_flags  cxx_flags  link_flags                               # flags
```

The **backend translates** these canonical fields into its native form — this is
the *only* translation the tool performs (§1.2). Each canonical field maps to a
**universal environment variable** (`CC`, `CXX`, `AR`, …, `CFLAGS`, `RUSTC`,
which nearly every tool honours) **plus a backend-native definition** where one
exists (`CMAKE_C_COMPILER`, `CMAKE_AR`, …; meson is nearly 1:1 and can emit a
native file directly; cargo maps `rustc → RUSTC`, `launcher → RUSTC_WRAPPER`).
One `cc = "clang"` declaration is therefore correct under every backend, written
once. Because this mapping runs at **L0**, an explicit preset or CLI override of
the same key still wins.

Anything outside the canonical vocabulary (e.g. an exotic tool) goes in the
toolchain's `environment` block and is passed through untranslated.

### 5.5 Toolchains are *selected*, not rewritten — and there are no built-ins

Toolchains are **100% user-declared**; the tool ships **no built-in toolchain
definitions**. Overriding a toolchain means moving along a **selection chain**,
never patching a toolchain's internals per-project:

```
user config [toolchains]  →  project/repo's toolchain field  →  CLI --toolchain / env
```

Environment beats `--toolchain` (consistent with the codebase's "env is the
deliberate, ephemeral override" philosophy, §10). Per-project rewriting of a
toolchain's internals is not supported by design.

### 5.6 Org config — inherited unconditionally; org presets still need naming

An org may declare `[org.environment]` and `[org.definitions]`. Every project that
joins the org **inherits them unconditionally**, at L0.5 — below the project's own
tables, so a project overrides any inherited key by declaring it.

The rule this follows is deliberately the one that already governed a project, now
applied at both levels:

> A level's bare `environment` / `definitions` are that level's **unconditional
> contribution**; that level's `presets` are **named choices**.

That is the whole justification. `[project.environment]` was always applied
unconditionally and `[project.presets.*]` always had to be named; an org uses the
same section names for the same concepts, so behaving differently made it the one
exception in the model. Inheriting removes the exception rather than adding a
mechanism — note that no new config key was introduced for this.

An earlier revision made these a *referenceable palette* instead, on the reasoning
that silent inheritance makes builds unpredictable. That was reconsidered: the
unpredictability of a config system comes from values you cannot see, not from
values you did not spell twice, and requiring every project to re-export its org's
constants by hand produced exactly the N×M duplication the org concept exists to
remove. Being the lowest layer keeps it recoverable — nothing is ever forced on a
project that declares its own value. (What this does raise the price of is
*visibility*: the resolved config should be inspectable without running a build.)

The template namespace `org.environment.*` / `org.definitions.*` **remains**, and
is not redundant. It is the only way to bind an org value to a *differently named*
key, and the only way to reach one from a context that never runs the build
pipeline at all — notably `[repos.*.hooks]`, which `update`/`clone` resolve
against a Profile-free per-repo context. Removing it would break lifecycle hooks.

Two boundaries worth stating, both unchanged by this:

- **Org presets are still named choices** — via `default_presets`, an
  `applies_when` match, `--preset org/name`, or another preset's `extends`. A
  preset is a *bundle you select*, which is a different thing from what the org
  *is*.
- **Inheritance follows the single `project.org`**; there is no multi-org
  inheritance. Reaching another org stays the explicit, per-name `org/preset`
  form (§5.7), which is what LLVM-family projects already use.

### 5.7 Presets

- **Three levels with cross-level merge**: presets exist at **org → project →
  repo**, declared via `[org.presets.NAME]`, `[project.presets.NAME]`,
  `[repos.<focus>.presets.NAME]`. A referenced name is the *merge* of the
  same-named preset at each level. **Maps** (environment/definitions) merge by
  key with the **nearest (most specific) level winning**; **lists** (extra args)
  are **replaced by the nearest level** — the most specific level's list is the
  one that applies, exactly as a command line's nearest occurrence wins. There is
  no system/global level (toolchains are the system concern, §5.5).
- **Cross-org references**: a reference may be org-qualified as `<org>/<preset>`
  to pull from another org (invaluable for LLVM-family projects sharing an `llvm`
  org). Works in `extends`, `default_presets`, and `--preset`.
- **Inheritance**: `extends` is kept. The merged same-named definition is formed
  first; its `extends` are then resolved from that merged form.
- **Auto-application by structured match**: rather than arbitrary conditions, a
  preset declares `applies_when` over a fixed key set — `build_type`,
  `toolchain`, `os`, `arch`, `generator`. Keys are AND-ed; a key's value is a
  scalar (equality) or an array (membership/OR); comparison is **case-sensitive**.
  A match auto-applies the preset. A project may also list `default_presets` that
  always apply.

```toml
[project.presets.dev]
extends      = ["llvm/base", "debug"]
applies_when = { build_type = "debug", toolchain = ["clang", "clang-cl"] }
definitions  = { ASSERTS = true }

[project]
default_presets = ["warnings"]
```

The application order is `default_presets` → `applies_when` matches → `--preset`,
de-duplicated by name keeping the **last** position (so an explicitly-passed
preset moves late and thereby wins). CLI `-X`/`--extra-*-args` (L3) sit above all
of it. Auto-application is deliberately a *structured* match rather than an
arbitrary expression: predictable, and easy to validate ahead of time.

---

## 6. Templates, Profile, and path resolution

### 6.1 Template engine (minimal, `wits_util::template`)

Config is **TOML only**. The engine is a zero-domain `{{ }}` / `[[ ]]` evaluator,
reusable and unit-testable in isolation:

- **`{{ path.to.var }}`** — dotted lookup over the context (tables by key, arrays
  by integer index). A whole-string single placeholder returns the **typed** value
  (a list or int survives); an embedded placeholder is stringified. Resolution is
  lazy and recursive with memoisation and cycle detection, so one `environment`
  entry referencing another simply resolves on demand — no separate dependency-map
  or topological-sort pass is needed.
- **`[[ … ]]`** — a *minimal* numeric expression, kept for real needs like
  `LINK_JOBS = "[[ max(1, system.mem.gb // 4) ]]"`. Scope: `+ - * / // %`
  over int/float, comparisons, and the functions `min max int float str bool`.
  It is **not** a general expression language — no `**`/bitops, no `and`/`or`, no
  ternary, no arbitrary names, no list/dict literals. Arbitrary conditions are
  handled by `applies_when` (§5.6), not here.

Error semantics: every failure is a hard error — unknown path, cycle, type
mismatch, division by zero. The context is **always fully populated** (optional
values like `upstream` are filled with their fallback at assembly time), so a
missing path always means a real mistake, never a silent empty string.

### 6.2 Context variables

```
project.{ name, org, focus }
repo.*                     # the *current* repo (the focus repo in project scope;
                           #   the repo itself in a repo-scoped field such as a hook)
  { name, path, kind, main_branch, anchor, origin, upstream, mirrors }
repos.<name>.*             # any repo by explicit name; same fields as repo.*,
                           # plus its resolved workdir in a full plan (§3.4)
branch.{ raw, slug }       # raw = the branch name; slug = filesystem-sanitised
build_type
toolchain.{ name, cc, cxx, rustc, ar, nm, ranlib, strip, linker, launcher,
            c_flags, cxx_flags, link_flags }
generator
system.{ os, arch, memory.gb, cpu.count }
env.*                      # process environment
spec.*                     # CLI-registered vars (--spec K=V); purely
                           #   referenceable — supplied on the command line,
                           #   required if a template names it, never applied alone
```

`repo` is a **relative** alias for *the repo currently being resolved*, never a
synonym for a fixed repo; cross-references always use `repos.<name>`. There is no
bare `{{branch}}` — a template must pick `.raw` or `.slug`, so there is never
ambiguity between the raw name and its sanitised form.

### 6.3 Profile vs BuildOptions

The axes that affect *resolution* are separated from the options that affect only
*command steps* — a distinction worth keeping crisp:

```rust
pub struct Profile {          // affects identity / build_dir / repo workdir resolution
    build_type: Option<String>,   // None → the config default ("debug")
    toolchain:  Option<String>,   // None → selection chain (§5.5)
    generator:  Option<String>,
    branch:     Option<String>,   // None → the focus repo's current branch
    presets:    Vec<String>,
    focus:      Option<String>,   // --focus override (§4.1)
    work_dir:   Option<PathBuf>,  // --work-dir: use this checkout verbatim (§6.5)
    specs:      Map<String,String>,  // --spec K=V → the spec.* namespace (§6.5)
}

pub struct BuildOptions {     // affects command steps only
    mode:    BuildMode,           // auto | config-only | build-only | reconfig | uninstall
    install: bool,
    install_dir: Option<PathBuf>, // --install-dir: override the resolved prefix
    build_dir:   Option<PathBuf>, // --build-dir: override the resolved build dir
    target:  Option<String>,
    extra_config_args:  Vec<String>,
    extra_build_args:   Vec<String>,
    extra_install_args: Vec<String>,
}
```

`info --branch X`, a hook resolving a dir for a deleted branch, and `build` all
share the same `Profile` to resolve paths; `BuildOptions` appears only when a
build actually executes.

### 6.5 The CLI override layer and the `review` interaction

Three command-line overrides sit **above** the config's declared paths, at the
highest priority (§5.5), and exist so a checkout materialised *outside* the
project's own strategy can be built through the project machinery:

- **`--work-dir <DIR>`** (a `Profile` axis) uses that directory verbatim as
  the build repo's `repos.<name>.workdir`, skipping the `worktree_dir`/in-place
  resolution of §3.4. Because named templates anchor on that repo's `workdir`,
  `build_dir`/`source_dir`/`install_dir` follow the override with no other
  change. `build` also reads it
  as "the caller owns the checkout": it neither requires the strategy's worktree
  to exist nor runs the in-place branch dance.
- **`--build-dir` / `--install-dir`** (`BuildOptions`) override the two resolved
  output paths directly, ignoring their templates.
- **`--spec K=V`** (a `Profile` axis) registers `spec.*` template variables
  (§6.2). A project that opts in by writing `{{spec.mr}}` *requires* the caller
  to supply it — a hard error otherwise — so an out-of-band identity (an MR
  number, a variant tag) enters resolution without being baked into the file or
  guessed. Unlike an org's tables (§5.6) it is never applied as a layer of its
  own — it exists purely to be named.

This is deliberately how **`project` and `review` interact — at the workflow
level, with no code dependency either way** (consistent with `review` never
owning code/branches, and with `project`'s core being a thing other tools
*consume*, §1.4). A reviewer runs `review checkout <mr>` to materialise an MR's
snapshot into a worktree, then `build <proj> --work-dir <that worktree> --spec
mr=<n>` to build it — the two commands meet only at a path and a spec value the
user passes, never in code. For a submodule-of-a-monorepo MR the same seam
serves both shapes: an independently-built component (`anchor = self`) points
`--work-dir` at the review worktree, while one that must build via its root is
checked out in place and disambiguated by a `spec.*`-keyed `build_dir`. Moving
`build_dir`/`install_dir` onto the repo (so the build base owns
`work`/`source`/`build`/`install` together) is a coherent future shape but was
**not** taken: `review` needs an *override*, not a per-repo *default*, and the
override layer is the smaller, one-directional change (§1.2).

### 6.4 Branch identity

Identity **is the branch name, and only that.** The richer five-layer waterfall
(ref-tip → Change-Id → slug → hash) sometimes proposed for stacked diffs is
deliberately **out of scope**: even under stacked-diffs, driving from one stable
branch
gives the whole experience and stays compatible with existing scripts, at a
fraction of the complexity. A **detached HEAD is not supported** — `build`
requires a branch (its own, or `--branch`). `branch.slug` sanitises the name by
replacing every character outside `[A-Za-z0-9._-]` (including `/`) with `_`.

---

## 7. `update` / `clone` semantics

Fusing "update the repo" with "prepare to build a specific branch" is what
produces a tangled root/component switch-stash-restore dance, so the two are kept
separate: **`update` never switches branches**. It fast-forwards the existing
main checkout when one exists (the main linked worktree for a bare-backed repo),
otherwise only the ref, and treats every repo uniformly because a submodule is
just a nested Repo (§2).

```
update(project):
  for repo in repos, parents before nested (subtree contributes no git work,
                                            and a `from` borrow is its owner's
                                            unless --with-borrowed, §3.6):
    if repo.path is missing → clone lifecycle:
        clone action:
          in-place: git clone + remotes + sparse-before-checkout
          worktree/hybrid: init --bare + remote add + fetch + main_branch from
                           <sync>/<main_branch>, then bootstrap main worktree
          override: current cwd; owns repository + bootstrap creation
        initialise submodules in the checkout
        apply `skip`: deinit covered submodules, write patterns, verify
        post_clone (cwd = checkout), then verify `skip` again
    else → verify `skip` in the checkout → ensure-remotes → pre → action → post
           (bare-backed hooks use the main worktree, or bare path if absent)

  where sync = upstream if declared, else origin (§3.1)

default update action:
  conventional clone:
    if currently on main_branch: git fetch <sync>; merge --ff-only <sync>/<main>
    else: git fetch <sync> <main>:<main>                 # ref-only fast-forward
  bare-backed:
    git fetch <sync>                                     # tracking refs for all
    if a worktree holds main: merge --ff-only <sync>/<main> there
    else:                     update-ref refs/heads/<main> <sync>/<main>, ff-only
  then advance declared submodule repos (their own lifecycle), and refresh
       undeclared nested submodules with: git submodule update --recursive -- <materialised paths>
```

The pivotal simplification is the **no-switch default**. A conventional feature
checkout advances the main ref without checking it out. A bare-backed repo
updates the worktree already holding main; if none remains, it advances only the
bare ref. No path is ever repointed just to update.

**Sparse-checkout is safe by design.** A refspec fetch updates refs and objects
without ever expanding the sparse cone; an `--ff-only` merge honours the cone; the
submodule refresh is limited to explicitly-passed, already-materialised paths and
never uses `--init`. `--init` appears only on a *fresh working-tree event* —
clone, or worktree creation (§8.3) — never on update. This is also why a
`skip`ped submodule needs no mention in the refresh: it is not on disk, and the
refresh only ever passes paths that are.

**A contradicted `skip` stops the repo before anything else happens** (§3.7).
Refreshing a checkout whose declared mask is not in force would only entrench
whichever copy of a shared component should not be there, so the verification
runs first and fails.

**Failure is fail-fast with guaranteed restoration.** A hook or action exiting
non-zero stops the operation immediately, the RAII guard returns the repo to its
original branch (and pops any stash), a log line is written, remaining repos are
skipped, and the program exits non-zero.

---

## 8. `build`, backends, and build contexts

### 8.1 The backend abstraction — the only extension axis

A new build system is a new `Backend` impl plus registration. Backends live
in `wits_util::build_system` (§1.4), never in the core; the core and the resolver
name no concrete backend. The abstraction is split so the core depends only on
the half it needs — the L0 toolchain translation — via the core-owned
`ToolchainInjector` seam:

```rust
// in wits_util::project::resolve (core): the only backend-facing thing the pipeline sees
trait ToolchainInjector {
    fn apply_toolchain(&self, tc: &Toolchain, cfg: &mut LogicalConfig);   // L0
}

// in wits_util::build_system: the full build-time abstraction, a ToolchainInjector plus emission
trait Backend: ToolchainInjector {
    fn name(&self) -> &str;                        // "cmake" | "meson" | "cargo"
    fn steps(&self, ctx: &EmitContext) -> anyhow::Result<Vec<Step>>;
    fn is_configured(&self, build_dir: &Path) -> bool;
}
```

`apply_toolchain` runs at **L0** (so overrides can win, §5.4) and is the *only*
backend method the core invokes — through the seam, given the concrete backend
by `build`. `steps` runs at emit and owns the definition→argv spelling (cmake's
`-DK:TYPE=V` vs meson's `-Dk=v`), the command sequence per `BuildMode`, and
`is_configured` detection (cmake's `CMakeCache.txt`, meson's `coredata.dat`,
none for cargo) — none of which the core ever sees.

Whether a declared `build_system` actually *has* a backend is reported by
`wits build` at run time, not by `wits project --check`: the core does not know the
set of supported build systems, so `--check` validates only declared-fact
consistency (e.g. a toolchain's `supports` list vs the `build_system`).

### 8.2 Modes, install, and uninstall

`BuildMode` is `auto | config-only | build-only | reconfig | uninstall`.
`--install` adds an install step to a build. `uninstall` is its own mode because
install is **not** a plain `rm`: an install dir can equal the build dir or be a
shared prefix like `$HOME/.local` mixed with other projects. Uninstall is
therefore backend-driven — meson `ninja -C <build> uninstall`, cmake via
`install_manifest.txt`, cargo unsupported — never a recursive delete.

### 8.3 Build contexts — and why `project` no longer manages them

A branch's **build context** is its physical build space: a worktree plus a
build dir (worktree/hybrid), or just a build dir (in-place). `project` resolves
*where* that space is (§5.5) and stops there. Worktree resolves the declared
path; hybrid first discovers the branch's actual checkout and falls back to the
declared path as a suggestion. `build` requires either checkout to exist and
never creates one implicitly.

**Removed: `project context {create,prune}`.** It once created a branch's worktree
and tore the pair down again, strategy-transparently. Two things retired it:

- Worktree management is not project-shaped. `wits worktree` does the same job for
  **any** repository — registered or not, bare or a submodule — which is strictly
  more useful, and it is where the interesting behaviour now lives (submodule
  object borrowing, reclaim predicates, `switch`). Keeping a second, registry-only
  implementation meant two code paths for one act.
- Its sparse-checkout handling had become dead weight. It added the worktree
  `--no-checkout`, replicated the patterns, then checked out — a dance that
  predates git 2.36, which copies the pattern file itself, before the checkout,
  with or without `--no-checkout`. The replication could only be lossier than
  git's own copy.

The `--work-dir` override (§6.5) is what remains of the seam, and it is enough:
`project work-dir` returns the deterministic path (worktree) or the discovered
path/fallback suggestion (hybrid), `wits worktree create` makes one anywhere,
and `build --work-dir` builds from whatever you hand it. The two components meet
at a path and share no code.

What genuinely went away is the **build-dir teardown** — nothing else deletes a
branch's `build_dir`, because `wits worktree` is project-agnostic by design and
knows nothing about build dirs. `project build-dir` prints the path to delete.
Install prefixes were never in scope either way (§8.2); install reversal is
`build --uninstall`.

---

## 9. Crate API and CLI contract

### 9.1 The crate API (read-only, consumer-driven)

The core is a read-only library other tools query. It **resolves** paths but
never destroys them — the only side-effecting entry points are the `build` and
`update` actions.

```rust
let ws = Workspace::load(config_root)?;
ws.projects();                        // iterate all
ws.project("mesa", org)?;             // by name / org
ws.project_for_path(&path);           // reverse lookup: which project owns this checkout?

let p = ws.project(...)?;
p.repos();  p.build_repo();           // all repos / the focus repo
p.main_branch();                      // e.g. wits stack's base-branch resolution
p.git_state();                        // branch, commit, dirty, submodules (read-only)
p.work_dir(&profile);                 // resolved build repo workdir
p.build_dir(&profile);  p.install_dir(&profile);
p.resolve("{{ ... }}", &profile);     // expose the resolver to callers
p.validate();                         // structural issues (info --check)
```

Two real consumers shaped this surface: **`wits stack`** needs
`project_for_path → main_branch` to resolve its base branch from a checkout's
path (see `docs/stack/design.md` §5.1); **git hooks** need `build_dir`/`work_dir`
resolution for an arbitrary branch to clean up after a deleted branch (§8.3).

### 9.2 CLI contract

- **`project [<name>]`** — the read command. No name lists a summary of every
  project; a name gives details. With `Profile` flags it shows resolved
  build/install/work dirs; without them it shows the raw templates. It also lists
  a project's worktrees and their resolved dirs. Pure read.
- **`project --check [<name>]`** — config-legality validation (required fields,
  valid build system, preset/inheritance cycles, template resolvability, toolchain
  references exist). No name checks everything (CI use). A `--check` mode of the
  read command rather than a separate verb.
- **`build <name>`** — resolves a full `Profile`, runs the §5 pipeline and the
  backend emit, honouring the focus repo's branch strategy.
- **`update [<name>]`** — the §7 lifecycle over all of the project's repos; no
  name updates every project. `--with-borrowed` includes repos whose owner is
  another project (§3.6), which are otherwise left to it.

Every verb's positional is a **name or a path**, mutually exclusive. A token that
is `.`/`..` or begins with `.`, `/`, or `~` is a **path** — it may point *inside*
a checkout, and the owning project is found by deepest-prefix match
(`project_for_path`, §9.1); anything else is a **name**, bare or fully-qualified
`org/name`. A bare name ambiguous across orgs is a hard error asking for
qualification; there is no `--org` filter. When the positional is omitted, `info`
covers every project while `build`/`update` operate on the project
owning the current directory (a hard error if none does). (Shells expand `~` and
leave `./` literal, so in practice the classifier keys on a leading `.` or `/`.)

---

## 10. Configuration topology

Configuration is **content-addressed, not path-addressed**: files may live
anywhere under one config-root and declare what they are via their TOML sections.

### 10.1 Config-root resolution

Exactly one config-root, resolved in priority order:

```
$WITS_PROJECT_CONFIG (env)  >  $XDG_CONFIG_HOME/wits/project  >  $HOME/.wits/project
```

A fixed user location (rather than a `$PWD`-relative one) is deliberate: `wits
stack`'s path reverse-lookup and hook-driven cleanup must find *the same* project
registry regardless of the current directory. Environment is the single explicit
override, consistent with §5.5.

### 10.2 One project per file; registries merge

- A file containing `[project]` (with a required `repos.main`) **is one project**.
  The same `(org, name)` appearing in two files is a **hard error**, not a silent
  override — cross-file layering of a single project would be a confusion source.
- `[toolchains.*]` and `[org]` + `[org.presets.*]` are **additive registries**
  that freely merge across the whole tree.
- The root is scanned recursively for `*.toml` **eagerly** at `Workspace::load`;
  every file is loaded and its top-level sections routed. A single file may mix
  sections.

### 10.3 `org` is always explicit

An org is declared with `[org] name = "…"` and joined with `project.org`. It is
**never inferred from the file path**, because placement is arbitrary.

---

## 11. Naming

The tool's configuration and environment namespace is **`wits`**: config lives
under `wits/project`, environment overrides are `WITS_*`. (The umbrella binary
name is a separate, out-of-scope concern.)

---

## 12. Open questions / future

- **[open]** Output *format* of `info` (`--json` rejected as the answer; the exact
  human/scriptable format is to be designed).
- **future** Which backends ship in v1 (cmake / meson / cargo are confirmed real;
  bazel / make pending a real need).
- **future** The finer points of submodules inside worktrees beyond the v1 rule
  in §8.3.
- **TODO** Whether a *nested focus* (a `focus` whose `anchor` is not itself)
  should exist at all — it may be simplified away. For now the focus/anchor roles
  are defined (§4) and a nested focus is always switched **in-place**.
- **decided** The effective checkout is exposed on the named build repo as
  `{{repos.<name>.workdir}}`; there is no top-level `work.dir`. `branch_strategy`
  stays the **build repo's** alone (§3.4). A nested or borrowed focus therefore
  still has `{{repos.<focus>.path}}` as its repository path, while its effective
  checkout is `{{repos.<focus>.workdir}}` and the build source is the explicitly
  named anchor's `workdir`. Configuration that
  changes focus/anchor must name the corresponding repo in its path templates;
  the resolver deliberately does not add dynamic map-key lookup.
- **open** `skip` is applied only where wits builds the checkout. Anything cloned
  another way — an overriding `clone` hook that clones elsewhere, or your own `git
  clone` — has no mask until you apply it, and the first `update` says so. Whether
  that should stay a manual step (the current answer, consistent with §1.2) or earn
  an explicit apply verb is unsettled.
- **out of scope** The five-layer branch identity (§6.4) and cross-project
  dependencies (§1.5) — recorded here so they are not re-proposed by reflex.
  `from` (§3.6) is deliberately *not* an exception to the latter: it borrows a
  repo's identity, never triggers another project's build.
