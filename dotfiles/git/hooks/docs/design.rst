.. _design-book:

Design
======

This document explains *why* the hooks are shaped the way they are. The
:doc:`guide` tells you how to drive them and the :doc:`config` book is the
reference; here the subject is the reasoning behind each structural choice —
the seams, the contracts, and the trade-offs — so that a change to any of it
starts from why a thing is there, not just what it does.

.. contents:: On this page
   :local:
   :depth: 2

What this is, and what it deliberately is not
---------------------------------------------

A single set of global git hooks, shared across every repository through
``core.hooksPath``, that stays modular and coexists with the tools that also
want to own your hooks — ``git-branchless``, Git LFS, Husky, ``git-machete``.
The design problem here is not *how to run a script on commit*; git already
solves that. The problem is keeping a dozen small behaviours — formatting,
linting, secret scanning, protected-branch guards, per-branch bookkeeping —
organised, individually toggleable, and robust against the third-party tools
that rewrite hook files behind your back.

Two non-goals, stated once so they are never re-litigated:

* This is not a hook *manager*. It has no lockfile and no registry; it does not
  compete with Husky, it runs alongside it. Installing, enabling and versioning
  hooks is Husky's job and it already does it — we simply let its output compose
  with ours in the same pipeline.
* It does not reimplement what ``branchless``, ``lfs`` or ``machete`` already
  do. Those are sequenced alongside our own scripts, and the framework steps
  out of their way.

Everything is POSIX ``sh`` and nothing is a build step: the hooks are the
source. There is no manifest to compile and no framework to install — dropping
an executable into a directory is the entire extension mechanism, which is what
keeps the bar for *adding* a behaviour as low as it is.

The dispatch model
------------------

Three pieces, one path through them:

================  ============================================================
Piece             Role
================  ============================================================
``<hook>``        A tiny stub git actually invokes; sources ``core/runner``.
``core/runner``   Resolves the library, buffers stdin, dispatches the layers.
``core/lib.sh``   Shared state and functions every script leans on.
``<hook>.d/*``    The actual behaviours, one executable per concern.
================  ============================================================

When git fires ``pre-commit``, it runs the ``pre-commit`` stub, which sources
``core/runner``. The runner works out which hook it is from the name git
invoked it under — the stub's own filename — loads ``core/lib.sh`` and the
dispatch engine beside it, then walks the execution layers, running every
enabled script in ``pre-commit.d/`` in order.
That is the entire control flow; the rest of the design is about the seams
where the pieces meet.

Why the stubs are real files, not symlinks
------------------------------------------

Each ``<hook>`` entrypoint is a stub that sources ``core/runner`` — a shebang
and one line of code, not a symlink to the runner, not a copy of the logic.
This looks redundant until you
recall who else lives in this ecosystem. ``git branchless init``, ``git lfs
install`` and Husky all *append to or overwrite* hook files as a matter of
course. A symlink would be followed and the shared runner clobbered; inlined
logic would be corrupted by an appended block.

A minimal stub that immediately hands off to the runner makes an appended block
inert, and sets up a clean boundary: the shared, correct logic lives in one
place, and the entrypoints are disposable boilerplate. When an installer does
overwrite a stub outright, recovery is restoring a file whose executable
content is one line, not reconstructing a pipeline.

The branchless bootstrap takes the same idea one step further. Rather than let
``git branchless init`` install its own entrypoints over ours, the hook points
that installation at a dedicated, inactive generated-hooks directory, so the
shared ``core.hooksPath`` scripts remain the *only* active dispatch path. Git
branchless still gets every event it needs — but through our ordered ``.d``
layer, exactly the way the rest of the pipeline delivers them.

The ``.d`` execution contract
-----------------------------

A hook's behaviours live as separate executables in ``<hook>.d/``, run in
filename order. The numeric prefix (``10-``, ``25-``, ``50-``) is pure sort
order; it carries no other meaning, and it deliberately leaves room to slot a
new script between existing ones without renaming the world. The clean name
(prefix stripped) is the address a script answers to when you disable it — see
:ref:`turning-things-off` — so a renumbering never breaks a toggle.

The contract is deliberately blunt: scripts run in order, and **the first
non-zero exit stops the hook with that status.** For a blocking hook
(``pre-commit``, ``pre-push``) that is exactly right — a failed formatter or a
detected secret should abort the commit, and later checks are pointless once one
has already failed. For an advisory hook (``post-checkout``, ``post-merge``) the
same rule means a script that genuinely fails would take the chain down with
it, so those scripts are written to be tolerant: a branchless recorder that
fails is logged and carried on rather than aborting the chain.

Missing tools are handled uniformly. A formatter without its tool is a silent
``exit 0`` — a machine simply may not have ``clang-format``, and that is not
an error. A linter warns before it skips, so you are told a check you asked
for is not running. The integrations skip themselves the same way:
``git-branchless`` looks for its binary and its config file, ``git-lfs`` for
its subcommand.

The asymmetry lives on the failure side. A formatter or linter that runs and
fails blocks the commit — its findings are the point. A branchless recorder
that fails is warned about and the chain carries on, because history recording
must never stand between you and your work. A failing LFS sync stops the hook:
for ``pre-push`` that is the guarantee the script exists to give — a push
never completes with half-uploaded LFS content.

Splitting one concern per file is what makes the disable hierarchy, the
overlays, and the shortness of each script possible: a behaviour is a place,
a toggle, and a few lines you can read in one sitting.

Four layers, one pipeline
-------------------------

The runner dispatches a hook through four layers, in order, and any of them can
abort the run under the fail-fast rule:

1. **Our scripts** — the base ``<hook>.d/`` directory, run in filename order.
2. **Overlay layers** — any ``secret-*`` directories beside the base. An
   overlay is how private or domain-specific behaviour is layered on top of
   the shared set without editing it: drop a ``secret-work/pre-commit.d/``
   next to the base and it runs right after the base scripts. This keeps
   machine- or employer-specific hooks out of the shared tree while still
   going through the same sequencing and toggles.
3. **External hooks** — the escape hatch for project-local conventions. A repo
   that already uses Husky or ``.githooks`` can have those run in the same
   pipeline (both the single-file ``hook`` and split ``hook.d/`` forms are
   understood), and a project with bespoke script paths can map them in
   explicitly. This is the "meet existing projects where they are" decision:
   adopting the framework costs a project nothing and displaces nothing.
4. **The repository's own ``.git/hooks/<hook>``** — anything a tool installed
   the old-fashioned way still runs last, so pointing ``core.hooksPath`` at
   this framework never silently drops a hook a repo was relying on. A
   branchless stub that older initialisations left behind is detected and
   skipped, since branchless is already dispatched from the shared ordered
   layer.

The ordering is the point: shared → overlaid → project-external → repo-local,
each stage additive, none aware of the others. A repository that adopts the
framework gradually — first with its own hooks still in place, then migrating
them outward — sees the same pipeline the whole way.

.. _turning-things-off:

Turning things off
------------------

There are three levels, because "off" means different things at different
scopes: the whole framework, one hook type, or one script. Each level answers
to both git config and an environment variable, and the split between them is
deliberate.

**Git config is for standing preference** — "this repo never runs the
formatter" — and travels with the repo (or your machine). **Environment
variables are for the ephemeral, one-shot override** — "not this once" — and
compose with the command in front of you
(``WITS_HOOKS_PRE_COMMIT_DISABLE=1 git commit ...``). The same env-over-config
precedence runs through the whole codebase, so there is exactly one rule to
remember instead of a different one per setting.

Scripts are addressed by their **clean name** — the filename minus its numeric
prefix (``50-format-python`` → ``format-python``) — so the toggle is stable
even when a script is renumbered. That decoupling is the reason the prefix can
stay a pure ordering device, and it is what lets a per-language check be
toggled with no wiring beyond its filename: ``60-lint-go`` answers to
``wits.hooks.pre-commit.lint-go-disable`` for free.

The enable/disable naming convention itself carries the default. A
``-disable`` key guards a behaviour that is *on* until you set it; an
``-enable`` key one that is *off* until you set it. Reading a key name tells
you its polarity, and the convention keeps the default uniform — always "key
unset" — which means an unset key never needs to be distinguished from an
explicitly-false one.

Repo-scoped state: eager for the few, lazy for the rest
-------------------------------------------------------

Every ``.d`` script is its own process that re-sources ``core/lib.sh`` for its
functions (shell functions cannot cross an ``exec``), and a handful of facts —
the git dir, the common dir, the top level, the current branch, the all-zero
SHA — each cost a ``git`` subprocess to learn. Those resolvers live in
``core/runner``, and it pays for them in one eager pass before any script
runs, in two batches. ``warm_config`` reads the hook's config keys once and
hands them to every script as environment twins; out of that same batch the
runner resolves the kill switch the dispatcher consults for every candidate.
``warm_facts`` then resolves exactly the repo facts the hook's scripts declare
they need: the staged-content cache and current branch on ``pre-commit``, the
null SHA on ``pre-push``, the common dir and top level on ``post-checkout``,
the git dir on ``prepare-commit-msg`` — and every one of them on
``reference-transaction``. Each declaration is one auditable line in the
runner, resolved in the parent and inherited by every child as a plain
``$VAR``.

**Everything else is lazy.** The remaining facts are memoizing getters
(``git_dir``, ``null_sha``, ...) that fill their variable on first use and are
a no-op after; a script calls the getter it needs *after* its early-exit
guards, then reads the plain variable. The placement is the whole point: on a
``reference-transaction`` fire that is not a committed branch deletion — the
overwhelming majority — every script bails at its guard, so not one of those
``git`` subprocesses is ever spawned. This is what keeps the hottest hook cheap
without a value cache to invalidate.

What lazy evaluation deliberately does *not* buy: it cannot stop each child
from re-parsing the library on ``exec``. POSIX ``sh`` has no way to share
compiled functions across processes, and that interpreter cost is the price of
the shell architecture — the only lever against it is compiling the hot path,
which is a separate decision from this one.

One wrinkle worth recording: a getter honours a value already in the
environment, so a hook that recursively drove *another* repository's hooks
would see the outer repo's values. That is already true of the ``GIT_DIR`` that
git itself exports, and no hook here does it — but it is why the getters read
the environment first rather than assuming a single repository forever.

Coexisting with encrypted repositories
---------------------------------------

Some repositories encrypt tracked files through a clean/smudge filter (the
``transcrypt``/``wits transcrypt`` lineage). A hook script that is itself an
encrypted blob at rest has no shebang — it is ciphertext. Rather than trying to
execute that and emitting noise, the runner checks each candidate for a ``#!``
shebang and skips anything without one as "not currently a runnable script."
The check reads only the first bytes and is cheap, and it means the framework
degrades quietly in a repository whose hooks are sealed rather than failing in
a confusing way — the same repository that rehydrates them on checkout sees
them run like any other.

The encrypted-file-modes script takes this cohabitation further. Because the
index records only a file's executable bit, a checkout materialises the rest
of a mode from the umask, so a transcrypt secret lands ``0644`` no matter what
its author intended — and the filter itself cannot fix it, because
clean/smudge see a byte stream, never the file. The hook re-asserts ``0600`` at
the moment the tree changes. The chmod is invisible to ``git status`` because
``600`` vs ``644`` differ only in bits the index drops, so the enforcement does
not dirty the tree. The one batched ``check-attr`` pass over the index is how
the whole cost stays to a single subprocess.

POSIX ``sh``, deliberately
--------------------------

Every script targets POSIX ``sh``, not bash. Hooks run on whatever the user
and their tools happen to invoke — a minimal CI image, a BSD userland, a
``dash`` ``/bin/sh`` — and the one thing worse than a missing feature is a hook
that works on the author's laptop and breaks on a teammate's. Sticking to the
portable subset (and the handful of near-universal externals like ``awk``,
``sed``, ``mktemp``) is a fidelity argument: behave identically everywhere by
not depending on the parts that differ.

The practical tax is small and mostly paid once in the library — careful
``read`` loops, no arrays, portable ``sed`` — so the individual scripts stay
readable. It is the same discipline the rest of this toolset follows, and it is
the reason the same hooks directory feels at home on a full-toolchain
workstation and in a bare CI container.
