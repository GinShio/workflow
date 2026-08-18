# `wits project`

Build, update, and introspect the source projects you work on, from one
declarative registry that knows *what each project is* — where its repos live,
which branches, which toolchain, how to build it — and drives cmake / meson /
cargo on your behalf without you re-typing the same flags.

Three commands share that registry: **`wits project`** describes and validates
(read-only), **`wits build`** configures and builds, and **`wits update`** refreshes
git. Per-branch worktrees are [`wits worktree`](worktree.md)'s, which is
project-agnostic. This page is the **usage guide** for the three.

For
the exhaustive list of every config key and every flag, see
[`project/reference.md`](project/reference.md). For *why* the tool is shaped this
way, see [`project/design.md`](project/design.md).

> Status: implemented (v1). This guide describes the behaviour the tool provides.

---

## The mental model in one minute

- A **project** is a buildable unit described by one TOML file. It owns one or
  more **repos** (git checkouts); one of them, `repos.main`, is always required.
  A repo may instead be **borrowed** from another project with `from`, which is
  how a component several projects consume gets a single home.
- A **toolchain** is a named set of compilers/tools you declare once and select
  per build. The tool ships no built-in toolchains — you declare your own.
- A **preset** is a reusable bundle of build settings you can layer on.
- A **build context** is where a branch actually builds: either a git *worktree*
  or an in-place build directory, your choice per repo.
- An **org** is a project's shared parent. Its `[org.environment]` /
  `[org.definitions]` are inherited unconditionally by every project that joins
  it; its `[org.presets.*]` apply only when named.

Everything is content-addressed: config files can live anywhere under the config
root and declare what they are by their sections. There is no required layout.

