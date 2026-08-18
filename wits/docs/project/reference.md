# `wits project` — Reference

The exhaustive reference: every configuration key, every CLI flag, the template
language, and the resolution rules. For a gentle introduction read
[`../project.md`](../project.md); for rationale read [`design.md`](design.md).

> Status: implemented (v1). This documents the contract the code upholds.

---

## 1. CLI

```
project [<name|path>] [--check] [--focus <repo>] [profile flags]
build   [<name|path>] [--focus <repo>] [profile flags] [build options]
update  [<name|path>] [--with-borrowed]

# --with-borrowed also refreshes repos declared with `from` (§9.4), which update
# otherwise leaves to the project that owns them.

# Worktrees are not `project`'s: `wits worktree` creates and reclaims them for any
# repository (see docs/worktree.md). The two meet only at a path — `project
# work-dir` resolves/discovers one, `build --work-dir` accepts any.

# Profile flags include --work-dir <DIR> and --spec K=V (see §1.2); build adds
# --build-dir <DIR> (§1.3). Together they let a checkout materialised elsewhere
# (e.g. a `review checkout` worktree) be built through the project machinery.

# Machine-readable queries for scripts and git hooks.
project exists       <name>
project main-branch [<name|path>]
project build-dir   [<name|path>] [--branch <X>]
project install-dir [<name|path>] [--branch <X>]
project source-dir  [<name|path>] [--branch <X>]
project work-dir    [<name|path>] [--branch <X>]
```

`exists` resolves a bare or fully-qualified name, then succeeds only when
`repos.main.path` is the root of a Git working-tree checkout or bare clone. A
registered but un-cloned project returns a non-zero status; missing and
ambiguous names remain lookup errors.

The four `*-dir` queries resolve the same build [plan](#5-resolution-pipeline)
as `build`/`info` and print one of its paths — `build_dir`, `install_dir`,
`source_dir`, or the selected repo's `workdir` respectively (`build-dir`/`install-dir` error when
the project declares no such template; `source-dir`/`work-dir` are always
resolvable). The branch defaults to the anchored repo's current one. This is how
a checkout hook points `compile_commands.json` at the active build, or a script
`cd`s into a branch's `repos.<name>.workdir`.

Each verb's positional is a **name or a path**, mutually exclusive:

- **path** if the token is `.`/`..` or begins with `.`, `/`, or `~`. It may point
  *inside* a checkout; the owning project is found by deepest-prefix match
  (`project_for_path`). Shells expand `~` and leave `./` literal, so in practice
  the classifier keys on a leading `.` or `/`.
- **name** otherwise — a bare name or a fully-qualified `org/name`. A bare name
  ambiguous across organisations is a hard error asking you to qualify it. There
  is no `--org` flag.
- **omitted**: `info` covers every project; `build`/`update` operate on the
  project owning the current directory (a hard error if none does).

### 1.1 Global flags

`-v/--verbose` and `-n/--dry-run` are inherited from the `wits` process layer and
described in [`../project.md`](../project.md) and the top-level README.

### 1.2 Profile flags (affect resolution — build & info)

These set the `Profile` axes and therefore change how paths
(`repos.<name>.workdir`,
`build_dir`, `install_dir`) resolve.

| Flag | Alias | Meaning | Default |
|---|---|---|---|
| `--branch <X>` | `-b` | Target branch (the build identity). | focus repo's current branch |
| `--build-type <T>` | `-B` | Build type (`debug`, `release`, `debugoptimized`, …). | the config's default |
| `--toolchain <N>` | `-T` | Select a declared toolchain. | selection chain (§5) |
| `--generator <G>` | `-G` | Build-system generator (e.g. `Ninja`). | the project's `generator` |
| `--preset <P>` | `-p` | Apply a preset; repeatable; accepts `org/preset`. | — |
| `--focus <repo>` | | Override which repo is the build focus. | `project.focus` |
| `--work-dir <DIR>` | | Use this checkout verbatim as the selected repo's `repos.<name>.workdir`, bypassing the branch strategy's `worktree_dir`/in-place resolution. Everything (`build_dir`/`source_dir`/…) still anchors on it. The seam for building a checkout materialised elsewhere (a `review checkout` worktree). | strategy-resolved |
| `--spec <K=V>` | | Register a template variable, exposed as `{{spec.K}}`; repeatable. A template that references `{{spec.K}}` **requires** it (hard error otherwise) — how an out-of-band value (an MR number, a variant tag) enters resolution without living in the file. | — |

`--work-dir` and `--spec` are `Profile` axes, so they work on the `project` read
queries (`build-dir`, …) as well as `build` — a script can resolve the effective
path for a materialised checkout without building.

### 1.3 Build options (affect command steps only)

| Flag | Alias | Meaning |
|---|---|---|
| `--config-only` | | Configure only; do not compile. |
| `--build-only` | | Compile only; assume already configured (errors if not). |
| `--reconfig` | | Delete the build dir and configure fresh. |
| `--install` | | Add an install step after building. |
| `--install-dir <DIR>` | | Override the resolved `install_dir` prefix (the backend's install-prefix, e.g. cmake's `CMAKE_INSTALL_PREFIX`). Affects configure as well as install. |
| `--build-dir <DIR>` | | Override the resolved `build_dir`, ignoring the project's template — e.g. to build a `review checkout` in an isolated dir. The symmetric partner of `--install-dir`; verbatim, highest priority. |
| `--uninstall` | | Reverse an install (backend-driven; see §7.3). Mutually exclusive with a build. |
| `--target <T>` | `-t` | Build a specific target (where the backend supports it). |
| `--extra-config-args <A>…` | `-Xconfig,<arg>` | Raw args appended to the configure command, verbatim. |
| `--extra-build-args <A>…` | `-Xbuild,<arg>` | Raw args appended to the build command, verbatim. |
| `--extra-install-args <A>…` | `-Xinstall,<arg>` | Raw args appended to the install command, verbatim. |

Extra args are applied **last, at the highest priority**, and are never
interpreted by the tool.

Modes are mutually exclusive; the default is `auto` (configure if needed, then
build).

### 1.4 `project` (describe / validate)

`project` with no subcommand is the read command:

- No positional: a one-line summary of every project.
- A name or path (§1): full details for that project, including each repo's
  branch/commit and any worktrees. With profile flags, resolved
  `repos.<name>.workdir`/`build_dir`/`install_dir` are shown; without them, the raw templates.
- `--check`: validate configuration legality (see §8). No positional validates
  everything (CI use); a name/path validates one.

### 1.5 Worktrees are not here

`project context {create,prune}` is **removed**; `wits worktree` manages worktrees
for any repository (see [`../worktree.md`](../worktree.md) and §8.3). Nothing
deletes a branch's `build_dir` any more — `project build-dir` prints the path.

---

## 2. Configuration topology

- **Config root** (one only), resolved highest-first:
  `$WITS_PROJECT_CONFIG` → `$XDG_CONFIG_HOME/wits/project` → `$HOME/.wits/project`.
- The root is scanned recursively for `*.toml` at load time. A file's top-level
  sections decide what it contributes; a file may mix sections.
- A file with `[project]` (and a required `[repos.main]`) **is one project**. The
  same `(org, name)` in two files is a hard error.
- `[toolchains.*]` and `[org]` + `[org.presets.*]` are additive registries merged
  across the whole tree.
- Organisations are always explicit: `[org] name = "…"` declares one,
  `project.org` joins it. Never inferred from the file path.

### 2.1 Org config (`[org.environment]` and `[org.definitions]`)

An org may declare shared value tables that every project joining it inherits:

```toml
[org]
name = "acme"

