# Git hooks — Design

This file explains *why* the hook framework is shaped the way it is. The
companion [guide](guide.md) explains *how to drive it* and carries the full,
reader-facing reference — every config key, every environment variable, every
script. Neither restates the other: behaviour-for-users goes there, rationale
goes here.

---

## 1. What this is, and what it deliberately is not

A single set of global git hooks, shared across every repository through
`core.hooksPath`, that stays modular and coexists with the tools that also want
to own your hooks — `git-branchless`, Git LFS, Husky, `git-machete`. The design
problem is not "run a script on commit"; git already does that. It is keeping a
dozen small behaviours — formatting, linting, secret scanning, protected-branch
guards, per-branch bookkeeping — organized, individually toggleable, and robust
against the third-party tools that rewrite hook files behind your back.

Everything is POSIX `sh` (§9) and nothing is a build step: the hooks are the
source. There is no framework to install, no manifest to compile — dropping an
executable into a directory is the entire extension mechanism (§4).

Non-goals, stated once: this is not a hook *manager* with a lockfile and a
registry (that is Husky's job, and we integrate with it rather than compete,
§5). It does not reimplement what `branchless`/`lfs`/`machete` already do — it
sequences them alongside our own scripts and gets out of the way.

## 2. The dispatch model

Three pieces, one path through them:

```
<hook>            a tiny stub git actually invokes  →  sources core/runner
core/runner       resolves the library, buffers stdin, dispatches the layers
core/lib.sh       shared state + functions every script leans on
<hook>.d/*        the actual behaviours, one executable per concern
```

When git fires `pre-commit`, it runs the `pre-commit` stub, which sources
`core/runner`. The runner works out which hook it is from its own name, loads
`core/lib.sh`, and then walks the execution layers (§5), running every enabled
script in `pre-commit.d/` in order. That is the whole control flow; the rest of
the design is about the seams.

## 3. Why the stubs are real files, not symlinks

Each `<hook>` entrypoint is a three-line stub that sources `core/runner` — not
a symlink to it, and not a copy of the logic. This looks redundant until you
remember what lives in this ecosystem. `git branchless init`, `git lfs install`,
and Husky all *append to or overwrite* hook files as a matter of course. A
symlink would be followed and the shared runner clobbered; inlined logic would
be corrupted by an appended block.

A minimal stub that immediately hands off to the runner — which does the whole
job and then exits — makes the framework robust to that tampering. Whatever a
tool appends after the hand-off is inert: the runner has already run and exited
by the time control would reach it. So `git branchless init` re-appending its
block to `post-commit` is harmless noise, and the branchless integration is
instead wired the *supported* way, as a `post-commit.d/00-git-branchless` script
under our own control. The stub is a firebreak, and the cost of it is two lines.

## 4. The `.d` execution contract

A hook's behaviours live as separate executables in `<hook>.d/`, run in
filename order. The numeric prefix (`10-`, `25-`, `50-`) is just sort order and
carries no other meaning; it leaves room to slot things between without renaming
the world.

The contract is deliberately blunt: scripts run in order, and **the first
non-zero exit stops the hook with that status.** For a blocking hook
(`pre-commit`, `pre-push`) that is exactly right — a failed formatter or a
detected secret should abort the commit, and later checks are pointless once one
has already failed. For an advisory hook (`post-checkout`, `post-merge`) the
same rule means a script that genuinely fails takes the chain down with it, so
those scripts are written to be tolerant: a failure in a non-essential step (a
branchless recorder, an LFS sync) is logged and carried on rather than aborting
the chain. The one exception is the formatter/linter `command -v` guards — a
missing language tool is a silent `exit 0` because formatting and linting are
environment-dependent; git itself, git-lfs, and git-branchless are assumed
present.

Splitting one concern per file is what makes the disable hierarchy (§6) and the
overlays (§5) possible, and what keeps each script short enough to read in one
sitting.

## 5. Three layers, one pipeline

The runner dispatches a hook through three layers, in order, and any of them can
abort the run under the fail-fast rule:

1. **Our scripts** — the base `<hook>.d/` directory, plus any `secret-*` overlay
   directories beside it. An overlay is how private or domain-specific behaviour
   is layered on top of the shared set without editing it: drop a
   `secret-work/pre-commit.d/` next to the base and it runs after the base
   scripts. This keeps machine- or employer-specific hooks out of the shared
   tree while still going through the same sequencing and toggles.