`repos.main.path` (and any repo's `path`) is a template resolved against
`project.name`, `project.org`, `env.*`, and `system.*` — so paths like
`~/src/{{project.org}}/{{project.name}}` work and remain answerable without a
full build profile.

---

## Where config lives

`project` reads one config root, resolved in this order:

1. `$WITS_PROJECT_CONFIG` (environment)
2. `$XDG_CONFIG_HOME/wits/project`
3. `$HOME/.wits/project`

Drop `*.toml` files anywhere under it. A file with a `[project]` section is a
project; files with `[toolchains.*]` or `[org]` sections contribute to shared
registries and are merged across the whole tree.

---

## Your first project

Create `~/.wits/project/hello.toml`:

```toml
[project]
build_system = "cmake"
toolchain    = "clang"

[repos.main]
path        = "~/src/hello"
main_branch = "main"
[repos.main.remotes]
origin = "git@github.com:me/hello.git"

# Where the build lands. `repos.main.workdir` is this build's checkout dir;
# keying by branch means switching branches never clobbers another build.
build_dir   = "{{repos.main.workdir}}/_build/{{toolchain.name}}/{{build_type}}"
install_dir = "{{repos.main.workdir}}/_install/{{build_type}}"
```

Declare the `clang` toolchain once (e.g. `~/.wits/project/toolchains.toml`):

```toml
[toolchains.clang]
cc       = "clang"
cxx      = "clang++"
ar       = "llvm-ar"
linker   = "mold"
launcher = "ccache"
supports = ["cmake", "meson"]
```

Now:

```sh
update  hello      # clone if missing, otherwise refresh git
build   hello      # configure + build with clang, debug by default
project hello      # what is it, where does it build, what branch is it on
```

`build` translates the toolchain into cmake's native flags for you — you never
write `CMAKE_C_COMPILER` yourself.

---

## Building: types, toolchains, presets, modes

```sh
build hello -B release                 # build type (lowercase, meson-aligned)
build hello -T gcc                     # a different declared toolchain
build hello -p asan -p lto             # apply presets (repeatable)
build hello --config-only              # (re)configure, don't compile
build hello --build-only               # compile, assume already configured
build hello --reconfig                 # wipe the build dir and configure fresh
build hello --install                  # add an install step
build hello --uninstall                # reverse an install (backend-driven)
```

Pass raw, untouched flags straight through to the underlying tool when you need
something one-off — these are applied last, at the highest priority:

```sh
build hello --extra-config-args -DFOO=BAR --extra-config-args -DBAZ=1
build hello -Xconfig,-DFOO=BAR         # short form, scope = config|build|install
build hello -Xbuild,-j8
```

The tool does not interpret these — `-DFOO=BAR` is handed to cmake verbatim.

### Choosing a toolchain without editing config

Selection order is `env → --toolchain → the project's toolchain field →
[toolchains]`. Environment wins, so you can flip toolchain for a run:

```sh
WITS_PROJECT_TOOLCHAIN=gcc build hello
```

(Selecting a toolchain always happens so paths like `.../{{toolchain.name}}/...`
resolve; in `auto`/`build-only` mode an already-configured build dir is trusted
and not reconfigured just because you re-ran `build`.)

---

## Presets

A preset bundles environment, definitions, and extra args, and can inherit and
auto-apply itself:

```toml
[project.presets.debug]
definitions = { ENABLE_ASSERTS = true, ENABLE_TESTS = true }

[project.presets.asan]
extends      = ["debug"]
applies_when = { build_type = "debug", toolchain = ["clang", "clang-cl"] }
environment  = { ASAN_OPTIONS = "detect_leaks=1" }
definitions  = { SANITIZER = "address" }

[project]
default_presets = ["warnings"]     # always applied
```

- `default_presets` always apply; an `applies_when` match auto-applies; `-p NAME`
  applies explicitly. Explicit wins.
- Presets exist at three levels — `[org.presets.*]`, `[project.presets.*]`,
  `[repos.<focus>.presets.*]` — and a name is the merge of all three, most
  specific winning. Reach another org with `-p llvm/base`.

See the reference for the exact merge and match rules.

---

## Multiple repos: monorepos, submodules, subtrees

A project can own several repos. `repos.main` is the required root; others hang
off it and pick a `focus` for building.

```toml
[project]
focus        = "lvp"           # build the lavapipe component
build_system = "meson"
toolchain    = "clang"

[repos.main]                   # the mesa clone (required root)
path        = "~/src/mesa"
main_branch = "main"
[repos.main.remotes]
origin   = "git@github.com:me/mesa.git"
upstream = "https://gitlab.freedesktop.org/mesa/mesa.git"

[repos.lvp]                    # a subtree (inferred: nested path, no main_branch)
path   = "src/gallium/frontends/lavapipe"   # relative to repos.main → shares mesa's git
anchor = "main"                # build via the mesa root

build_dir = "{{repos.main.workdir}}/_build/lvp/{{build_type}}"
```

- `anchor = "main"` means "build from the mesa root" — the configure source is
  mesa, and lavapipe is selected through meson options. `anchor` may point at any
  repo, or be left unset to build a repo on its own.
- The **kind** of each repo is inferred, never declared: a nested path with its
  own `main_branch` is a **submodule**; a nested path without one is a **subtree**;
  a non-nested path is **standalone**. A submodule is cloned through `repos.main`.
- `update` refreshes *every* repo; `build` builds the `focus`; you can switch
  focus for one run with `--focus <repo>` — handy in a large monorepo.
- If a repo's top-level `CMakeLists.txt`/`meson.build` is not at the checkout
  root, point `source_dir` at it (a template, default
  `{{repos.main.workdir}}`):
  `source_dir = "{{repos.main.workdir}}/src"`. Only the configure source moves;
  `repos.main.workdir`, `build_dir`, and the branch still key off the checkout.

---

## One component, several projects

Sooner or later a component is a submodule of *more than one* project, at a
different path in each. Declared the obvious way that means the same repo
described in every project file, cloned once per project, and the same feature
branch created over and over. Two keys collapse it: **`from`** declares the
component once where it lives, and **`skip`** keeps each consumer's own copy out
of the way.

```toml
# engine.toml — the component. A project of its own: one checkout, one update.
[project]
build_system = "cmake"

[repos.main]
path        = "~/src/engine"
main_branch = "stable"
[repos.main.remotes]
origin = "git@github.com:me/engine.git"
```

```toml
# viewer.toml — a project that consumes it.
[project]
focus     = "engine"          # the component is what you are working on
build_dir = "{{repos.main.workdir}}/_build/{{branch.slug}}-{{build_type}}"

[repos.main]
path        = "~/src/viewer"
main_branch = "main"
skip        = ["/third_party/engine"]   # don't check out our own copy

[repos.engine]
from   = "engine"             # …use that one instead
anchor = "main"               # and build it through the viewer root

# Point the build at the borrowed checkout. `{{repos.<name>.workdir}}` is the
# branch-specific working path — the definition name is your build system's,
# not ours.
[project.definitions]
ENGINE_SOURCE_DIR = "{{repos.engine.workdir}}"
```

`build viewer` now builds *your* engine checkout through the viewer, with the
build directory keyed by the engine's branch — and a second consumer (`editor`,
say) is another file with its own `skip` and its own definitions, not a second
copy of the engine.