[org.environment]
REGISTRY = "registry.acme.example"

[org.definitions]
ACME_VERSION = 3
```

These are **applied unconditionally** to every project with `org = "acme"`, at
pipeline layer L0.5 (§5) — below the project's own `[project.environment]` /
`[project.definitions]`, so a project overrides an inherited value simply by
declaring the same key:

```toml
[project]
org = "acme"
[project.definitions]
ACME_VERSION = 4          # wins over the org's 3
```

The rule is the same one that governs a project: a level's bare `environment` /
`definitions` are that level's unconditional contribution, while its `presets`
(§4) apply only when named. Definitions keep their TOML type through inheritance,
so an org's `false` reaches a backend as a boolean, not the string `"false"`.

The same values are *also* exposed as `org.environment.*` / `org.definitions.*`
in the template context (§6.3), which is what you want when a key must be bound
under a different name, or in a context the build pipeline never runs:

```toml
[project.environment]
PUSH_TO = "{{org.environment.REGISTRY}}"     # bind to a different name

[repos.main.hooks]
clone = "boot --at {{org.environment.REGISTRY}}"   # hooks: no pipeline, no L0.5
```

Naming an org that no file declares is not an error; it simply inherits nothing
(and any `{{org.*}}` reference then fails to resolve).

---

## 3. `[project]`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `org` | string | no | Organisation to join (must be declared by some `[org]`). |
| `focus` | string | no | Which `[repos.*]` is the build focus. Default `"main"`. |
| `build_system` | string | when building | `cmake` \| `meson` \| `cargo` (backends shipping in v1). |
| `toolchain` | string | no | Default toolchain name (part of the selection chain, §5). |
| `generator` | string | no | Build-system generator (e.g. `Ninja`). |
| `build_dir` | template | when building | Build directory; see §6 for templating. |
| `install_dir` | template | no | Install prefix; templated. |
| `default_presets` | list\<string\> | no | Presets always applied (§4). |

`[project.environment]` and `[project.definitions]` — templated maps merged at
pipeline layer L1 (§5). `environment` becomes process env for the build;
`definitions` are build-system `-D` parameters. `extra_config_args`,
`extra_build_args`, `extra_install_args` — templated lists appended to the
respective commands.

`[project.presets.<name>]` — project-level presets (§4).

---

## 4. Presets

Declared at three levels:

- `[org.presets.<name>]` — org level (in a file that declares `[org]`).
- `[project.presets.<name>]` — project level.
- `[repos.<focus>.presets.<name>]` — repo level (the focus repo).

### 4.1 Preset keys

| Key | Type | Meaning |
|---|---|---|
| `extends` | string \| list | Inherit other presets; accepts `org/preset`. |
| `applies_when` | table | Structured auto-application match (§4.3). |
| `environment` | table | Templated env vars. |
| `definitions` | table | Templated build definitions. |
| `extra_config_args` | list | Appended to configure. |
| `extra_build_args` | list | Appended to build. |
| `extra_install_args` | list | Appended to install. |

### 4.2 Cross-level merge

A referenced name is the merge of the same-named preset at each level:

- **Maps** (`environment`, `definitions`): merged by key; on conflict the
  **nearest** (repo > project > org) level wins.
- **Lists** (`extra_*_args`): the **nearest** level's list **replaces** the
  others (not appended).

The merged definition's `extends` are then resolved.

### 4.3 `applies_when`

A table over a fixed key set: `build_type`, `toolchain`, `os`, `arch`,
`generator`.

- Multiple keys are AND-ed.
- A key's value is a scalar (equality) or an array (membership / OR).
- Comparison is **case-sensitive**.

A match auto-applies the preset for that build.

### 4.4 Application order

`default_presets` → `applies_when` matches → `--preset` (CLI). The combined list
is de-duplicated by name keeping the **last** position, so an explicitly-passed
preset moves late and wins. CLI `-X`/`--extra-*-args` (pipeline L3) sit above all
presets.

---

## 5. `[toolchains.<name>]`

Toolchains are **100% user-declared** — there are no built-ins. The vocabulary is
aligned with meson's native file. All fields are optional; declare what your
build needs.

### 5.1 Canonical fields (translated to each backend, §7)

| Field | Meaning |
|---|---|
| `cc`, `cxx`, `rustc` | Compilers. |
| `ar`, `nm`, `ranlib`, `strip` | Binutils. |
| `linker` | Linker (e.g. `mold`, `lld`). |
| `launcher` | Compiler launcher (e.g. `ccache`, `sccache`). |
| `c_flags`, `cxx_flags`, `link_flags` | Flag lists. |
| `supports` | Optional list of build systems, used only by `info --check`. |

Each canonical field is translated to a universal environment variable (`CC`,
`CXX`, `AR`, `NM`, `RANLIB`, `STRIP`, `CFLAGS`, `CXXFLAGS`, `LDFLAGS`, `RUSTC`)
plus a backend-native definition where one exists — see §7.

### 5.2 Pass-through blocks (not translated)

- `[toolchains.<name>.environment]` — env vars applied verbatim.
- `[toolchains.<name>.definitions]` — definitions applied verbatim.

### 5.3 Selection chain

```
env  >  --toolchain  >  project/repo `toolchain` field  >  [toolchains] entry
```

The toolchain *name* is always selected (path templates depend on it). Its
env/definitions **injection** is skipped in `auto`/`build-only` mode when the
build dir is already configured and no toolchain was explicitly requested.

---

## 6. Templates

Config values are templated. Config format is TOML only.

### 6.1 `{{ path }}` substitution

Dotted lookup over the context (§6.3): tables by key, arrays by integer index. A
value that is a single whole-string placeholder returns the **typed** value (a
list or integer survives); an embedded placeholder is stringified (`true`/`false`
lowercase, integers decimal). Resolution is lazy and recursive with cycle
detection.

### 6.2 `[[ expr ]]` expressions

A minimal numeric expression, e.g. `LINK_JOBS = "[[ max(1, system.mem.gb // 4) ]]"`.

- Operators: `+ - * / // %` over int/float; comparisons `== != < <= > >=`.
- Functions: `min`, `max`, `int`, `float`, `str`, `bool`.
- **Not** supported: `**`, bitwise ops, `and`/`or`/`not`, ternary, arbitrary
  names, list/dict literals. (Conditions are `applies_when`, §4.3.)

