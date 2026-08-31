.. _hooks-index:

git hooks
=========

One set of git hooks, shared across every repository on the machine, that
shoulders the routine chores of committing and switching branches — formatting
and linting what you stage, scanning it for secrets and stray conflict markers,
guarding the branches you meant to leave alone, and keeping per-branch
bookkeeping in step with the branch you are actually on.

It is a single directory you point git at once. Every repository from then on
runs the same pipeline, and the pipeline is designed to stay out of your way
until there is something worth stopping you for.

.. contents:: On this page
   :local:
   :depth: 2

What it is
----------

The collection is what a shared ``core.hooksPath`` gives you: one place where
all your hooks live, rather than a copy of them buried in ``.git/hooks`` of
each project. Pointing git at this directory is the whole install:

.. code-block:: console

   $ git config --global core.hooksPath /path/to/git/hooks

No lockfile, no registry, nothing to compile. Each hook is a tiny stub that
hands off to a shared runner (``core/runner``), which walks the enabled
behaviours in ``<hook>.d/`` in filename order; the shared state and helpers live
beside it in ``core/lib.sh``. Adding a behaviour is dropping one executable into
the right directory — that is the entire extension mechanism.

Two external tools ride along on several hooks with nothing for you to
configure: **Git LFS** keeps large objects synced during checkout, merge,
commit and push, and **git-branchless** records history events for its
smartlog and undo. Neither is required for the framework to run; each just
participates where it is installed.

What each hook does
-------------------

The ten entrypoints below are the pipeline. Each row maps a hook to the
behaviours it runs; everything in a ``.d/`` directory is an individual script,
so the table doubles as a map of the scripts.

.. list-table::
   :widths: 22 78
   :header-rows: 1

   * - Hook
     - What it runs
   * - ``pre-commit``
     - The gate that can reject a commit. Block markers, sanity checks, secret
       scan, encoding check, then the formatters and linters for every language
       on your ``PATH``.
   * - ``prepare-commit-msg``
     - Seeds the message before your editor opens: an issue-ID prefix read from
       the branch name (opt-in) and a provenance trailer on merges,
       cherry-picks and reverts.
   * - ``pre-push``
     - Can abort a push. A confirmation prompt for protected branches (opt-in),
       then a Git LFS pre-push sync.
   * - ``post-checkout``
     - After ``git checkout``/``switch`` and after clone. Branchless bootstrap
       and record, LFS sync, encrypted-file modes, the
       ``compile_commands.json`` workspace restore, a dependency lockfile
       warning, and git maintenance enrolment.
   * - ``post-merge``
     - Branchless record, LFS sync, encrypted-file modes, and the same
       dependency lockfile warning as checkout.
   * - ``post-commit``
     - Branchless record, LFS sync.
   * - ``post-applypatch``
     - Branchless record.
   * - ``post-rewrite``
     - Branchless record, a re-assertion of encrypted-file modes (a rebase or
       amend can surface freshly smudged secrets), and the dependency lockfile
       warning (the upstream you rebased onto may have moved them).
   * - ``pre-auto-gc``
     - Branchless record.
   * - ``reference-transaction``
     - Reacts to committed ref changes: branchless record, pruning deleted
       branches from the ``git-machete`` file, and (opt-in) cleaning up a
       deleted branch's build directory.

The ordering within a hook is set by each script's numeric prefix. The list
below is the order ``pre-commit`` runs them in, which shows the shape of a
stage at a glance:

.. list-table::
   :widths: 22 78
   :header-rows: 1

   * - Script
     - Concern
   * - ``10-``
     - Protected-branch warning (opt-in)
   * - ``20-``
     - Sanity checks
   * - ``25-``
     - Block markers
   * - ``30-``
     - Encoding check
   * - ``40-``
     - Secret scan
   * - ``50-``
     - Formatters (clang, python, rust, zig, generic)
   * - ``60-``
     - Linters (python, zig)

The prefix is pure sort order — ``50-`` and ``60-`` simply say "these scripts
are the same stage". It carries no other meaning, and stripping it gives you
the name a script answers to when you disable it (see the :ref:`config-book`).

Reading order
-------------

The book is four documents, written to be read in this order the first time:

.. toctree::
   :maxdepth: 1
   :caption: The book

   guide
   config
   design

.. list-table::
   :widths: 12 88
   :header-rows: 1

   * - Document
     - What it is for
   * - :doc:`guide`
     - How to drive it. Switching it on, what each hook is for, how to tune or
       turn off individual pieces, and how to bring your own hooks into the
       same pipeline. Start here.
   * - :doc:`config`
     - The complete, verifiable reference. Every config key, its default, its
       environment-variable twin, and every per-script toggle in one place.
       Keep this open when you are changing a setting.
   * - :doc:`design`
     - Why it is shaped this way. The stub/runner split, the ``.d`` execution
       contract, the layering, the portability rules, and how it coexists with
       the third-party tools that also want to own your hooks.
   * - This page
     - The overview you are reading now, plus this reading map.

.. warning::

   The docs cover the hooks as they are checked into this repository. Your
   checkout may carry private overlay layers (``secret-*``) or machine-specific
   additions that this book does not document — those are yours, and the
   framework deliberately keeps them invisible here.

Requirements
------------

Everything runs on **POSIX ``sh``** and the handful of near-universal externals
that come with it (``awk``, ``sed``, ``mktemp``). No bash, no build step.

The framework itself needs only git. Everything else is incremental:

.. list-table::
   :widths: 22 78
   :header-rows: 1

   * - Tool
     - Where it is used, and how it is discovered
   * - ``git``
     - Always. Everything else is optional.
   * - ``git-lfs``
     - LFS sync on checkout, merge, commit and push. Runs only when the
       ``git lfs`` subcommand exists.
   * - ``git-branchless``
     - History recording on nearly every hook. Runs only when the tool is
       installed and its config file (``<common-dir>/branchless/config``) is
       present, which ``post-checkout`` initialises for you on first use.
   * - ``gitleaks`` / ``git-secrets``
     - The pre-commit secret scan. Whichever is on your ``PATH`` is used,
       ``gitleaks`` preferred. Installing one is how you turn the scan on.
   * - ``clang-format``, ``rustfmt``, ``zig``, ``ruff``/``black``
     - The per-language formatters and linters. A language is handled only
       when its tool is on your ``PATH``; it is silently skipped otherwise.
   * - ``iconv``
     - The UTF-8 half of the encoding check. When absent, newline checking
       still runs and UTF-8 validation is simply skipped.
   * - ``wits``
     - Resolves build directories (for workspace-restore and build-dir
       cleanup) and edits the ``git-machete`` file (cleanup-machete). When
       absent, those features fall back to their own built-in logic or warn.
   * - ``git-machete``
     - Its definition file is read and pruned by ``reference-transaction``;
       the tool itself never needs to run.

Because each hook only acts on what is actually installed, the same shared
hooks directory works on a laptop with a full toolchain and on a minimal CI
image — the behaviours simply shrink to the tools that exist.