### `from`

`from = "[<org>/]<project>[:<repo>]"` (repo defaults to `main`) makes the entry
*be* that project's repo. The rule for what comes along is one sentence: **a
repo's git identity travels; how this build uses it does not.** So `path`,
`main_branch`, `remotes`, `hooks`, `branch_strategy`, `worktree_dir`,
`bootstrap_worktree_dir`, and `skip` come from the source — declaring one of
them here too is an error — while `anchor`, `source_dir`, and `presets` are
yours, which is what lets each consumer set its own build knobs on the same
component.

Two things follow that are worth knowing before you hit them:

- **The owner answers for the path.** `cd ~/src/engine && project` gives you
  `engine`, never a project that merely borrows it. Borrowed entries are not
  candidates for the path lookup, so a shared checkout has exactly one owner.
- **`update` leaves borrowed repos alone**, so the component is fetched once by
  its owner rather than once per consumer. `update viewer --with-borrowed` opts in.

`from` can also name a nested repo — `from = "viewer:engine"` — which says "the
component lives inside *that* project's tree". Useful when you would rather not
move it out at all.

### `skip`

`skip` lists the paths a checkout never materialises, as ordered gitignore-style
patterns where `!` re-includes and the last match wins:

```toml
skip = ["/third_party/engine", "/vendor", "!/vendor/keep.c"]
```

It is not only for borrowing — a monorepo you build one component of is the same
need. It is realised as a non-cone sparse-checkout, plus `git submodule deinit`
for any submodule it covers (sparse alone cannot remove a materialised submodule).

**`clone` applies it; everything else only checks it.** An in-place clone writes
the patterns before its first checkout. A worktree/hybrid clone first creates a
bare repository and its bootstrap worktree, initialises submodules there, then
deinitialises skipped submodules and applies the sparse mask. Later `wits
worktree create` calls are driven from that bootstrap checkout, so Git copies
the mask. But `update` and `project --check` only *verify* and fail — applying a
mask to a tree wits did not build means deleting content, which is yours to do:

```sh
update viewer
# [ERROR] repo 'main': skipped path 'third_party/engine' is materialised …
# [INFO] re-run with -v to see the commands that fix this

update viewer -v          # prints the exact deinit + sparse-checkout commands
```

Your own sparse patterns are safe in a different sense: the check asks whether
anything the list excludes is still materialised, not whether the pattern file
matches what wits would have written. But wits will **refuse** to *write* over
patterns it did not put there — `sparse-checkout set` replaces the whole list, so
a checkout something else narrowed to a sparse cone would be widened rather than
masked. If a `clone` hook establishes its own sparse cone, fold the exclusions
into it there instead of declaring `skip`.

---

## Branches: in-place, worktree, and hybrid

Each repo picks how multi-branch work is realised:

```toml
[repos.main]
# Choose one: "in-place" (default), "worktree", or "hybrid".
branch_strategy        = "hybrid"
worktree_dir           = "{{repo.path}}.worktrees/{{branch.slug}}"
bootstrap_worktree_dir = "{{repo.path}}.primary"
```

