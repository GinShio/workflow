.. _config-book:

Configuration reference
=======================

Every setting the hooks honour, in one place, written out in full so nothing
has to be inferred. This is the document to keep open while you change
something; the :doc:`guide` explains what each behaviour *does* and when you
would want it.

Two rules govern everything here, stated once and then assumed:

* Every key lives under the ``wits.hooks.`` namespace, and every key has an
  **environment-variable twin** that, when set, *overrides* the config value for
  that one command. The twin is the config key upper-cased with every ``-`` and
  ``.`` turned into ``_``.
* A boolean switch's default is decided by its own suffix: a ``-disable`` key
  means a behaviour that is **on by default**, an ``-enable`` key one that is
  **off by default**. "Key unset" is always the default; set a key only to move
  *off* it.

The environment twin is omitted from the per-key tables below, because the
transform is mechanical and always applies: for any key
``wits.hooks.pre-commit.format-python-disable`` the twin is simply
``WITS_HOOKS_PRE_COMMIT_FORMAT_PYTHON_DISABLE``. The shared keys in the next
section are written out only because more than one hook reads them — their
twins follow the same rule.

.. contents:: On this page
   :local:
   :depth: 2

Disable hierarchy
-----------------

The switches below answer to three scopes, from widest to narrowest. Whichever
scope a hook consults, config is the standing value and the environment twin is
the one-shot override. The scopes only narrow what they silence — any matching
switch that is set turns the behaviour off, and no scope outranks another — so
the way back on is always to unset the switch.

.. list-table::
   :widths: 34 24 42
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.disable``
     - off
     - Master switch. When true, no hook — built-in, overlay, external, or
       local — runs at all.
   * - ``wits.hooks.<hook>.disable``
     - off
     - Turns off one hook entirely, e.g.
       ``wits.hooks.pre-commit.disable``.
   * - ``wits.hooks.<hook>.<script>-disable``
     - off
     - Turns off one script within a hook, addressed by its clean name (numeric
       prefix stripped), e.g.
       ``wits.hooks.pre-commit.format-python-disable``.

Shared keys
-----------

These are read by more than one hook, or shape the behaviour of the whole run.
Their twins are listed here because several places depend on the same key —
even though each twin is still the plain mechanical transform:

.. list-table::
   :widths: 34 24 42
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.protected-branch``
     - ``^(main|master|dev|release-.*|patch-.*)$``
     - An extended regular expression (matched with ``grep -E``) a branch name
       must match to be treated as protected. Shared by the ``pre-commit`` and
       ``pre-push`` prompts. **Twin** ``WITS_HOOKS_PROTECTED_BRANCH`` (this one
       is identical to the mechanical transform, listed here because both
       prompts depend on it).
   * - ``wits.hooks.external-dirs``
     - — (unset)
     - A colon-separated list of directories to scan for external hooks, each
       absolute or relative to the repo root. In each, a single file named
       ``<hook>`` and/or a ``<hook>.d/`` directory run for that hook.
       **Twin** ``WITS_HOOKS_EXTERNAL_DIRS`` (identical to the transform).
   * - ``wits.hooks.<hook>.external-scripts``
     - — (unset)
     - A colon-separated list of explicit external hook scripts for one hook,
       absolute or relative to the repo root. They run in the order listed.
       **Twin** ``WITS_HOOKS_<HOOK>_EXTERNAL_SCRIPTS`` — the mechanical transform
       applied through the hook name, e.g.
       ``WITS_HOOKS_PRE_COMMIT_EXTERNAL_SCRIPTS``.

pre-commit
----------

The blocking gate that runs after you stage and before the commit is recorded.
Its scripts and their keys:

.. list-table::
   :widths: 40 20 40
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.pre-commit.warn-protected-enable``
     - off
     - Ask for confirmation before committing while a protected branch (per
       ``wits.hooks.protected-branch``) is checked out. ``-enable``: off until
       set.
   * - ``wits.hooks.pre-commit.sanity-checks-max-file-size``
     - ``26214400``
     - Size ceiling in bytes for a staged file; anything larger blocks the
       commit. 25 MiB by default.
   * - ``wits.hooks.pre-commit.format-clang-style``
     - — (unset)
     - A named style (``llvm``, ``google``, …) for ``clang-format`` when the
       repo has no ``.clang-format`` of its own. When unset, format
       discovery falls back to ``-style=file`` then LLVM.
   * - ``wits.hooks.pre-commit.format-clang-whole-file-enable``
     - off
     - Format the whole staged C/C++ file instead of only the commit's
       diff-scoped window (the ``-U3`` hunks). ``-enable``: off until set.
   * - ``wits.hooks.pre-commit.format-generic-notrim``
     - — (unset)
     - A space-separated list of extra file extensions whose trailing
       whitespace the generic pass must preserve (on top of the built-in
       Markdown/CSV/TSV set).