### 6.3 Context variables

```
project.{ name, org, focus }
repo.*                     # the *current* repo (focus repo in project scope;
                           #   the repo itself in a repo-scoped field like a hook)
  { name, path, kind, main_branch, anchor, origin, upstream, mirrors }
repos.<name>.*             # any repo by explicit name; same fields as repo.*
org.environment.<K>        # org entry (§2.1); inherited, and nameable here too
org.definitions.<K>        # org entry (§2.1); inherited, and nameable here too
repos.<name>.workdir       # effective checkout dir for the named repo (§9)
branch.{ raw, slug }       # raw = branch name; slug = filesystem-sanitised
build_type
toolchain.{ name, cc, cxx, rustc, ar, nm, ranlib, strip,
            linker, launcher, c_flags, cxx_flags, link_flags }
generator
system.{ os, arch, memory.gb, cpu.count }
env.*                      # process environment
spec.*                     # CLI-registered vars (--spec K=V); required if referenced
```

- `repo` is a **relative** alias for the repo being resolved; use `repos.<name>`
  to reference any other repo.
- There is no bare `{{branch}}`; use `{{branch.raw}}` or `{{branch.slug}}`.
- `repo.upstream` falls back to `repo.origin` when no upstream is declared.
- `spec.*` holds only what `--spec K=V` supplied on the command line, so a
  template referencing `{{spec.mr}}` fails loudly unless the caller passes it —
  never guessed or defaulted.