- **in-place**: `build --branch X` stashes, switches to X, builds, then always
  switches back and restores your working tree — even if the build fails.
- **worktree**: `path` is a bare clone and each branch resolves deterministically
  to `worktree_dir`. `bootstrap_worktree_dir` is optional; when absent, the
  initial main checkout is `worktree_dir` rendered for `main_branch`.
- **hybrid**: also bare-backed, but first asks Git whether the branch is already
  checked out anywhere and uses that actual path. If not, `worktree_dir` is the
  suggested location. Its fixed `bootstrap_worktree_dir` is required and may not
  reference `branch.*`. A relative bootstrap value such as `"main"` is resolved
  beside `worktree_dir(main_branch)`, never against the command's current
  directory.

Both worktree modes require `worktree_dir`. `build` requires the target
worktree to exist and never creates one implicitly:

```sh
wits worktree create feature-x "$(project work-dir hello --branch feature-x)"
build hello --branch feature-x                    # build in it
wits worktree prune feature-x                     # reclaim it when done
```

Creating and reclaiming worktrees belongs to [`wits worktree`](worktree.md), which
does it for **any** repository rather than only a registered one. `project` and
worktrees meet at a path and nowhere else: ask `project work-dir` where the
strategy says the checkout goes, or skip the registry entirely and point
`build --work-dir` at a worktree you made yourself.

On a fresh `update`, worktree/hybrid build a bare repository that **tracks** its
remote (`init --bare` + `remote add` + `fetch`, so the remote's branches stay in
`refs/remotes/origin/*` rather than being copied into `refs/heads`), create the
main bootstrap checkout, initialise its submodules, and apply `skip` there.
Existing conventional clones are not converted.

> Removing a worktree does not remove its **build directory**; `wits worktree` is
> project-agnostic and knows nothing about build dirs. Delete it yourself when you
> want the space back — `project build-dir hello --branch feature-x` prints the
> path. (`project context prune`, which used to do both, is gone.)

---

## Updating

```sh
update hello                    # one project's repos
update                          # every project
update hello --with-borrowed    # include repos owned by another project
```

`update` is safe by default: if you are on a feature branch, it fast-forwards the
main branch's ref *without* checking it out — nothing is stashed or switched, and
a sparse checkout is never expanded. It also ensures your declared remotes exist
(adding missing ones and mirror push-URLs) but never rewrites URLs you set
yourself. Repos borrowed with `from` are left to the project that owns them unless
you ask for them, and a repo whose declared `skip` is not in force is a hard error
rather than a refresh.

For a bare-backed repo, `update` fast-forwards the linked worktree currently
holding `main_branch`. If that worktree was removed, it advances the bare branch
ref directly and touches no working tree; nested repo lifecycle work waits until
a main worktree exists again.

---

## Inspecting and validating

```sh
project                       # one-line summary of every project
project hello                 # details for one
project hello -b feature-x -B release   # resolved build/install/work dirs for that profile
project --check               # validate every project's config (CI)
project --check hello         # validate one
```

`info` is pure read — it never builds or switches anything.

---

## Running from inside a checkout

Every verb takes a project **name** or a **path**, or nothing at all. A path may
point *inside* a checkout — the owning project is found automatically — and with
no argument the verbs act on the project you are currently standing in:

```sh
cd ~/src/mesa/src/gallium/frontends/lavapipe
build                # builds the project owning this directory
project .            # details for that project
project ~/src/hello  # by path, from anywhere
```

A token starting with `.`, `/`, or `~` is treated as a path; anything else is a
name (`hello` or `mesa/lavapipe`). With no argument, `info` lists every project
while the other verbs use the current directory.

## Global flags

Inherited from `wits` (see the top-level README):

| Flag | Meaning |
|---|---|
| `-v`, `--verbose` | Show the underlying git / build commands as they run |
| `-n`, `--dry-run` | Print mutating commands instead of running them (reads still run) |

---

## Where to go next

- [`project/reference.md`](project/reference.md) — every config key and every CLI
  flag, precisely.
- [`project/design.md`](project/design.md) — the rationale behind every decision
  here.