The per-script toggles for ``pre-commit``:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key (under ``wits.hooks.pre-commit.``)
     - Turns off
   * - ``warn-protected-disable``
     - the protected-branch prompt (``10-warn-protected``)
   * - ``sanity-checks-disable``
     - the sanity checks (``20-sanity-checks``)
   * - ``block-checks-disable``
     - the marker guard (``25-block-checks``)
   * - ``encoding-disable``
     - the encoding check (``30-encoding``)
   * - ``secret-scan-disable``
     - the secret scan (``40-secret-scan``)
   * - ``format-clang-disable``
     - the C/C++ formatter (``50-format-clang``)
   * - ``format-generic-disable``
     - the generic whitespace pass (``50-format-generic``)
   * - ``format-python-disable``
     - the Python formatter (``50-format-python``)
   * - ``format-rust-disable``
     - the Rust formatter (``50-format-rust``)
   * - ``format-zig-disable``
     - the Zig formatter (``50-format-zig``)
   * - ``lint-python-disable``
     - the Python linter (``60-lint-python``)
   * - ``lint-zig-disable``
     - the Zig linter (``60-lint-zig``)

prepare-commit-msg
------------------

Runs as git assembles the initial message, before your editor opens.

.. list-table::
   :widths: 40 18 42
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-enable``
     - off
     - Seed the message with an issue ID read from the branch name.
       ``-enable``: off until set.
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-regex``
     - ``[A-Z]+-[0-9]+``
     - What to pull from the branch name; the first match wins.
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-position``
     - ``prepend``
     - Where the ID goes: ``prepend`` (start of the subject) or ``append``
       (a git trailer at the end of the body).
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-format``
     - ``[%s] `` / ``Refs: %s``
     - How to wrap the ID. The first ``%s`` is replaced by the ID; everything
       else is literal. Default depends on position: ``[%s] `` when prepending,
       ``Refs: %s`` when appending (should stay trailer-shaped, ``Token: %s``).
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-default``
     - — (unset)
     - A fallback ID to use when the branch name carries none. In ``append``
       mode it is added only when no issue trailer exists yet, never stacked
       onto a real branch-derived reference.

The per-script toggles:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key (under ``wits.hooks.prepare-commit-msg.``)
     - Turns off
   * - ``issue-tracker-disable``
     - the issue-ID prefix (``10-issue-tracker``)
   * - ``provenance-disable``
     - the provenance trailer (``20-provenance``)

pre-push
--------

Runs before git hands refs to a remote, and can abort the push.

.. list-table::
   :widths: 40 20 40
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.pre-push.warn-protected-enable``
     - off
     - Ask for confirmation before pushing to a protected branch (per
       ``wits.hooks.protected-branch``) on the remote. ``-enable``: off until
       set.

The per-script toggles (``wits.hooks.pre-push.``):

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key
     - Turns off
   * - ``warn-protected-disable``
     - the protected-branch prompt (``10-warn-protected``)
   * - ``git-lfs-disable``
     - the Git LFS pre-push sync (``20-git-lfs``)

post-checkout
-------------

Runs after ``git checkout``/``switch`` and after ``clone``. Advisory — nothing
here blocks the operation.

No value keys to configure; every script is quiet and automatic. The per-script
toggles (``wits.hooks.post-checkout.``):

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key
     - Turns off
   * - ``branchless-init-disable``
     - the branchless bootstrap (``00-branchless-init``)
   * - ``git-branchless-disable``
     - the branchless recorder (``01-git-branchless``)
   * - ``git-lfs-disable``
     - the Git LFS sync (``20-git-lfs``)
   * - ``encrypted-modes-disable``
     - the encrypted-file modes (``25-encrypted-modes``)
   * - ``workspace-restore-disable``
     - the ``compile_commands.json`` restore (``80-workspace-restore``)
   * - ``check-dependencies-disable``
     - the dependency-change warning (``85-check-dependencies``)
   * - ``maintenance-disable``
     - the maintenance enrolment (``90-maintenance``)

post-merge
----------

Runs after a merge. Per-script toggles (``wits.hooks.post-merge.``); the
``25-encrypted-modes`` and ``85-check-dependencies`` entries are the same
scripts as ``post-checkout``'s:

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key
     - Turns off
   * - ``git-branchless-disable``
     - the branchless recorder (``00-git-branchless``)
   * - ``git-lfs-disable``
     - the Git LFS sync (``20-git-lfs``)
   * - ``encrypted-modes-disable``
     - the encrypted-file modes (``25-encrypted-modes``)
   * - ``check-dependencies-disable``
     - the dependency-change warning (``85-check-dependencies``)