2. **External hooks** — the escape hatch for project-local conventions. A repo
   that already uses Husky or `.githooks` can have those run in the same
   pipeline (both single-file `hook` and split `hook.d/` forms), and a project
   with bespoke script paths can map them in explicitly. We meet existing
   projects where they are instead of demanding they convert.
3. **The repo's own `.git/hooks/<hook>`** — anything a tool installed the
   old-fashioned way still runs last, so setting `core.hooksPath` to this
   framework never silently drops a hook a repo was relying on.

The ordering is the point: shared → overlaid → project-external → repo-local,
each stage additive, none aware of the others.

## 6. Turning things off

Three levels, because "off" means different things at different scopes: the
whole framework, one hook type, or one script. Each level answers to both git
config and an environment variable, and the split between them is deliberate.

**Git config is for standing preference** — "this repo never runs the
formatter" — and lives with the repo (or globally). **Environment variables are
for the ephemeral, one-shot override** — "not this once" — and compose with the
command in front of you (`WITS_HOOKS_PRE_COMMIT_DISABLE=1 git commit …`). The
same env-over-config precedence runs through the whole codebase; hooks follow it
so there is one rule to remember.

Scripts are addressed by their **clean name** — the filename minus its numeric
prefix (`50-format-python` → `format-python`) — so the toggle is stable even
when a script is renumbered. That decoupling is the reason the prefix can stay a
pure ordering device (§4), and it is what lets a per-language check be toggled
with no wiring beyond its filename.

## 7. Repo-scoped state: eager for the few, lazy for the rest

`core/lib.sh` resolves a handful of facts — the git dir, the common dir, the
top-level, the current branch, the all-zero SHA, the protected-branch pattern,
the global kill switch — each of which costs a `git` (or config) subprocess. The
runner sources the library once, then execs each `.d` script as its own process,
and every one of those re-sources the library to get its functions (shell
functions cannot cross an `exec`).

The split that matters: **only two things are resolved eagerly and exported** —
the kill switch (the dispatcher consults it for every candidate) and, on
`pre-commit`, the staged-content cache (every content script shares it). A child
inherits those and never re-runs the guarded block. **Everything else is lazy.**
Each remaining fact is a memoizing getter (`git_dir`, `null_sha`, …) that fills
its variable on first use and is a no-op after; a script calls the getter it
needs *after* its early-exit guards and then reads the plain variable. The
placement is the whole point: on a `reference-transaction` fire that isn't a
committed branch deletion — the overwhelming majority — every script bails at its
guard, so not one of those `git` subprocesses is ever spawned. This is what
keeps the hottest hook cheap without a value cache to invalidate.

What lazy evaluation deliberately does *not* buy: it cannot stop each child from
re-parsing the library on `exec` (POSIX `sh` has no way to share compiled
functions across processes). That interpreter cost is the price of the shell
architecture; the only lever against it is compiling the hot path, which is a
separate decision from this one.

The one wrinkle worth recording: a getter honours a value already in the
environment, so a hook that recursively drove *another* repository's hooks would
see the outer repo's values. That is already true of the `GIT_DIR` git itself
exports, and no hook here does that — but it is why the getters read the
environment first rather than assuming a single repo forever.

## 8. Coexisting with encrypted repositories

Some repositories encrypt tracked files through a clean/smudge filter (the
`transcrypt`/`wf transcrypt` lineage). A hook script that is itself an encrypted
blob at rest has no shebang — it is ciphertext. Rather than trying to execute
that and emitting noise, the runner checks each candidate for a `#!` shebang and
skips anything without one as "not currently a runnable script." The check is
cheap and reads only the first bytes, and it means the framework degrades
quietly in a repo whose hooks are sealed rather than failing in a confusing way.

## 9. POSIX `sh`, deliberately

Every script targets POSIX `sh`, not bash. Hooks run on whatever the user and
their tools happen to invoke — a minimal CI image, a BSD userland, a `dash`
`/bin/sh` — and the one thing worse than a missing feature is a hook that works
on the author's laptop and breaks on a teammate's. Sticking to the portable
subset (and the handful of near-universal externals like `awk`, `sed`, `mktemp`)
is the same fidelity argument the rest of this toolset makes: behave identically
everywhere by not depending on the parts that differ.

The practical tax is small and mostly paid in the library — a cross-platform
`sed -i`, careful `read` loops, no arrays — so the individual scripts stay
readable.
