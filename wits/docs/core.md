# The shared floor

The `wits-util` crate (`crates/wits-util/`) is the shared library behind the
`wits` binary and its plugins. Its modules are flat, but there is a gradient:
this document covers the thin *floor* — `process`, `git`, `config`, `crypto`,
`log`, `template` — the primitives the commands and subsystems lean on.
It records *why* each one exists and the one decision in it that isn't obvious —
the kind of thing that is invisible in the code and expensive to rediscover. For
the mechanics, read the module; for the API, read the signatures.

## `process` — running commands, with dry-run baked in

These tools spend most of their time shelling out, and the thing that makes that
fiddly is dry-run. `-n` should suppress anything that *changes* the world, but
the read-only queries that decide what to do next must still run — otherwise a
dry-run collapses into a no-op that reports nothing useful.

That tension is the module's reason to exist. A command is built, and a
read-only query opts out of the dry-run guard with `force_run`. Everything else
is printed instead of executed when `-n` is on. The dry-run preview goes to
stdout while logs go to stderr, so a plan can be captured cleanly.

There are two ways to run. The default captures stdout/stderr — the right thing
for a query whose output we parse. The other inherits the terminal and returns
only an exit code, for a command that *is* an interaction: anything that opens an
editor or drives an interactive rebase must own the terminal, so capturing its
output would break it. Both honour dry-run.

## `git` — driven through the CLI, deliberately

The tempting alternative is libgit2. We don't, and the reason is fidelity rather
than weight. A user's real git behaviour is the sum of their `~/.gitconfig`
includes, conditional includes, credential helpers and SSH setup. libgit2
reimplements a subset and drifts from the CLI in exactly the corners (includes,
helpers) that config resolution depends on. Spawning the same `git` the user's
shell runs means we read precisely what they would, with no second
implementation to keep honest. A process spawn per query is free next to the
work these commands actually do.

There is one handle, `Repository` — a cheap wrapper over a repo path — carrying
the whole surface. It reaches from the read/ref floor (config, branch and ref
reads, a commit-log range read for MR titles, the force-with-lease push — the
lease, not a bare force, is the deliberate bit: a stack is rewritten constantly
so non-fast-forward pushes are the norm, yet the lease still refuses to clobber a
remote someone else moved — and the review-fetch ref plumbing) up to the wider
working-tree porcelain the `project`/`build`/`update` actions drive (worktrees,
stashes, submodules, branch switches, clone). Those were once *two* types in two
module trees (`git` and `project::git`) with overlapping reads; fusing them into
one `Repository` removes that duplication, and the file split (`git/floor.rs` vs
`git/worktree.rs`) keeps the two concerns legible without splitting the type. The
three ways a git child is run — a captured read that answers under dry-run, a
captured mutation that keeps git's error text, a streamed mutation that inherits
the terminal and honours dry-run — are the module's three private primitives.

The larger git-hosting concerns — parsing remote URLs, detecting a forge,
talking to its MR API — deliberately do *not* live on this floor. They sit in
`wits_util::forge` (remote-URL parsing is `forge::remote`, beside the forge
detection it feeds), because they carry real domain logic of their own. See
`docs/stack/design.md` for that layer's shape.

## `config` — where does configuration come from?

There are two entirely different "config" questions, and conflating them is how a
config system rots — so this one module answers both, deliberately, at two
scopes that share a purpose but never each other's logic.

**The coarse one — the config *tree*.** *Where is the config directory, and what
`*.toml` files are in it?* The search order is the usual env → XDG → HOME ladder,
parameterised per tool by a `Root { env, xdg, home }` so a second subsystem gets
the same behaviour just by naming its own variable and subpaths, not by copying
the walk. Discovery returns every nested `*.toml` in sorted order, and a missing
root is an empty list rather than an error — an uninstalled tool simply has
nothing to load. What a subsystem then *does* with those files — route each by
section (as `project` does) or deep-merge them into one document — is its own
business; this layer only finds them.

**The fine one — a single *setting*** (`Resolver`). *What is the value of one
setting?* A value like the encryption password can live in an environment
variable or in git config, and we want one predictable precedence order (env over
git) with no bootstrap loop — the resolver can't itself need config to find
config. The subtle part is context isolation: a repository can hold several
independent secret sets — `default`, `prod`, and so on — and when a non-default
context is active the resolver refuses to fall back to the bare, context-less
key. Falling back would silently hand a `prod` operation the `default` password
and encrypt data under the wrong key; the bare key is only consulted for the
default context.