post-commit / post-applypatch / post-rewrite / pre-auto-gc
-----------------------------------------------------------

The recorder hooks — mostly pass-through to branchless (and LFS where noted).
Per-script toggles:

.. list-table::
   :widths: 40 26 34
   :header-rows: 1

   * - Hook
     - Toggle key (under ``wits.hooks.<hook>.``)
     - Turns off
   * - ``post-commit``
     - ``git-lfs-disable``
     - the Git LFS sync (``20-git-lfs``)
   * - ``post-rewrite``
     - ``encrypted-modes-disable``
     - the encrypted-file modes (``25-encrypted-modes``)
   * - ``post-rewrite``
     - ``check-dependencies-disable``
     - the dependency-change warning (``85-check-dependencies``)

Every one of these hooks also carries a ``git-branchless-disable`` toggle for
its ``00-git-branchless`` recorder (which is always present), and each answers
to its own ``wits.hooks.<hook>.disable``.

reference-transaction
---------------------

Fires whenever refs change; acts only on committed branch deletions.

.. list-table::
   :widths: 40 20 40
   :header-rows: 1

   * - Full key
     - Default
     - Meaning
   * - ``wits.hooks.reference-transaction.cleanup-build-dir-enable``
     - off
     - Remove a branch's build directory (resolved through the ``wits``
       project registry) when that branch is deleted. ``-enable``: off
       until set, because it deletes files.

The per-script toggles (``wits.hooks.reference-transaction.``):

.. list-table::
   :widths: 34 66
   :header-rows: 1

   * - Toggle key
     - Turns off
   * - ``git-branchless-disable``
     - the branchless recorder (``00-git-branchless``)
   * - ``cleanup-machete-disable``
     - the machete pruning (``50-cleanup-machete``)
   * - ``cleanup-build-dir-disable``
     - the build-directory cleanup (``60-cleanup-build-dir``)

External hooks
--------------

How project-local hooks join the pipeline. Both take colon-separated entries,
absolute or relative to the repo root:

.. list-table::
   :widths: 40 60
   :header-rows: 1

   * - Full key
     - Meaning
   * - ``wits.hooks.external-dirs``
     - Directories to scan for external hooks (Husky ``.husky``, ``.githooks``,
       …). A scanned directory's single-file hook is toggled under the
       pseudo-name ``external`` (``WITS_HOOKS_PRE_COMMIT_EXTERNAL_DISABLE=1``);
       its ``.d/`` scripts under their own filenames.
   * - ``wits.hooks.<hook>.external-scripts``
     - Explicit script paths for one hook, run in the listed order. Toggled
       under their own filenames.
   * - ``wits.hooks.<hook>.local-disable``
     - The repository's own legacy hook (``.git/hooks/<hook>``), which runs
       last of all, answers to the pseudo-name ``local`` — set this to skip it
       while every other layer keeps running.

Logging
-------

Diagnostics go to **stderr** and are gated by one environment variable (there
is no config key; it is per-command by nature):

.. list-table::
   :widths: 22 78
   :header-rows: 1

   * - ``WITS_HOOKS_LOG_LEVEL``
     - Meaning
   * - ``0``
     - Silent.
   * - ``1``
     - Errors only.
   * - ``2``
     - Errors and warnings — the default.
   * - ``3``
     - ...plus info notes.
   * - ``4``
     - ...plus a debug trace, including a shell trace of the scripts.

.. code-block:: console

   $ WITS_HOOKS_LOG_LEVEL=3 git commit -m "message"

Environment-variable twins
--------------------------

As a worked example, here is a representative spread of keys and their exact
twins, covering the mechanical transform at each nesting depth:

.. list-table::
   :widths: 56 44
   :header-rows: 1

   * - Config key
     - Environment twin
   * - ``wits.hooks.disable``
     - ``WITS_HOOKS_DISABLE``
   * - ``wits.hooks.pre-commit.disable``
     - ``WITS_HOOKS_PRE_COMMIT_DISABLE``
   * - ``wits.hooks.pre-commit.format-python-disable``
     - ``WITS_HOOKS_PRE_COMMIT_FORMAT_PYTHON_DISABLE``
   * - ``wits.hooks.protected-branch``
     - ``WITS_HOOKS_PROTECTED_BRANCH``
   * - ``wits.hooks.prepare-commit-msg.issue-tracker-enable``
     - ``WITS_HOOKS_PREPARE_COMMIT_MSG_ISSUE_TRACKER_ENABLE``

Rule: upper-case everything, replace every ``-`` and ``.`` with ``_``. When you
need the twin of an unfamiliar key, apply that rule rather than looking it up —
it is guaranteed.
