# `wits dotfiles`

Compile a dotfiles repository that describes itself in TOML into the flat
catalogue [Dotdrop](https://github.com/deadc0de6/dotdrop) can actually deploy.

You write what each module deploys, which machines exist, and which content
layers and execution contexts each machine wants. Dotdrop has no conditional
selection, no layered sources, and no notion of privilege, so it cannot hold any
of that. `wits dotfiles generate` turns the description into per-host Dotdrop
configs; `dotdrop install -c` deploys them.

Two verbs, because there are two questions worth asking of a description:

| Command | Answers |
|---|---|
| `wits dotfiles check` | Is it coherent? Reads everything, decides everything, writes nothing. |
| `wits dotfiles generate` | What does it mean? Writes the Dotdrop configs. |

Both read a repository's layout declaration: `--config FILE`, or `--root DIR` for
the default-named file in a directory. Without either, the declaration is found
from `$WITS_DOTFILES_CONFIG`, then `wits.dotfiles.config` in git config, then by
walking up from the current directory looking for `dotfiles.toml`. The walk is
what makes the command work with no arguments when you are standing in the
repository, which is where you are when you edit a manifest.

`-n` previews a `generate` without touching the disk.

> Status: implemented. It renders no file bodies, installs no packages, and holds
> no opinion about encryption — those are Dotdrop's, your package manager's, and
> `.gitattributes`'s respectively.

---

## The mental model in one minute

- A **module** is one tool, at `modules/<app>/`. It owns a `manifest.toml` saying
  what it deploys, and one directory per overlay holding the content.
- An **install** is one deployable unit. It names a *path inside an overlay*, not
  a source directory, so one install fans out to one Dotdrop entry per overlay
  that actually contains that path.
- An **overlay** is a content and variable layer: `common`, `personal`,
  `khronos3d`. A host stacks them in order, and later ones win.
- A **capability** (`develop`, `desktop`, …) selects install units and nothing
  else.
- A **plane** (`user`, `system`, …) is an execution context. It selects install
  units *and* partitions output, because two planes cannot share a Dotdrop run.
- A **host** is one machine: the capabilities it has, the overlays it wants, and
  the planes it deploys.

Capabilities, overlays, and planes are independent. Capabilities filter, overlays
multiply, planes partition.

`common` is an ordinary overlay with no special handling, and no module name is
privileged.

## Layout

That model is fixed. The paths that express it are not — they belong to the
repository, and `dotfiles.toml` at its root declares them. Its own directory is
the root every path in it is relative to, so the two can never disagree.

Every key has a default, so a repository that follows them declares one path:

```toml
[layout]
composition = 'modules/dotdrop/hosts.toml'
```

which implies the rest:

| Key | Default | Meaning |
|---|---|---|
| `layout.modules` | `modules` | One subdirectory per module |
| `layout.module_manifest` | `manifest.toml` | A module's manifest, within it |
| `layout.module_fragments` | `manifest` | Directory of a module's per-overlay values, and the one name inside a module that is never an overlay |
| `layout.composition` | `hosts.toml` | Planes, hosts, base backend settings |
| `layout.globals` | `globals.toml` beside the composition | Cross-module plaintext values |
| `layout.fragments` | `<module_fragments>` beside the composition | Per-overlay values with no module owner |
| `output.dir` | the composition's directory | Root of generated output |
| `output.entrypoint` | `{plane}.{host}.toml` | One config per plane × host |
| `output.variables` | `variables.toml` | Shared values |
| `output.actions` | `actions.toml` | Shared actions |
| `output.overlay_variables` | `{overlay}/variables.toml` | One per overlay that changes something |

Output keys under `[output]` are paths within `output.dir`, and the placeholders
may sit in directory positions — `entrypoint = '{plane}/{host}.toml'` is a
layout, not just a name. `{plane}` and `{host}` are required in `entrypoint` and
`{overlay}` in `overlay_variables`; a template missing one would collapse
distinct files onto a single path, and the loser would simply be absent.

Every path a generated file uses to reach another is **computed** from this, not
spelled: move `output.dir` one level deeper and the `dotpath` and imports follow.
A literal `..` would be correct exactly until someone moved a directory, and then
wrong in a way that reads like a missing variable.

A directory under `layout.modules` that holds the composition, the globals, the
ownerless fragments, or the output is not a module. That is how a repository can
keep those files inside the module tree — as this one does, under
`modules/dotdrop/` — without any module name being special to the tool.

## Planes

A plane is declared, not built in. There is nothing privileged about the names
`user` and `system`; they are simply the two contexts this setup happens to
deploy into.

```toml
[planes.user]
dst_prefixes = ['~/', '{{@@']

[planes.system]
dst_prefixes = ['/etc/']

[planes.system.config]
workdir = '/var/lib/dotdrop'
```

A plane carries data rather than being a bare name because a privileged run is
genuinely a different environment: `~` resolves to root's home under `sudo`, so
the system plane needs its own `workdir` or it scatters rendered templates
through `/root`. `[planes.<name>.config]` is layered over the repository-wide
`[config]` in each of that plane's entrypoints.

`dst_prefixes` is a guard, not a requirement: when set, every install in the
plane must have a `dst` starting with one of the listed prefixes. It costs one
line and catches the mistake this layout invites — a `~/…` destination in a
plane that runs as root, or an `/etc/…` one in a plane that does not.

## Hosts

```toml
[config]
backup = true
create = true
workdir = '~/cyber/dotfiles'

[defaults]
ccache_remote_enabled = false

[hosts.strix]
capabilities = ['develop', 'desktop']
overlays = ['common', 'personal']
planes = ['user', 'system']

[hosts.'Khronos3D-russell-openSUSE']
capabilities = ['develop', 'desktop']
overlays = ['common', 'khronos3d']
planes = ['user', 'system']

[hosts.'Khronos3D-russell-openSUSE'.variables]
ccache_remote_enabled = true
```

Hosts never list modules. `overlays` is merge and deployment order. Omitting
`planes` means every declared plane. `[defaults]` holds variables every host
starts with; `[hosts.<name>.variables]` overrides them.

`[config]` is the backend's settings block every generated entrypoint starts
from, passed through untouched. The three exceptions are `dotpath`,
`import_variables`, and `import_actions`, which are computed from the layout and
so rejected here rather than silently overwritten. Everything else is yours,
including keys this tool has never heard of.

## Module manifests

```toml
[[install]]
id = 'prompts-agents'
dst = '~/.cursor/agents/'
path = 'agents/'
capabilities = ['develop']
planes = ['user']
link = 'absolute'

[variables.ssh.github]
url = 'github.com'

[dynvariables]
aria2_bin = 'command -v aria2c 2>/dev/null || echo /usr/bin/aria2c'

[actions]
systemd-reload = 'systemctl daemon-reload || true'
```

| Field | Meaning |
|---|---|
| `id` | Unique across all modules. The Dotdrop id is `<id>-<overlay>`. |
| `dst` | Passed to Dotdrop verbatim; it may be a template. |
| `path` | Relative to `modules/<app>/<overlay>/`. `.` means the overlay root. |
| `capabilities` | Empty means unconditional. Otherwise one must match the host. |
| `planes` | Required. A unit with no stated plane has no defined privilege. |
| `requires_overlays` | All must be present on the host. |
| `link`, `chmod`, `actions` | Passed through to Dotdrop. |

An install becomes a deployed entry for overlay `O` when
`modules/<app>/<O>/<path>` exists. That is why `git` with `path = '.'` produces
`git-common` and `git-personal`, while `amdgpu-pro` naturally produces only
`amdgpu-pro-khronos3d` — no other overlay has that path.

`planes` is required rather than defaulted, and it is per install rather than per
module, so one module can deploy into more than one context.

## Per-overlay values

A module's fragment directory carries the values that belong to one module and
one overlay:

```toml
# modules/git/manifest/personal.toml
[variables.git.identity]
username = 'GinShio'
email = 'ginshio78@gmail.com'
```

Variables only. Actions and dynvariables live in a single flat namespace shared
by every module, so letting an overlay define one would make the available names
depend on which machine you are standing at — a collision that appears on one
host and nowhere else is the worst kind to debug.

Private values stay with their owner. `layout.fragments` points at a directory
for a secret with no module owner at all; if it is growing, something is in the
wrong place.

### Splitting an overlay across files

One overlay may have any number of files:

```
manifest/personal.toml            # the overlay's plain fragment
manifest/personal.identity.toml   # a named part
manifest/personal.secret.toml     # another
```

They merge as a single layer, plain fragment first and named parts after it in
name order, so a later part overrides an earlier one exactly the way a later
overlay overrides an earlier one.

The reason to split is **encryption**. `.gitattributes` marks whole files, so
values that belong to the same overlay but not to the same secret cannot share
one. A split is the only way to say "this half is public and that half is not".

Flat suffixes rather than a directory per overlay, for the same reason: the
split is driven by path patterns, and a suffix keeps every fragment of a module
one glob away (`manifest/*.toml`) while leaving the ordinary single-value case as
a single file with a meaningful name.

Two rules follow from the naming:

- A file naming no overlay any host selects is reported as dead weight; before,
  fragments were probed by name and such a file was silently invisible.
- A file that could belong to two overlays — possible only when overlay names
  nest, like `work` and `work.eu` given `work.eu.toml` — is an error. Guessing
  which was meant would file a value under the wrong encryption key.

## Merge order

Lowest to highest:

1. `globals.toml`
2. every module's `manifest.toml`, in module-name order
3. each module's `manifest/<overlay>.toml`, in the host's overlay order
4. `dotdrop/manifest/<overlay>.toml`, in the host's overlay order
5. host defaults, then the host's own variables

Tables merge recursively; scalars and lists replace. Lists replace rather than
concatenate because a list here is a *setting* — a host's overlays, a module's
kernel parameters — and the only useful thing a later layer can say about a
setting is what it should now be. Appending would make "drop the inherited value"
inexpressible.

Within step 3, a module's own fragments merge plain-first then by part name.

Two modules defining the same action or dynvariable name is an error, not an
override: the winner would depend on directory order. Two modules writing the
same variable path is reported as a note, since nesting gives each module its own
subtree by convention and a collision usually means one of them is in the wrong
namespace. So is two *fragments of one overlay* writing the same path — splitting
a fragment is what makes one file able to quietly overwrite another.

## What gets generated

Four kinds of file, at whatever paths `[output]` names:

- **one entrypoint per plane × host** — a complete config: settings, the dotfiles
  that host deploys, its profile;
- **shared variables** and **shared actions** — plaintext, from `globals.toml`
  and every module's manifest;
- **one file per overlay that changes something** — encrypted under that
  overlay's key.

Then:

```sh
dotdrop install -c modules/dotdrop/user.strix.toml -p strix
sudo dotdrop install -c modules/dotdrop/system.strix.toml -p strix
```

The default entrypoint name is keyed on the host rather than an overlay because
the set of aggregates one may import is a property of the host. Naming them after
an overlay only works while every machine has exactly one private overlay.

Output is deterministic, and a file whose content has not changed is not
rewritten — so "did editing that manifest change anything?" is answerable from
`git status`.

### Why the output is shaped this way

Three Dotdrop behaviours decide the layout. None is documented; all three were
established by reading `cfg_yaml.py` and confirmed against a live install.

**Dotdrop accepts TOML.** It dispatches on the file extension for the main config
and for every `import_*` target alike, so the generated tree is TOML end to end,
in the same language as its inputs. The only translation in the pipeline is
structural.

**Imported variables merge shallowly.** The last file to mention a top-level key
replaces that whole key. An overlay that changes `testing.result_dir` cannot
contribute just that leaf — it would erase the sibling `testing.*` values. So
each aggregate republishes the whole top-level key it touches.

**Each imported variables file is resolved on its own.** The templater is built
from that file alone, so a republished `runner_dir = "{{@@ testing_runner_dir @@}}"`
arrives undefined unless `testing_runner_dir` travels with it. The generator
therefore closes each aggregate over the shared values its own templates refer
to, transitively.

The obvious alternative — copying every shared variable into every aggregate,
which is what a hand-maintained bundle drifts into — is correct but expensive in
the one place it hurts. These files are encrypted, encrypted blobs do not merge,
and a wholesale copy means every edit to a shared variable rewrites every private
file. Carrying only what an overlay changes, plus what those values need in order
to resolve, keeps a shared edit out of them entirely. It is also what lets a host
stack two private overlays without the later one's copy of the shared defaults
silently reverting the earlier one's overrides.

A fourth behaviour explains why entrypoints are self-contained rather than
importing a shared per-plane catalog: a `dst` like
`{{@@ sysenv.amd_config_dir @@}}/…` is rendered by whichever config *declares*
it, using only that config's variables. A catalog in an imported file cannot see
the host's. The same file would also have to carry every host's profile, which
puts each machine's variables in front of every other machine — the wrong default
when a host variable can be a secret.

## Encryption

Encryption is a git concern, not this tool's. `.gitattributes` assigns a
transcrypt key to encrypted overlay trees, per-module overlay fragments, and the
generated aggregates:

```
modules/*/manifest/personal.toml    filter=transcrypt-personal …
modules/dotdrop/bundle/personal/**  filter=transcrypt-personal …
```

Because [`wits transcrypt`](transcrypt.md) runs as a smudge filter, a fragment is
already plaintext in the working tree of a clone that has the key — the generator
just reads files. What it does do is refuse to run when a fragment it needs is
still ciphertext, naming the file: generating from a locked fragment would
silently produce a bundle missing that overlay's values, which is far worse than
stopping.

## Checks

`check` fails on anything that makes the model incoherent:

- an install with no `planes`, or naming an undeclared one
- an install id used by two modules
- an install whose `dst` is outside its plane's `dst_prefixes`
- an install referencing an action nobody defines
- an action or dynvariable name defined by two modules
- a host naming an undeclared plane, or no overlays
- a reserved key written into `[config]`
- a fragment file that could belong to two overlays
- a fragment that is still encrypted, or malformed

It reports, without failing, things that are dead or suspicious but still
coherent — a module with content but no manifest, an install whose path exists in
no overlay, a fragment naming no selected overlay, a generated file left behind
by a rename, two modules or two fragments writing the same variable. A module you
have not wired up yet must not stop you regenerating.

Stale detection is scoped to the file extensions `[output]` produces. The output
directory is not owned outright — a repository may keep prose or
`.gitattributes` beside its generated files, and a tool that calls a
hand-written file stale teaches you to ignore it.

## What this does not do

Deployment. `generate` writes configs; running Dotdrop with the right privilege
for each plane is still yours to drive. A plane could reasonably grow a `become`
field and a `wits dotfiles deploy` to honour it — that is the point at which the
plane abstraction would start paying for itself in more than naming — but until
that command exists the field would be decoration, so it is not there.

Replacing Dotdrop, for now. Everything upstream of the last stage is
backend-agnostic already: the model, the merge rules, and the layout say nothing
about Dotdrop, and `[output]`'s keys name roles — one config per execution
context and machine, one shared value file, one file per overlay that changes
something — that any deployment tool would still need. What is Dotdrop-specific
is the shape of the emitted documents and the `[config]`, `link`, and `chmod`
pass-throughs. A second backend is a second emitter, not a rewrite.
