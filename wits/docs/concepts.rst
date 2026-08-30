.. _concepts:

The shared floor
================

Every command in ``wits`` stands on the same library, ``wits-util``
(``crates/wits-util/``). Its modules are flat on purpose — you name
``wits_util::process`` or ``wits_util::forge`` directly, no grouping layer in
between — but there is a gradient. A thin *floor* holds the primitives every
command leans on: running commands, talking to git, resolving config, doing
crypto, logging, rendering templates, reading the clock. Around it sit larger,
self-contained *subsystems* with real domain logic of their own: the project
registry and resolver, the build systems, the forge layer, the worktree
machinery.

This chapter describes the floor: what each piece exists for, and the one
decision in it that is not obvious — the kind of thing that is invisible in the
code and expensive to rediscover. For the mechanics, read the module; for the
API, read the signatures.

The process layer — running commands, with dry-run baked in
-----------------------------------------------------------

These tools spend most of their time shelling out, and the thing that makes that
fiddly is dry-run. ``-n`` should suppress anything that *changes* the world, but
the read-only queries that decide what to do next must still run — otherwise a
dry-run collapses into a no-op that reports nothing useful.

That tension is the module's reason to exist. You build a command; a read-only
query opts out of the dry-run guard with ``force_run``; everything else is
printed instead of executed when ``-n`` is on. The dry-run preview goes to
**stdout** while logs go to **stderr**, so a plan can be captured cleanly.

There are two ways to run a command. The default captures stdout/stderr — the
right thing for a query whose output you parse. The other inherits the terminal
and returns only an exit code — for a command that *is* an interaction.
Anything that opens an editor or drives an interactive rebase must own the
terminal, so capturing its output would break it. Both honour dry-run.

Git — driven through the CLI, deliberately
------------------------------------------

The tempting alternative is libgit2. ``wits`` does not use it, and the reason is
fidelity rather than weight. A user's real git behaviour is the sum of their
``~/.gitconfig`` includes, conditional includes, credential helpers, and SSH
setup. libgit2 reimplements a subset of that and drifts from the CLI in exactly
the corners — includes, helpers — that config resolution depends on. Spawning
the same ``git`` the user's shell runs means ``wits`` reads precisely what they
would, with no second implementation to keep honest. A process spawn per query
is free next to the work these commands actually do.

There is one handle, ``Repository`` — a cheap wrapper over a repo path — that
carries the whole surface. It reaches from the read/ref floor (config, branch
and ref reads, a commit-log range read for MR titles, the force-with-lease push
— the lease, not a bare force, is the deliberate bit: a stack is rewritten
constantly so non-fast-forward pushes are the norm, yet the lease still refuses
to clobber a remote someone else moved — and the review-fetch ref plumbing) up
to the wider working-tree porcelain the ``project`` / ``build`` / ``update``
actions drive: worktrees, stashes, submodules, branch switches, clone. Those
were once two types in two module trees with overlapping reads; fusing them into
one ``Repository`` removed the duplication, and the file split keeps the two
concerns legible without splitting the type. The three ways a git child is run
— a captured read that answers under dry-run, a captured mutation that keeps
git's error text, a streamed mutation that inherits the terminal and honours
dry-run — are the module's three private primitives.

The larger git-hosting concerns — parsing remote URLs, detecting a forge,
talking to its MR API — deliberately do *not* live on this floor. They sit in
``wits_util::forge`` (remote-URL parsing as ``forge::remote``, beside the forge
detection it feeds), because they carry real domain logic of their own. The
forge layer's shape is laid out in :doc:`reference/stack-design` and
:doc:`reference/review-design`.

Config — where does configuration come from?
--------------------------------------------

There are two entirely different "config" questions, and conflating them is how
a config system rots — so the module answers both, deliberately, at two scopes
that share a purpose but never each other's logic.

**The coarse one — the config *tree*.** Where is the config directory, and what
``*.toml`` files are in it? The search order is the usual
env → XDG → HOME ladder, parameterised per tool by a small ``Root`` struct so a
second subsystem gets the same behaviour just by naming its own variable and
subpaths, not by copying the walk. Discovery returns every nested ``*.toml`` in
sorted order, and a missing root is an empty list rather than an error — an
uninstalled tool simply has nothing to load. What a subsystem then *does* with
those files — route each by section (as ``project`` does) or deep-merge them
into one document — is its own business; this layer only finds them.

**The fine one — a single *setting*.** A value like the encryption password can
live in an environment variable or in git config, and you want one predictable
precedence order (env over git) that has no bootstrap loop — the resolver must
not itself need config in order to find config. The subtle part is context
isolation: a repository can hold several independent secret sets — ``default``,
``prod``, and so on. When a non-default context is active, the resolver
**refuses** to fall back to the bare, context-less key. That fallback would
silently hand a ``prod`` operation the ``default`` password and encrypt data
under the wrong key; the bare key is only consulted for the ``default`` context.

The two halves stay strictly separate in code — the directory search knows
nothing of secrets, and the setting lookup knows nothing of ``*.toml`` trees.
They live in one module because they are the same question at two granularities,
not because they share machinery.

Crypto — authenticated encryption shaped by git filters
-------------------------------------------------------

Two domain constraints drive everything here.

**Compatibility.** Repositories already hold data encrypted by the earlier
``transcrypt`` tool. The packet layout, the algorithm-name spellings, and the
default PBKDF2 iteration count are a frozen wire format: reproduce them exactly
or that data becomes unreadable. This is why a few constants look arbitrary —
they are, and they cannot change. The PBKDF2 default of 99 989 iterations is
the value the original tool shipped; old repositories were encrypted with it.