- `org.environment.*` / `org.definitions.*` are available in project scope and in
  repo-scoped fields (hooks, `worktree_dir`, `bootstrap_worktree_dir`). Only accessible when `project.org`
  is set and the org declares the key; references to undeclared keys are hard errors.
  They are **not** available in a `repos.*.path` template, which resolves against
  the Profile-free path context (§9.1).

### 6.4 Errors

Every failure is hard: unknown path, cycle, type mismatch, division by zero. The
context is always fully populated, so a missing path always means a real mistake.

---

## 7. Backends

`build_system` selects a backend. A backend does three things: translates the
selected toolchain's canonical fields to native form, emits the command steps for
a mode, and detects prior configuration.

### 7.1 Canonical-field translation

| Canonical | cmake | meson | cargo |
|---|---|---|---|
| `cc` / `cxx` | `CMAKE_C/CXX_COMPILER` | `CC`/`CXX` (env / native file) | `CC`/`CXX` env |
| `rustc` | — | — | `RUSTC` |
| `ar`/`ranlib`/`strip`/`nm` | `CMAKE_AR`/`CMAKE_RANLIB`/… | native file / env | env |
| `linker` | `CMAKE_LINKER` / `-fuse-ld` | `CC_LD`/`CXX_LD` | `CARGO_TARGET_*_LINKER` |
| `launcher` | `CMAKE_*_COMPILER_LAUNCHER` | prefix on `CC`/`CXX` | `RUSTC_WRAPPER` |
| `c_flags`/`cxx_flags`/`link_flags` | `CMAKE_C/CXX_FLAGS` / linker flags | `CFLAGS`/`CXXFLAGS`/`LDFLAGS` | `CFLAGS` / `RUSTFLAGS` |

For meson and cargo, each canonical field is *also* exported as its universal env
var (`CC`, `CXX`, `AR`, `CFLAGS`, …); **cmake is the exception** — it is
configured entirely through `-D` definitions and is not given these environment
variables, which it does not need and which can conflict with its cached
compiler. This translation runs at pipeline layer L0, so an explicit preset or
CLI override of the same key wins.

