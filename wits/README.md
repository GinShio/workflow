# `wits`

A single binary that collects personal workflow tools behind one command
tree. The point of a collection (rather than a directory of loose scripts) is a
shared library, consistent flags, and one thing to build and put on `$PATH`.

The collection grows one subcommand at a time, and this repository only ever
contains what is actually finished. Today that is:

| Command | Purpose |
|---|---|
| [`wits transcrypt`](docs/commands/transcrypt.rst) | Transparent file encryption wired into git's clean/smudge filters |
| [`wits stack`](docs/commands/stack.rst) | Manage a stack of branches as a set of merge requests (push, open/retarget MRs, navigation) |
| [`wits review`](docs/commands/review.rst) | Review merge requests locally across forges: fetch, comment, verdict, and submit as one batch |
| [`wits worktree`](docs/commands/worktree.rst) | Create, inspect, and reclaim git worktrees in any repo (submodules borrowed, not re-cloned) |
| [`wits project`](docs/commands/project.rst) | Describe/validate source projects from one declarative registry, and answer path queries for scripts |
| [`wits build`](docs/commands/build.rst) | Configure and build a project on top of that registry (cmake/meson/cargo) |
| [`wits update`](docs/commands/update.rst) | Refresh git for every repo of a project |
| [`wits dotfiles`](docs/commands/dotfiles.rst) | Compile a TOML manifest tree into Dotdrop's per-host configs |

Built-in commands live in the `wits` binary (a module under
`crates/wits/src/cmd/` plus a match arm). Anything else is a **plugin**: `wits
foo` runs a `wits-foo` executable from your `$PATH`, git-style, so a
domain-specific workflow plugs in without being compiled into `wits`.

The documentation is a full RST book under [`docs/`](docs/index.rst):
installation, the command guides, plugins, the scaffold plugin, and a
`reference/` volume holding the exhaustive config/flag tables together with
the design notes.

## Build and install

`wits` is a Cargo workspace (the `wits` binary + the shared `wits-util` library,
plus any plugin crates). **Meson** drives the build and install; **cargo** does
the Rust compilation underneath. That split is deliberate: some dependencies
ship native-code build scripts (notably `ring`, via the forge's HTTPS client)
that Meson's native Rust support cannot build without hand-porting, so cargo owns
compilation while Meson owns install/packaging.

```sh
meson setup build              # configure (needs meson >= 0.61, cargo, ninja)
meson compile -C build         # runs `cargo build --release` under the hood
meson install -C build         # installs wits + wits-<sub> symlinks into <prefix>/bin
```

Pass `--prefix ~/.local` to `meson setup` to install under your home; Meson
honours `DESTDIR` for packaging. For plain development, `cargo build`/`cargo
test` work as usual (binary at `target/debug/wits`; add `--release` for
`target/release/wits`), no Meson required. See
[Installation](docs/installation.rst) for the details.

## Invocation forms

A built-in `foo` can be called two ways:

```sh
wits foo ...     # umbrella form
wits-foo ...     # direct form (a symlink to wits, created by `meson install`)
```

The direct form is a symlink whose name `wits` reads from `argv[0]` and splices
back in as the subcommand — same binary, no second process. Applet names come
straight from the subcommand list, so a new built-in earns its symlink for free.

## Plugins

When `foo` is not a built-in, `wits foo` execs a `wits-foo` executable found on
`$PATH` — the convention git and cargo use. A plugin is therefore any executable
named `wits-<name>`, in any language; an in-tree Rust plugin can additionally
depend on `wits-util` to reuse the process/git/config/template floor rather than
reinventing it. `wits help` lists the built-ins plus the plugins it discovers on
`$PATH`. The full contract is in [Writing a wits plugin](docs/plugins.rst).

Shipped in this workspace:

| Plugin | Purpose |
|---|---|
| [`wits scaffold`](docs/scaffold.rst) | Register a new Vulkan/SPIR-V extension everywhere a tree needs it, from the specification that defines it |

## Global flags

| Flag | Meaning |
|---|---|
| `-v`, `--verbose` | Show the underlying git commands as they run |
| `-n`, `--dry-run` | Print mutating commands instead of running them (read-only queries still run) |

`-n` has no visible effect on `transcrypt`, which only ever reads — it lives at
the top level because it is part of the contract future, state-changing commands
inherit from the process layer.