**Determinism.** A clean filter runs on every ``git add``. If encrypting
unchanged content produced fresh randomness, git would see the file as modified
forever. So the default mode derives salt and IV from the content itself: same
input, same output, no phantom diffs. The cost is that identical plaintext is
observably identical once encrypted — fine here, and the price of a filter that
does not fight git. The derivation also folds in the file path as the AEAD's
*additional data*, binding a ciphertext to its location so a moved blob fails to
authenticate instead of silently decrypting.

There is also an explicit random mode for callers off the filter path — ones
that can afford fresh randomness and want the stronger guarantee.

Log — two global switches and a stream split
--------------------------------------------

``--verbose`` and ``--dry-run`` are genuinely global to a run, and threading
them through every call site would be noise, so they live in two process-wide
atomics set once at startup. Everything else in the module follows from that.

The one decision worth recording is the split of streams. Ordinary log lines go
to **stderr**; the dry-run preview of a command that *would* run goes to
**stdout**. That way ``wits … -n`` can be captured or piped as a clean plan
without log chatter mixed in. The level policy matches the split's intent:
``info`` — the normal per-action feedback (``pushed X``, ``created MR``, each
build step) — is shown by default, and only ``debug`` is gated behind
``--verbose``. Getting that gate wrong once made every command silent on
success; it is now pinned by a test.

Template — a small ``{{ }}`` / ``[[ ]]`` engine with no domain knowledge
------------------------------------------------------------------------

Project config is full of values that reference other values — a ``build_dir``
built from ``{{repos.main.workdir}}`` and ``{{build_type}}``, an environment
entry computed from another. Rather than bake that into the project layer, the
substitution engine is a floor primitive that knows nothing about projects: it
resolves ``{{ dotted.path }}`` lookups and ``[[ arithmetic ]]`` expressions
against an opaque value tree. The project layer supplies the tree; the engine
answers lookups. The scaffold plugin does *not* use this engine — it generates
text and needs real loops and conditionals, so it gets a Jinja engine instead
(``minijinja``). Config resolution does not.

Two decisions carry the weight. Resolution is **lazy** — a context entry may
itself be a template that references another — so the order entries appear in a
map never matters; each is resolved on demand and memoised. And because
laziness invites loops, the engine keeps a path stack and turns a self-reference
cycle into a hard error rather than a stack overflow. Unknown paths and type
mismatches are likewise hard failures, so a typo surfaces at resolution instead
of silently rendering empty.

The ``[[ … ]]`` expressions are deliberately tiny: ``+ - * / // %``,
comparisons, and the functions ``min max int float str bool``. Anything bigger
belongs in the project layer's structured ``applies_when`` blocks, not in an
ad-hoc expression language.

Time — one clock, and the age a user writes at it
-------------------------------------------------

Two commands sweep stale things — ``review prune`` drops dormant MRs,
``worktree prune`` reclaims dormant checkouts — and both spell "how stale" the
same way, with the same ``--older-than`` flag. That shared spelling is the
module's reason to exist: the parser lives once rather than beside each caller.

Time is whole Unix seconds everywhere, so a stored timestamp and a computed
cutoff are one type that compares directly. Absolute dates need no date crate —
an ISO day count is pure arithmetic, and a Gregorian month-length check rejects
an impossible date like ``2026-02-31`` rather than silently over-counting.

The module also renders an age back the other way, for ``worktree info``'s
panels. That direction is deliberately **coarse** — one unit, the largest that
fills — so ``3 weeks ago`` rather than ``3 weeks, 2 days ago``. It exists to
answer "is this stale?", where the unit carries the whole message and a second
component is noise. A timestamp in the future reads as ``just now``: it means a
clock skewed somewhere, and "in 3 days" would draw the eye to a fact about the
clock rather than about the thing being described.

The spellings: a **day count** (``30``, ``30d``, ``4w``) or an **ISO-8601
date** (``2026-06-01``). The one non-obvious decision is a refusal — a bare,
unit-less four-digit number is rejected instead of being read as a day count:
``--older-than 2026`` is almost always a mistyped date, and silently meaning
about five-and-a-half years is worse than an error that asks for ``2026d`` or
``2026-01-01``. An unreadable clock yields ``0`` rather than an error, because
every consumer builds a *cutoff* from it and the epoch is the value that selects
nothing instead of sweeping everything.

Host facts — the one place wits probes the machine
--------------------------------------------------

``system`` builds a single tree of what can be learned about the host — os,
cpu, memory, gpu, distro, power, hostname, desktop — best-effort and
Linux-first. The project layer exposes it as the ``system.*`` template
namespace (``system.os.name``, ``system.os.kernel.*``, ``system.cpu.count``,
``system.cpu.arch``, ``system.mem.mb``/``gb``, ``system.gpu.list``,
``system.distro.id``, ``system.power.laptop``, …). Detection is deliberately
one module and one pass, so the machine is probed exactly once per process and
a dotted path addresses one value unambiguously.

The two unification pairs
-------------------------

Two pairs that were once separate modules are unified where they belong:
``config`` folds in the single-setting resolver (both answer *where does this
come from?*), and the git-remote parsing that feeds forge detection lives in
``forge::remote``, beside the forge it serves — because a remote URL is the
same git-hosting concern whether you are parsing it or talking to it.