Multi-config cmake generators (Ninja Multi-Config, Visual Studio, Xcode) are
handled correctly: `CMAKE_BUILD_TYPE` is *not* set at configure, and the build
type is selected at build/install time with `--config`.

### 7.2 `is_configured`

- cmake: `CMakeCache.txt` present in the build dir.
- meson: `meson-private/coredata.dat` present.
- cargo: not applicable.

### 7.3 Modes

`auto` | `config-only` | `build-only` | `reconfig` | `uninstall`. `--install`
adds an install step to a build. `uninstall` is backend-driven — meson `ninja -C
<build> uninstall`, cmake via `install_manifest.txt`, cargo unsupported — never a
recursive delete, because an install prefix may be shared.

---

## 8. `info --check` validation

Reports (does not fix): required fields present (`repos.main`, `main_branch` for
own-git repos, `build_dir`/`build_system` when building); preset inheritance and
template reference cycles; template resolvability against a representative
context; referenced toolchains exist; when a toolchain declares `supports`, that
it covers the project's build system; and, for a cloned checkout, that a declared
`skip` is in force (§9.5). No `<name>` checks every project.

Malformed structure is rejected earlier, when the registry loads, so `--check`
never sees it: a repo with neither `path` nor `from`, a `from` naming an unknown
project or repo, a borrowed repo that is itself borrowed, and a travelling field
declared alongside `from` (§9.4) all fail the load outright. So do a
worktree/hybrid repo without `worktree_dir`, a hybrid repo without
`bootstrap_worktree_dir`, and a bootstrap template that references `branch.*`.

Whether the declared `build_system` actually has a backend is **not** checked
here — that is reported by `wits build` at run time, since the read-only core
deliberately knows nothing of which build systems are implemented.

---

## 9. Repos, branches, and build contexts

### 9.1 `[repos.<name>]`

| Key | Type | Required | Meaning |
|---|---|---|---|
| `path` | **template** | yes, unless `from` | On-disk repository location (the bare common dir for worktree/hybrid) or subpath relative to `repos.main` (nested). Resolved against a Profile-free context: `project.name`, `project.org`, `env.*`, `system.*` — no `repos.*` (would be circular). Nesting + `main_branch` determine the inferred kind (below). |
| `from` | string | yes, unless `path` | Borrow another project's repo as this one: `[<org>/]<project>[:<repo>]`, the repo defaulting to `main`. See §9.4. |
| `skip` | list\<string\> | no | Paths this checkout never materialises: ordered gitignore-style patterns where `!` re-includes. **Not templated.** See §9.5. |
| `main_branch` | string | own-git repos | The branch `update` fast-forwards. Not allowed for `subtree`. |
| `anchor` | string | no | Repo whose `path` is this build's source/base; unset → self. |
| `source_dir` | template | no | Where the backend configures from (the top-level `CMakeLists.txt`/`meson.build`/…) when it is not the checkout root. Read from the build repo; defaults to its `repos.<name>.workdir`. E.g. `"{{repos.main.workdir}}/src"`. Only the configure source changes — the named `workdir` still anchors `build_dir`/`install_dir` and branch identity. |
| `branch_strategy` | string | no | `in-place` (default) \| `worktree` \| `hybrid`. Worktree and hybrid use a bare clone. |
| `worktree_dir` | template | worktree/hybrid | Where a branch's worktree belongs. Hybrid first discovers an existing checkout and uses this only as the fallback/suggested path. A relative result is anchored beside `repo.path`, never at process cwd. |
| `bootstrap_worktree_dir` | template | hybrid; optional for worktree | Fixed initial `main_branch` checkout created after a bare clone. Must not reference `branch.*`. Worktree defaults to `worktree_dir` rendered for `main_branch`; an explicit relative value is resolved beside that rendered main path. |

**Kind is inferred, not declared**: a non-nested `path` → `standalone`; a nested
`path` with `main_branch` → `submodule`; nested without `main_branch` → `subtree`.
`repos.main` is always standalone.