The two halves stay strictly separate in code (the directory search knows nothing
of secrets, and the setting lookup knows nothing of `*.toml` trees) — they live in
one module because they are the same question at two granularities, not because
they share machinery.

## `crypto` — authenticated encryption shaped by git filters

Two domain constraints drive everything here.

**Compatibility.** Repositories already hold data encrypted by the earlier
`transcrypt` tool. The packet layout, the algorithm-name spellings, and the
default PBKDF2 iteration count are therefore a frozen wire format: reproduce them
exactly or that data becomes unreadable. This is why a few constants look
arbitrary — they are, and they can't change.

**Determinism.** A clean filter runs on every `git add`. If encrypting unchanged
content produced fresh randomness, git would see the file as modified forever.
So the default mode derives salt and IV from the content itself: same input,
same output, no phantom diffs. The cost is that identical plaintext is
observably identical once encrypted — fine here, and the price of a filter that
doesn't fight git. The derivation also folds in the file path as the AEAD's
additional data, binding a ciphertext to its location so a moved blob fails to
authenticate instead of silently decrypting.

## `log` — two global switches and a stream split

`--verbose` and `--dry-run` are genuinely global to a run, and threading them
through every call site would be noise, so they live in two process-wide atomics
set once at startup. Everything else here follows from that.

The one decision worth recording is the split of streams. Ordinary log lines go
to **stderr**; the dry-run preview of a command that *would* run goes to
**stdout**. That way `wits … -n` can be captured or piped as a clean plan without
log chatter mixed in. The level policy matches the split's intent: `info` — the
normal per-action feedback (`pushed X`, `created MR`, each build step) — is
shown by default, and only `debug` is gated behind `--verbose`. (Getting that
gate wrong once made every command silent on success; it is now pinned by a
test.)

## `template` — a zero-domain `{{ }}` / `[[ ]]` engine

Project config is full of values that reference other values — a `build_dir`
built from `{{repos.main.workdir}}` and `{{build_type}}`, an environment entry computed
from another. Rather than bake that into the project layer, the substitution
engine is a floor primitive that knows nothing about projects: it resolves
`{{ dotted.path }}` lookups and `[[ arithmetic ]]` expressions against an opaque
`Value` tree.

Two decisions carry the weight. Resolution is **lazy** — a context entry may
itself be a template that references another — so the order entries appear in a
map never matters; each is resolved on demand and memoised. And because laziness
invites loops, the engine keeps a path stack and turns a self-reference cycle
into a hard error rather than a stack overflow. Unknown paths and type
mismatches are likewise hard failures, so a typo surfaces at resolution instead
of silently rendering empty.

## `time` — one clock, and the age a user writes at it

Two commands sweep stale things — `review prune` drops dormant MRs, `worktree
prune` reclaims dormant checkouts — and both spell "how stale" the same way. That
shared spelling is the module's reason to exist: the parser lives once rather
than beside each caller.

Time is whole Unix seconds everywhere, so a stored timestamp and a computed
cutoff are one type that compares directly. Absolute dates need no date crate —
an ISO day count is pure arithmetic (Hinnant's `days_from_civil`), and a
Gregorian month-length check rejects an impossible date like `2026-02-31` rather
than silently over-counting.

The module also renders an age back the other way, for `worktree info`'s panels.
That direction is deliberately **coarse** — one unit, largest that fills, so
`3 weeks ago` rather than `3 weeks, 2 days ago`. It exists to answer "is this
stale?", where the unit carries the whole message and a second component is
noise. A timestamp in the future reads as `just now`: it means a clock skewed
somewhere, and "in 3 days" would draw the eye to a fact about the clock rather
than about the thing being described.

The one non-obvious decision is a refusal. A bare, unit-less four-digit number is
rejected instead of being read as a day count: `--older-than 2026` is almost
always a mistyped date, and silently meaning ~5.5 years is worse than an error
that asks for `2026d` or `2026-01-01`. An unreadable clock yields `0` rather than
an error, because every consumer builds a *cutoff* from it and the epoch is the
value that selects nothing instead of sweeping everything.
