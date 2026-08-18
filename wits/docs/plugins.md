# Writing a `wits` plugin

`wits` has no plugin registry, no config file to edit, and no recompilation step
to add a workflow. It uses the same mechanism git and cargo do: **when `wits foo`
is not a built-in, it runs a `wits-foo` executable found on your `$PATH`.** A
plugin is just such an executable.

This keeps domain-specific, loosely-coupled workflows (say, a GPU test runner)
out of the core binary while still presenting them under the one `wits` command
tree.

## The contract

- **Naming.** A plugin is an executable named `wits-<name>` on `$PATH`. `<name>`
  is what the user types: `wits-gpu` is invoked as `wits gpu`.
- **Precedence.** Built-in subcommands win. A `wits-stack` on `$PATH` is never
  reached, because `stack` is built in. Pick a name that is not a built-in
  (`wits help` shows the reserved set).
- **Invocation.** `wits gpu a b --flag` execs `wits-gpu a b --flag`: everything
  after the subcommand name is forwarded verbatim. The direct form `wits-gpu ...`
  also works — it is just the executable.
- **Process model.** `wits` replaces itself with the plugin (`exec`), so the
  plugin owns the terminal and stdin/stdout/stderr, and *its* exit status is what
  the caller sees. `wits` adds nothing to the environment.
- **Discovery.** `wits help` lists every `wits-*` on `$PATH` (minus the built-in
  applet symlinks) under a `Plugins` section, so an installed plugin is visible
  without being registered anywhere.
- **Language.** Any. A shell script, a Python script, a Rust binary — anything
  executable and named correctly.

There are no required flags, but honouring the shared conventions (`-n/--dry-run`,
`-v/--verbose`) makes a plugin feel native.

## Sharing the `wits-util` floor (in-tree Rust plugins)

A plugin written in Rust in this workspace can reuse the same library the
built-ins use instead of reinventing it. Add a binary crate under `crates/` whose
binary is named `wits-<name>`, and depend on `wits-util`:

```toml
# crates/wits-gpu/Cargo.toml
[package]
name = "wits-gpu"
version.workspace = true
edition.workspace = true

[[bin]]
name = "wits-gpu"
path = "src/main.rs"

[dependencies]
wits-util.workspace = true
anyhow.workspace = true
clap.workspace = true
```

Add it to the workspace `members` in the root [Cargo.toml](../Cargo.toml). Now
`wits_util::{process, git, config, template, ...}` are all available — the same
subprocess/dry-run discipline, config-tree discovery, and template engine the
core commands use. Extend the workspace's `meson.build` to build and install the
plugin binary the same way `wits` is, so `meson install` ships it alongside
`wits`.

An out-of-tree plugin needs none of this: it only has to be named `wits-<name>`
and be on `$PATH`.