`[repos.<name>.remotes]` — `origin` (string, the push target / fork), `upstream`
(string, the **sync source**), `mirrors` (list of extra push URLs on origin).
The **sync source** = `upstream` if declared, else `origin`; it is what `clone`
and `update` fetch from and fast-forward `main` against. When an `upstream` is
declared, `origin` is **never fetched or cloned** — so a fork that does not yet
exist on the server is fine (it is only added as a push target). Reconciliation
is additive only: missing remotes/mirror push-URLs are added; existing URLs are
never modified or removed; unmentioned remotes are untouched.

`[repos.<name>.hooks]` — inline `sh -c` command strings, templated. Phases:
`clone` / `post_clone` and `pre_update` / `update` / `post_update`. (The clone
phase has no `pre` hook — the repo does not exist yet.) The bare phase name
(`clone`, `update`) overrides that phase's default action; `pre_`/`post_` add
hooks around it.

**Hook cwd by phase**: a `clone` override runs in the **current working
directory** (the repo's `path` does not exist yet, and `git clone` creates the
destination itself). For in-place, later hooks run in `path`; for
worktree/hybrid, `post_clone` and update hooks run in the bootstrap/current
`main_branch` worktree when it exists, otherwise update hooks fall back to the
bare `path`. A bare clone override must create its configured bootstrap
worktree.

A non-zero exit fails fast (§10).

`[repos.<name>.presets.<preset>]` — repo-level presets (§4).

### 9.2 `{{repos.<name>.workdir}}` resolution

`repos.<name>.workdir` is the named repo's effective checkout. In a build, the
**anchor** repo's `workdir` is where sources come from; the **focus** is the repo
switched to the target branch within it (its own git, or the git it shares when
the focus is a subtree).

- **in-place**: `repos.<name>.workdir` = that repo's `path`. Switching the focus to a
  non-current branch stashes, switches, builds, then always restores (branch,
  stash, and the focus's submodules) on any exit.
- **worktree**: `repos.<name>.workdir` = that repo's resolved `worktree_dir` for the target branch.
  It must already exist; `build` never creates it. `wits worktree create <branch>
  "$(project work-dir … --branch <branch>)"` makes one.
- **hybrid**: if Git reports a live worktree currently attached to the target
  branch, its actual path wins regardless of location. Otherwise resolution
  returns `worktree_dir` as the suggested path; `build` then fails with the
  creation command rather than creating it.

### 9.3 Branch identity

The identity is the branch name of the nearest own-git repo in the
`focus → anchor` chain. A detached HEAD is unsupported. `branch.slug` replaces
every character outside `[A-Za-z0-9._-]` (including `/`) with `_`.

`branch_strategy` is read from the **build repo** (the anchor) only. A
`branch_strategy` on a focus that is not its own anchor has no effect; to build a
particular worktree of such a focus, pass it with `--work-dir` (§1.2).

### 9.4 `from` — borrowing another project's repo

`from = "[<org>/]<project>[:<repo>]"` makes this entry *be* another project's
repo. The project reference resolves exactly as a CLI positional name does (§1):
bare or `org/name`, ambiguity across orgs is an error. The repo defaults to
`main`.

Resolved once at load. What the source supplies, and may therefore **not** be
declared alongside `from` (doing so is a hard error naming the fields):

```
path   main_branch   branch_strategy   worktree_dir
bootstrap_worktree_dir   skip   remotes   hooks
```

What stays the borrower's: `anchor`, `source_dir`, `presets`.

- The resolved `path` is the source's **absolute** path, so a nested source
  resolves against its own project's root.
- A borrowed repo's inferred kind is always `standalone` — from this project's
  side it is an external checkout with its own git.
- **A borrow may not itself be borrowed** (hard error).
- **A borrow never owns a path.** `project_for_path` / `repo_for_path` ignore
  borrowed entries, so a checkout shared by several projects resolves to the one
  that declares it as its own.
- **`update` skips borrowed repos** unless `--with-borrowed` is passed (§1). A
  borrowed hook then resolves against the *borrower's* `org.*` namespace.

### 9.5 `skip` — paths never checked out

An ordered list of gitignore-style patterns naming what to leave *out*; a `!`
entry re-includes, and the **last matching entry wins**:

```toml
skip = ["/third_party/engine", "/vendor", "!/vendor/keep.c"]
```

Realised as sparse-checkout patterns `/*` plus each entry with its leading `!`
toggled, always `--no-cone` (cone mode cannot express an exclusion). A skipped
path that is a **submodule** additionally needs `git submodule deinit`, before the
sparse write — sparse alone cannot remove a materialised submodule.

| When | What happens |
|---|---|
| in-place clone | Patterns written **before** the checkout, so nothing skipped is ever materialised. |
| worktree/hybrid clone | Create bootstrap checkout, initialise submodules, deinit covered submodules, then write and verify patterns. |
| clone override | Apply the mask to the checkout the override created, run `post_clone`, then verify again. |
| `wits worktree create` | Driven from the primary/bootstrap checkout, so `git worktree add` copies its pattern file. |
| `update` | **Verifies** before doing anything; a contradicted `skip` is a hard error. |
| `project --check` | Verifies and reports. |

Writing the patterns is **refused** when the checkout already has sparse patterns
wits did not write — including any cone-mode configuration, which cannot hold an
exclusion at all. `sparse-checkout set` replaces the whole list, so a checkout that
something else (typically a `clone` hook) narrowed to a cone would be *widened*
rather than masked. Fold the exclusions into those patterns where they are
maintained, or do not declare `skip` for that repo.

Verification is behavioural, not textual — it asks whether anything the list
excludes is still materialised, so extra sparse patterns of your own are legal and
ignored. It reports two things: a wholly-excluded path that exists on disk, and an
index entry under a skipped path not tagged `S` (skip-worktree). Glob entries, and
paths under them, are **not** verified. Under `-v` the git commands that would fix
a violation are printed, in the order that works; wits never runs them itself,
because applying a mask to a tree it did not build means deleting content.

---

## 10. `update` / `clone`

For each repo (parents before nested; subtrees do no git work):

The **sync source** = `upstream` if declared, else `origin` (§9.1).

- **Missing path → clone**: in-place defaults to `git clone --origin <sync>`;
  worktree/hybrid build a **tracking bare host** — `git init --bare`,
  `git remote add <sync>`, `git fetch --tags`, then `main_branch` created from
  `<sync>/<main_branch>` as the repository's symbolic HEAD — and add the
  configured bootstrap worktree on it. Deliberately not `git clone --bare`, which
  copies every remote branch into `refs/heads`, writes no fetch refspec, and
  publishes no `origin/HEAD`; see [worktree.md](../worktree.md#a-bare-repository-made-by-git-clone---bare).
  Initialise submodules, apply
  `skip`, run `post_clone` in the checkout, and verify `skip` again. A `clone`
  override runs in the current directory and owns both repository and bootstrap
  creation. Cloning names the fetched remote after the sync source, so tracking
  an `upstream` leaves `origin` free for a fork.
- **Existing → update**: ensure remotes (additive — including a fetch refspec for
  a remote that has none, which is how a repository cloned with `git clone --bare`
  is repaired) → `pre_update` → action → `post_update` (cwd = conventional
  checkout, bare main worktree, or bare path when that worktree is absent).

Default update action:

- On `main_branch`: `git fetch <sync>` then `git merge --ff-only
  <sync>/<main_branch>`.
- Otherwise: `git fetch <sync> <main_branch>:<main_branch>` — a ref-only
  fast-forward that does not check out, does not touch the working tree, and does
  not expand a sparse checkout.
- Bare-backed: `git fetch <sync>`, then fast-forward whichever linked worktree
  holds `main_branch`; if none exists, advance the local branch ref with
  `update-ref`, refusing anything that is not a fast-forward. Nested repo
  lifecycle work is skipped until a main worktree exists again.
- Declared submodule repos advance via their own lifecycle; undeclared nested
  submodules are refreshed with `git submodule update --recursive -- <materialised
  paths>` (no `--init`; `--init` happens only on clone or worktree creation).

Failure is fail-fast: a non-zero hook/action stops the operation, an RAII guard
restores the original branch (and pops any stash), a log line is written,
remaining repos are skipped, and the process exits non-zero.

---

## 11. Crate API (read-only)

```rust
let ws = Workspace::load(config_root)?;
ws.projects();                        // iterate
ws.project("mesa", org)?;             // by name / org
ws.project_for_path(&path);           // which project owns this checkout

let p = ws.project(...)?;
p.repos();  p.build_repo();
p.main_branch();
p.git_state();                        // branch, commit, dirty, submodules
p.work_dir(&profile);
p.build_dir(&profile);  p.install_dir(&profile);
p.resolve("{{ ... }}", &profile);
p.validate();
```

The core resolves paths but never destroys them; the only side-effecting entry
points are the `build` and `update` actions.
